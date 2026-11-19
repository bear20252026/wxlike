"""单一权威进度源（第四轮审计 N5）：快照必须与代码事实一致，漂移即红。

与 test_dep_graph_guard.py 同款：真仓库必须绿 + 注入漂移必须红。
"""

from __future__ import annotations

import importlib.util
import json
import pathlib
import subprocess
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts" / "progress_snapshot.py"
SNAPSHOT = ROOT / "docs/full-platform-delivery/progress-snapshot.json"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SNAP = load("progress_snapshot", SCRIPT)


class ProgressSnapshotTest(unittest.TestCase):
    def test_real_snapshot_is_in_sync(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--check"],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_snapshot_exists_and_has_date(self):
        data = json.loads(SNAPSHOT.read_text(encoding="utf-8"))
        self.assertEqual(data["schema"], 1)
        self.assertRegex(data["verified_at"], r"^\d{4}-\d{2}-\d{2}$")
        self.assertEqual(data["card_count"], len(data["cards"]))

    def test_card_ids_bijection_with_teams(self):
        snapshot, drift = SNAP.build_snapshot()
        self.assertEqual(drift, [], "tasks.json 与 teams 卡不是双射")

    def test_g06_wired_and_payment_callback_unblocked(self):
        # 第四轮审计 N7：支付回调已挂载，commerce/ledger 接线，G06 不再是孤岛，
        # PAYMENT-CALLBACK-REPLAY 已关闭（真实进程取证）。
        data = json.loads(SNAPSHOT.read_text(encoding="utf-8"))
        g06 = [c for c in data["cards"] if c["group"] == "G06"]
        self.assertTrue(all(c["status"] == "PARTIAL" for c in g06))
        self.assertNotIn("PAYMENT-CALLBACK-REPLAY", data["release_blocking_gaps"])
        self.assertEqual(data["release_blocking_gaps"], [])

    def test_g02_09_domain_implemented_pending_integration(self):
        # 2026-11-19：治理域已实现（identity/relations governance + 38 测试），
        # HTTP 面/平台联动待 G00 集成 → PARTIAL，不再是 MISSING。
        data = json.loads(SNAPSHOT.read_text(encoding="utf-8"))
        card = next(c for c in data["cards"] if c["id"] == "G02-09")
        self.assertEqual(card["status"], "PARTIAL")
        self.assertIn("G00", card["evidence"])

    def test_github_is_side_evidence_not_authority(self):
        # issue 闭 ≠ 验收过：快照的 status 不由 github_issue_state 决定。
        data = json.loads(SNAPSHOT.read_text(encoding="utf-8"))
        self.assertIn("旁证", data["authority"])
        # 存在 issue 已闭但完成度非 LANDED 的卡（证明二者解耦）。
        closed_not_landed = [
            c for c in data["cards"]
            if c.get("github_issue_state") == "CLOSED" and c["status"] != "LANDED"
        ]
        self.assertTrue(closed_not_landed, "GitHub 开闭与完成度应解耦")

    def test_injected_drift_goes_red(self):
        # 临时改快照里一张卡的状态 → --check 必须红；改回必须绿。
        original = SNAPSHOT.read_text(encoding="utf-8")
        data = json.loads(original)
        target = data["cards"][0]
        old = target["status"]
        target["status"] = "LANDED" if old != "LANDED" else "MISSING"
        try:
            SNAPSHOT.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            red = subprocess.run(
                [sys.executable, str(SCRIPT), "--check"],
                capture_output=True, text=True,
            )
            self.assertNotEqual(red.returncode, 0, "注入的完成度漂移竟然没报错")
            self.assertIn("漂移", red.stderr)
        finally:
            SNAPSHOT.write_text(original, encoding="utf-8")
        green = subprocess.run(
            [sys.executable, str(SCRIPT), "--check"],
            capture_output=True, text=True,
        )
        self.assertEqual(green.returncode, 0, green.stderr)


if __name__ == "__main__":
    unittest.main()