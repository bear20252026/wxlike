"""漏洞门禁用例：回退即红、RSC 不误报、例外到期、断网与取消。

其中 `test_rolled_back_version_blocks` 是验收要求里点名的那条：
"刻意回退漏洞版本导致 CI 失败"。
"""

from __future__ import annotations

import sys
import unittest
import urllib.error
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from fixture import FakeRepo  # noqa: E402
from sourceaudit import advisories  # noqa: E402
from sourceaudit.model import Status  # noqa: E402
from sourceaudit.util import DataError  # noqa: E402

DATASET = {
    "schema": "wxlike.advisories.v1",
    "advisories": [
        {
            "id": "RUSTSEC-TEST-1",
            "component": "webpki-roots",
            "ecosystem": "crates.io",
            "severity": "high",
            "affected": [">=1.0.0, <1.0.10"],
            "fixed": ["1.0.10"],
        },
        {
            "id": "GHSA-TEST-RSC",
            "component": "@vitejs/plugin-rsc",
            "ecosystem": "npm",
            "severity": "critical",
            "affected": ["<0.5.38"],
            "fixed": ["0.5.38"],
        },
        {
            "id": "RUSTSEC-TEST-WASM",
            "component": "wasmtime",
            "ecosystem": "crates.io",
            "severity": "critical",
            "affected": ["<36.0.19", ">=37, <48.0.7", ">=49, <49.0.4"],
            "fixed": ["49.0.4"],
        },
        {
            "id": "RUSTSEC-TEST-OLD",
            "component": "serde",
            "ecosystem": "crates.io",
            "severity": "low",
            "affected": ["<1.0.0"],
            "fixed": ["1.0.0"],
        },
    ],
}


class AdvisoriesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FakeRepo()
        self.write_policy()
        self.write_dataset(DATASET)

    def tearDown(self) -> None:
        self.repo.cleanup()

    # ---- 夹具 -------------------------------------------------------------
    def write_dataset(self, data: dict) -> None:
        self.repo.write_json("security/advisory-policy/advisories.json", data)

    def write_policy(self, exceptions: list | None = None, reachability: dict | None = None) -> None:
        self.repo.write_json(
            "security/advisory-policy/policy.json",
            {
                "schema": "wxlike.advisory-policy.v1",
                "gate": {"block_severities": ["critical", "high"]},
                "reachability": reachability or {},
                "exceptions": exceptions or [],
            },
        )

    def write_lock(self, packages: list[tuple[str, str]]) -> None:
        body = "version = 3\n"
        for name, version in packages:
            body += f'\n[[package]]\nname = "{name}"\nversion = "{version}"\n'
        (self.repo.root / "Cargo.lock").write_text(body, encoding="utf-8")

    def codes(self) -> list[str]:
        report = advisories.check(self.repo.root, offline=True)
        return [f.code for f in report.findings]

    # ---- 用例 -------------------------------------------------------------
    def test_rsc_in_current_frontend_locations_is_not_missed(self) -> None:
        for path in ("apps/desktop-web/package-lock.json", "design/ui-v2/prototype/package-lock.json"):
            with self.subTest(path=path):
                self.repo.write_json(path, {"packages": {"node_modules/@vitejs/plugin-rsc": {"version": "0.4.0"}}})
                report = advisories.check(self.repo.root, offline=True)
                self.assertTrue(any(f.subject.startswith("@vitejs/plugin-rsc@") for f in report.blocking))
                self.repo.rm(path)

    def test_known_fixed_version_passes(self) -> None:
        self.write_lock([("webpki-roots", "1.0.10")])
        self.assertEqual([f for f in advisories.check(self.repo.root, offline=True).blocking], [])

    def test_rolled_back_version_blocks(self) -> None:
        """验收原话：刻意回退漏洞版本必须让门禁失败。"""
        self.write_lock([("webpki-roots", "1.0.9")])
        report = advisories.check(self.repo.root, offline=True)
        self.assertEqual([f.code for f in report.blocking], ["ADV-CANDIDATE"])
        self.assertIn("webpki-roots@1.0.9", report.blocking[0].subject)

    def test_react_spa_without_rsc_is_not_flagged(self) -> None:
        """没有安装 plugin-rsc 的 React SPA 不得因为 React 版本被判受影响。"""
        self.repo.write_json(
            "ui/package-lock.json",
            {"lockfileVersion": 3, "packages": {"node_modules/react": {"version": "18.3.1"}}},
        )
        self.write_lock([("serde", "1.0.200")])
        codes = self.codes()
        self.assertIn("ADV-NOT_APPLICABLE", codes)
        self.assertNotIn("ADV-CANDIDATE", codes)

    def test_installed_rsc_blocks(self) -> None:
        self.repo.write_json(
            "ui/package-lock.json",
            {"lockfileVersion": 3, "packages": {"node_modules/@vitejs/plugin-rsc": {"version": "0.5.0"}}},
        )
        self.write_lock([("serde", "1.0.200")])
        report = advisories.check(self.repo.root, offline=True)
        self.assertIn("ADV-CANDIDATE", [f.code for f in report.blocking])

    def test_wasmtime_intersection(self) -> None:
        self.write_lock([("wasmtime", "49.0.0")])
        self.assertIn("ADV-CANDIDATE", self.codes())
        self.write_lock([("wasmtime", "49.0.4")])
        report = advisories.check(self.repo.root, offline=True)
        self.assertEqual(report.blocking, [], "49.0.4 同时满足两条 Critical 的修复条件")

    def test_expired_exception_blocks(self) -> None:
        self.write_lock([("webpki-roots", "1.0.9")])
        self.write_policy(
            exceptions=[
                {"id": "RUSTSEC-TEST-1", "owner": "G00", "issue": "#1", "expires": "2020-01-01", "reason": "旧"}
            ]
        )
        codes = [f.code for f in advisories.check(self.repo.root, offline=True).blocking]
        self.assertIn("EXC-EXPIRED", codes)

    def test_live_exception_is_reported_as_exempt(self) -> None:
        self.write_lock([("webpki-roots", "1.0.9")])
        self.write_policy(
            exceptions=[
                {"id": "RUSTSEC-TEST-1", "owner": "G00", "issue": "#1", "expires": "2099-01-01", "reason": "等升级"}
            ]
        )
        report = advisories.check(self.repo.root, offline=True)
        self.assertEqual(report.blocking, [])
        self.assertEqual([f.code for f in report.of(Status.EXEMPT)], ["ADV-EXEMPT"])

    def test_exception_without_owner_blocks(self) -> None:
        self.write_lock([("webpki-roots", "1.0.9")])
        self.write_policy(exceptions=[{"id": "RUSTSEC-TEST-1", "expires": "2099-01-01"}])
        self.assertIn("ADV-EXC", [f.code for f in advisories.check(self.repo.root, offline=True).blocking])

    def test_surface_is_named_in_message(self) -> None:
        """必须说清是哪一份锁 —— 桌面与根是两份独立锁。"""
        (self.repo.root / "ui/src-tauri").mkdir(parents=True, exist_ok=True)
        (self.repo.root / "ui/src-tauri/Cargo.lock").write_text(
            'version = 3\n\n[[package]]\nname = "webpki-roots"\nversion = "1.0.9"\n', encoding="utf-8"
        )
        self.write_lock([("webpki-roots", "1.0.10")])
        report = advisories.check(self.repo.root, offline=True)
        blocked = report.blocking
        self.assertEqual(len(blocked), 1)
        self.assertIn("desktop", blocked[0].message)

    def test_cargo_audit_json_input_is_accepted(self) -> None:
        self.repo.write_json(
            "audit.json",
            {
                "vulnerabilities": {
                    "list": [
                        {
                            "advisory": {"id": "RUSTSEC-2024-0001", "severity": "high", "url": "https://x"},
                            "package": {"name": "serde", "version": "1.0.200"},
                        }
                    ]
                }
            },
        )
        report = advisories.check(self.repo.root, offline=True, source=self.repo.root / "audit.json")
        self.assertIn("ADV-CANDIDATE", [f.code for f in report.blocking])

    def test_empty_dataset_warns(self) -> None:
        self.write_dataset({"schema": "wxlike.advisories.v1", "advisories": []})
        self.assertIn("ADV-EMPTY", self.codes())

    def test_bad_schema_is_data_error(self) -> None:
        self.write_dataset({"schema": "wrong", "advisories": []})
        with self.assertRaises(DataError):
            advisories.check(self.repo.root, offline=True)

    def test_offline_never_touches_network(self) -> None:
        self.write_lock([("serde", "1.0.200")])
        original = advisories.urllib.request.urlopen

        def boom(*_a, **_k):
            raise AssertionError("--offline 不允许联网")

        advisories.urllib.request.urlopen = boom
        try:
            advisories.check(self.repo.root, offline=True)
        finally:
            advisories.urllib.request.urlopen = original

    def test_network_failure_is_data_error_not_pass(self) -> None:
        self.write_lock([("serde", "1.0.200")])
        original = advisories.urllib.request.urlopen

        def boom(*_a, **_k):
            raise urllib.error.URLError("timed out")

        advisories.urllib.request.urlopen = boom
        try:
            with self.assertRaises(DataError):
                advisories.check(self.repo.root, offline=False, timeout=0.01)
        finally:
            advisories.urllib.request.urlopen = original


class VersionRangeTest(unittest.TestCase):
    def test_range_matrix(self) -> None:
        cases = [
            ("1.0.9", ">=1.0.0, <1.0.10", True),
            ("1.0.10", ">=1.0.0, <1.0.10", False),
            ("49.0.0", ">=49, <49.0.4", True),
            ("49.0.4", ">=49, <49.0.4", False),
            ("0.103.16", "<0.103.16", False),
            ("0.103.15", "<0.103.16", True),
            ("4.2.3", ">=4.2, <4.2.4", True),
            ("4.1.7", "<4.1.8", True),
            ("4.1.8", "<4.1.8", False),
        ]
        for version, spec, expected in cases:
            self.assertEqual(advisories._in_range(version, spec), expected, f"{version} in {spec}")

    def test_prerelease_sorts_below_release(self) -> None:
        self.assertLess(advisories._cmp("0.104.0-alpha.1", "0.104.0"), 0)

    def test_bad_spec_is_data_error(self) -> None:
        with self.assertRaises(DataError):
            advisories._in_range("1.0.0", "约等于 1")


if __name__ == "__main__":
    unittest.main()
