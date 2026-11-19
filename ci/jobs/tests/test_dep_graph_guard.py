#!/usr/bin/env python3
"""依赖图守卫自测：环 / 分层方向 / 孤儿基线 / 干净图（无网络、无 cargo）。"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
GUARD = ROOT / "scripts" / "dep_graph_guard.py"


def load_guard():
    spec = importlib.util.spec_from_file_location("dep_graph_guard", GUARD)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def pkg(name: str, deps: list[str]) -> dict:
    return {
        "id": f"path+file:///repo#{name}",
        "name": name,
        "dependencies": [{"name": d, "kind": None} for d in deps],
    }


def metadata(packages: list[dict]) -> dict:
    return {
        "workspace_members": [p["id"] for p in packages],
        "packages": packages,
    }


class DepGraphGuardTest(unittest.TestCase):
    def run_guard(self, meta: dict, *extra: str) -> subprocess.CompletedProcess:
        with tempfile.TemporaryDirectory() as td:
            f = Path(td) / "meta.json"
            f.write_text(json.dumps(meta), encoding="utf-8")
            # 合成图自测用空台账隔离真实 SEAM 基线（否则真实条目会被判陈旧）。
            empty = Path(td) / "wiring.json"
            empty.write_text('{"seams": []}', encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(GUARD), "--metadata", str(f),
                 "--wiring-baseline", str(empty), *extra],
                capture_output=True, text=True,
            )

    def test_clean_graph_passes(self):
        # server -> platform/net，platform 不依赖 net：合法。
        meta = metadata([
            pkg("wxlike-server", ["wxlike-platform", "wxlike-net"]),
            pkg("wxlike-platform", []),
            pkg("wxlike-net", []),
        ])
        r = self.run_guard(meta)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_cycle_is_blocked(self):
        meta = metadata([
            pkg("wxlike-a", ["wxlike-b"]),
            pkg("wxlike-b", ["wxlike-a"]),
        ])
        r = self.run_guard(meta)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("cycle", r.stdout)

    def test_forbidden_layering_edge_is_blocked(self):
        meta = metadata([
            pkg("wxlike-platform", ["wxlike-net"]),
            pkg("wxlike-net", []),
        ])
        r = self.run_guard(meta)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("layering", r.stdout)

    def test_dev_dependency_does_not_count_as_layering(self):
        # net 以 dev-dep 驱动 platform 打真 hub 是合法的，不算越层。
        meta = metadata([
            pkg("wxlike-server", ["wxlike-platform", "wxlike-net"]),
            pkg("wxlike-platform", []),
            {
                "id": "path+file:///repo#wxlike-net",
                "name": "wxlike-net",
                "dependencies": [{"name": "wxlike-platform", "kind": "dev"}],
            },
        ])
        r = self.run_guard(meta)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_new_orphan_is_blocked(self):
        # wxlike-zzz 无生产入边、不在基线 → 新孤儿违规。
        meta = metadata([
            pkg("wxlike-server", ["wxlike-platform"]),
            pkg("wxlike-platform", []),
            pkg("wxlike-zzz", []),
        ])
        r = self.run_guard(meta)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("orphan", r.stdout)

    def test_root_crate_is_not_orphan(self):
        # server 是组合根，无入边但不算孤儿。
        meta = metadata([
            pkg("wxlike-server", ["wxlike-platform"]),
            pkg("wxlike-platform", []),
        ])
        r = self.run_guard(meta)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)


class WiringGuardTest(unittest.TestCase):
    """规则 4：测而未接的孤岛必须显式登记 SEAM（第三轮审计 2026-11-17）。"""

    @classmethod
    def setUpClass(cls):
        cls.guard = load_guard()

    def seam(self, crate: str, **over) -> dict:
        entry = {
            "crate": crate,
            "owner": "G06",
            "task": "G06 integration",
            "close_when": "server routes dispatch into the crate",
            "evidence": "tests exist, zero production dependents",
        }
        entry.update(over)
        return entry

    def test_unledgered_island_is_blocked(self):
        findings = self.guard.check_wiring({"wxlike-commerce": 69}, {})
        self.assertTrue(any("unwired" in f and "wxlike-commerce" in f for f in findings), findings)

    def test_complete_seam_passes(self):
        ledger = {"wxlike-commerce": self.seam("wxlike-commerce")}
        self.assertEqual(self.guard.check_wiring({"wxlike-commerce": 69}, ledger), [])

    def test_seam_missing_required_field_is_blocked(self):
        for field in ("owner", "task", "close_when", "evidence"):
            ledger = {"wxlike-commerce": self.seam("wxlike-commerce", **{field: "  "})}
            findings = self.guard.check_wiring({"wxlike-commerce": 69}, ledger)
            self.assertTrue(any(field in f for f in findings), (field, findings))

    def test_stale_seam_for_wired_crate_is_blocked(self):
        # 已接线（不在孤岛集）却仍登记 → 陈旧条目，基线只许收紧。
        ledger = {"wxlike-commerce": self.seam("wxlike-commerce")}
        findings = self.guard.check_wiring({}, ledger)
        self.assertTrue(any("wxlike-commerce" in f for f in findings), findings)

    def test_island_chain_does_not_count_as_wired(self):
        # translation 只被 vision 依赖、vision 只被 translation 依赖：
        # 两个都不在任何入口闭包内 → 都是孤岛。
        adj = {
            "wxlike-server": set(),
            "wxlike-translation": {"wxlike-vision"},
            "wxlike-vision": set(),
        }
        wired = self.guard.reachable_from({"wxlike-server"}, adj)
        self.assertEqual(wired, {"wxlike-server"})

    def test_entry_closure_covers_transitive_deps(self):
        adj = {
            "wxlike-server": {"wxlike-platform"},
            "wxlike-platform": {"wxlike-core"},
            "wxlike-core": set(),
            "wxlike-commerce": set(),
        }
        wired = self.guard.reachable_from({"wxlike-server"}, adj)
        self.assertEqual(wired, {"wxlike-server", "wxlike-platform", "wxlike-core"})

    def test_test_counter_finds_inline_and_integration_tests(self):
        with tempfile.TemporaryDirectory() as td:
            d = Path(td)
            (d / "src").mkdir()
            (d / "tests").mkdir()
            (d / "src" / "lib.rs").write_text(
                "#[cfg(test)]\nmod t {\n#[test]\nfn a() {}\n#[tokio::test]\nasync fn b() {}\n}\n",
                encoding="utf-8",
            )
            (d / "tests" / "it.rs").write_text("#[test]\nfn c() {}\n", encoding="utf-8")
            self.assertEqual(self.guard.count_tests(d), 3)

    def test_synthetic_metadata_without_dirs_is_not_island(self):
        # 自测用的合成 metadata 没有真实 crate 目录：无测试可数，不判孤岛。
        names = {"wxlike-server", "wxlike-zzz"}
        adj = {"wxlike-server": set(), "wxlike-zzz": set()}
        self.assertEqual(self.guard.find_unwired(names, adj, {}), {})


class RealRepoWiringTest(unittest.TestCase):
    """真实仓库：守卫必须绿，且台账与已知孤岛事实一致。"""

    def test_guard_passes_on_real_repo(self):
        r = subprocess.run(
            [sys.executable, str(GUARD)], capture_output=True, text=True, cwd=ROOT
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("SEAM", r.stdout)

    def test_ledger_pins_known_islands(self):
        data = json.loads((ROOT / "scripts" / "wiring-baseline.json").read_text(encoding="utf-8"))
        crates = {e["crate"] for e in data["seams"]}
        # 第三轮审计实证的孤岛必须全部在台账里。commerce/ledger 已于第四轮审计 N7
        # 经 server/src/commerce_mount.rs 接线（支付回调挂载）；miniapps/container/
        # dpage 已经 server/src/mini_runtime.rs 真实接线（审核目录搜索/推荐、包体
        # 检视、根信任验签、容器沙箱启动）。按"基线只许收紧"移出台账——这里钉死
        # 它们**不再**是孤岛，防止有人把它们悄悄加回去。
        for island in (
            "wxlike-discovery",
            "wxlike-vision", "wxlike-translation",
            "wxlike-client-web",
        ):
            self.assertIn(island, crates)
        for wired in (
            "wxlike-commerce", "wxlike-ledger",
            "wxlike-miniapps", "wxlike-container", "wxlike-dpage",
        ):
            self.assertNotIn(wired, crates, f"{wired} 已接线，不应再登记为孤岛")
        for entry in data["seams"]:
            for field in ("owner", "task", "close_when", "evidence"):
                self.assertTrue(str(entry.get(field, "")).strip(), (entry["crate"], field))


if __name__ == "__main__":
    unittest.main()
