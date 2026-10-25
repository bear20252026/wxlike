"""发布评审门禁用例：pass 要真的可 pass，block 要 block 在对的地方。

最关键的两条：
* `test_clean_bundle_passes` —— 证明这道门禁不是"永远红"；
* `test_checklist_cannot_pass` —— 证明自报清单不能冒充验证。
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from fixture import FakeRepo  # noqa: E402
from sourceaudit import release  # noqa: E402
from sourceaudit.util import DataError  # noqa: E402


def clean_bundle() -> dict:
    return {
        "schema": "wxlike.release-evidence.v1",
        "release": {
            "name": "test",
            "reviewed_at": "2026-10-24",
            "reviewer": "G17",
            "commit": "abcdef0",
        },
        "claims": {"platforms": ["linux"]},
        "platforms": [{"name": "linux", "verified": True, "evidence": "ci.yml test-linux"}],
        "items": [
            {"id": "E-TEST", "required": True, "kind": "test", "status": "pass", "evidence": "cargo test"}
        ],
        "exceptions": [],
    }


class ReleaseTest(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FakeRepo()
        self._write_blockers([])
        (self.repo.root / "tests/security/corpus").mkdir(parents=True, exist_ok=True)

    def tearDown(self) -> None:
        self.repo.cleanup()

    def _write_bundle(self, bundle: dict) -> Path:
        path = self.repo.root / "security/release-reviews/evidence/latest.json"
        self.repo.write_json("security/release-reviews/evidence/latest.json", bundle)
        return path

    def _write_blockers(self, blockers: list[dict]) -> None:
        self.repo.write_json(
            "security/release-reviews/blockers.json",
            {"schema": "wxlike.release-blockers.v1", "blockers": blockers},
        )

    def codes(self) -> list[str]:
        path = self._write_bundle(clean_bundle())
        return [f.code for f in release.check(self.repo.root, evidence=path).findings]

    # ---- 通过 -------------------------------------------------------------
    def test_clean_bundle_passes(self) -> None:
        path = self._write_bundle(clean_bundle())
        report = release.check(self.repo.root, evidence=path)
        self.assertEqual(report.blocking, [], report.render_text())

    # ---- 证据强度 ---------------------------------------------------------
    def test_checklist_cannot_pass(self) -> None:
        bundle = clean_bundle()
        bundle["items"][0]["kind"] = "checklist"
        path = self._write_bundle(bundle)
        self.assertIn("REL-SELF-REPORT", [f.code for f in release.check(self.repo.root, evidence=path).blocking])

    def test_pass_without_evidence_blocks(self) -> None:
        bundle = clean_bundle()
        bundle["items"][0]["evidence"] = ""
        path = self._write_bundle(bundle)
        self.assertIn("REL-NO-EVIDENCE", [f.code for f in release.check(self.repo.root, evidence=path).blocking])

    def test_required_blocked_blocks(self) -> None:
        bundle = clean_bundle()
        bundle["items"][0].update({"status": "blocked", "owner": "G08", "issue": "#1", "notes": "待修"})
        path = self._write_bundle(bundle)
        report = release.check(self.repo.root, evidence=path)
        self.assertIn("REL-REQUIRED-FAIL", [f.code for f in report.blocking])
        self.assertEqual(report.blocking[0].owner, "G08", "报告里必须能看到 owner")

    def test_optional_item_only_warns(self) -> None:
        bundle = clean_bundle()
        bundle["items"][0]["required"] = False
        bundle["items"][0]["status"] = "unverified"
        path = self._write_bundle(bundle)
        report = release.check(self.repo.root, evidence=path)
        self.assertEqual(report.blocking, [])
        self.assertIn("REL-OPTIONAL-FAIL", [f.code for f in report.findings])

    def test_not_applicable_needs_reason(self) -> None:
        bundle = clean_bundle()
        bundle["items"][0]["status"] = "not_applicable"
        path = self._write_bundle(bundle)
        self.assertIn("REL-NA-REASON", [f.code for f in release.check(self.repo.root, evidence=path).blocking])

    # ---- 声明与平台 -------------------------------------------------------
    def test_claim_must_be_verified(self) -> None:
        bundle = clean_bundle()
        bundle["claims"]["platforms"] = ["linux", "ios"]
        path = self._write_bundle(bundle)
        self.assertIn("REL-CLAIM-UNVERIFIED", [f.code for f in release.check(self.repo.root, evidence=path).blocking])

    def test_verified_needs_evidence(self) -> None:
        bundle = clean_bundle()
        bundle["platforms"][0]["evidence"] = ""
        path = self._write_bundle(bundle)
        self.assertIn("REL-PLATFORM-EVIDENCE", [f.code for f in release.check(self.repo.root, evidence=path).blocking])

    # ---- 阻塞项 -----------------------------------------------------------
    def test_open_blocker_blocks_and_needs_owner(self) -> None:
        self._write_blockers([{"id": "B1", "title": "x", "severity": "high", "status": "open"}])
        path = self._write_bundle(clean_bundle())
        codes = [f.code for f in release.check(self.repo.root, evidence=path).blocking]
        self.assertIn("REL-BLOCKER-UNTRACKED", codes)
        self.assertIn("REL-BLOCKER-OPEN", codes)

    def test_external_blocker_still_blocks(self) -> None:
        self._write_blockers(
            [{"id": "B1", "title": "无真机证据", "severity": "high", "owner": "G16", "issue": "#97", "external": True}]
        )
        path = self._write_bundle(clean_bundle())
        codes = [f.code for f in release.check(self.repo.root, evidence=path).blocking]
        self.assertIn("REL-BLOCKER-EXTERNAL", codes)

    def test_closed_blocker_without_evidence_warns(self) -> None:
        self._write_blockers([{"id": "B1", "title": "x", "severity": "high", "owner": "G08", "issue": "#1", "status": "closed"}])
        path = self._write_bundle(clean_bundle())
        report = release.check(self.repo.root, evidence=path)
        self.assertEqual(report.blocking, [])
        self.assertIn("REL-CLOSED-NO-EVIDENCE", [f.code for f in report.findings])

    # ---- 例外 -------------------------------------------------------------
    def test_valid_exception_releases(self) -> None:
        self._write_blockers([{"id": "B1", "title": "x", "severity": "high", "owner": "G08", "issue": "#1"}])
        bundle = clean_bundle()
        bundle["exceptions"] = [{"id": "B1", "owner": "G17", "issue": "#1", "expires": "2099-01-01", "reason": "已知"}]
        path = self._write_bundle(bundle)
        report = release.check(self.repo.root, evidence=path)
        self.assertEqual(report.blocking, [], report.render_text())

    def test_expired_exception_blocks(self) -> None:
        self._write_blockers([{"id": "B1", "title": "x", "severity": "high", "owner": "G08", "issue": "#1"}])
        bundle = clean_bundle()
        bundle["exceptions"] = [{"id": "B1", "owner": "G17", "issue": "#1", "expires": "2020-01-01", "reason": "旧"}]
        path = self._write_bundle(bundle)
        self.assertIn("EXC-EXPIRED", [f.code for f in release.check(self.repo.root, evidence=path).blocking])

    # ---- 语料交叉核对 -----------------------------------------------------
    def test_corpus_blocked_sample_must_be_registered(self) -> None:
        self.repo.write_json(
            "tests/security/corpus/manifest.json",
            {
                "schema": "wxlike.fuzz-corpus.v1",
                "samples": [{"file": "s03.hex", "verdict": "blocked", "blocker": "SEC-FUZZ-001", "issue": "#229"}],
            },
        )
        path = self._write_bundle(clean_bundle())
        self.assertIn("REL-CORPUS-UNTRACKED", [f.code for f in release.check(self.repo.root, evidence=path).blocking])

    def test_corpus_blocked_sample_registered(self) -> None:
        self._write_blockers([{"id": "SEC-FUZZ-001", "title": "x", "severity": "high", "owner": "G08", "issue": "#229", "status": "closed", "closed_evidence": "commit 1"}])
        self.repo.write_json(
            "tests/security/corpus/manifest.json",
            {
                "schema": "wxlike.fuzz-corpus.v1",
                "samples": [{"file": "s03.hex", "verdict": "blocked", "blocker": "SEC-FUZZ-001", "issue": "#229"}],
            },
        )
        path = self._write_bundle(clean_bundle())
        report = release.check(self.repo.root, evidence=path)
        self.assertEqual(report.blocking, [])

    # ---- 坏输入 -----------------------------------------------------------
    def test_empty_items_is_data_error(self) -> None:
        bundle = clean_bundle()
        bundle["items"] = []
        path = self._write_bundle(bundle)
        with self.assertRaises(DataError):
            release.check(self.repo.root, evidence=path)

    def test_bad_bundle_schema_is_data_error(self) -> None:
        path = self._write_bundle({"schema": "wrong"})
        with self.assertRaises(DataError):
            release.check(self.repo.root, evidence=path)

    def test_missing_bundle_is_data_error(self) -> None:
        with self.assertRaises(DataError):
            release.check(self.repo.root, evidence=self.repo.root / "nope.json")


if __name__ == "__main__":
    unittest.main()
