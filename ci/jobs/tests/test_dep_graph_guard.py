#!/usr/bin/env python3
"""依赖图守卫自测：环 / 分层方向 / 孤儿基线 / 干净图（无网络、无 cargo）。"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
GUARD = ROOT / "scripts" / "dep_graph_guard.py"


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
            return subprocess.run(
                [sys.executable, str(GUARD), "--metadata", str(f), *extra],
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


if __name__ == "__main__":
    unittest.main()
