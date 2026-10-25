"""CLI 契约用例：退出码、JSON 输出、取消、参数错误。

退出码是 CI 唯一读得懂的东西，所以它必须被测试，而不是靠"应该没问题"。
"""

from __future__ import annotations

import io
import json
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from fixture import FakeRepo  # noqa: E402
from sourceaudit import cli  # noqa: E402
from sourceaudit.util import EXIT_BLOCK, EXIT_DATA, EXIT_OK  # noqa: E402


class CliTest(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FakeRepo()

    def tearDown(self) -> None:
        self.repo.cleanup()

    def run_cli(self, *args: str) -> tuple[int, str]:
        buf = io.StringIO()
        with redirect_stdout(buf):
            code = cli.main([*args, "--root", str(self.repo.root)])
        return code, buf.getvalue()

    def test_pass_exit_code(self) -> None:
        code, out = self.run_cli("provenance", "check")
        self.assertEqual(code, EXIT_OK, out)

    def test_broken_json_is_data_error(self) -> None:
        (self.repo.root / "third_party/reviews/registry.json").write_text("{ broken", encoding="utf-8")
        code, _ = self.run_cli("provenance", "check")
        self.assertEqual(code, EXIT_DATA, "坏数据应当是 4（工具跑不动），不是 3（检查没通过）")

    def test_blocked_policy_is_exit_3(self) -> None:
        reg = self.repo.load_json("third_party/reviews/registry.json")
        reg["upstreams"][0]["commit"] = "deadbeef"
        self.repo.write_json("third_party/reviews/registry.json", reg)
        code, _ = self.run_cli("provenance", "check")
        self.assertEqual(code, EXIT_BLOCK)

    def test_json_output_is_parseable(self) -> None:
        code, out = self.run_cli("provenance", "check", "--json")
        self.assertEqual(code, EXIT_OK)
        payload = json.loads(out)
        self.assertEqual(payload["gate"], "provenance")
        self.assertIn("counts", payload)

    def test_strict_promotes_warnings(self) -> None:
        reg = self.repo.load_json("third_party/reviews/registry.json")
        reg["files"] = []
        self.repo.write_json("third_party/reviews/registry.json", reg)
        lenient, _ = self.run_cli("provenance", "check")
        strict, _ = self.run_cli("provenance", "check", "--strict")
        self.assertEqual(lenient, EXIT_OK, "空表只有 warn，默认不阻断")
        self.assertEqual(strict, EXIT_BLOCK, "--strict 下 warn 必须阻断")

    def test_unknown_gate_is_usage_error(self) -> None:
        with self.assertRaises(SystemExit) as ctx:
            cli.main(["nonsense"])
        self.assertEqual(ctx.exception.code, 2)

    def test_root_outside_any_repo_is_usage_error(self) -> None:
        """错的 --root 必须立刻报错：在空目录上跑出"全绿"是最坏的失败形态。"""
        buf = io.StringIO()
        with redirect_stdout(buf):
            code = cli.main(["notice", "check", "--root", "C:/Windows"])
        self.assertEqual(code, 2)

    def test_cancel_maps_to_130(self) -> None:
        original = cli._dispatch

        def boom(*_a, **_k):
            raise KeyboardInterrupt

        cli._dispatch = boom
        try:
            code, _ = self.run_cli("provenance", "check")
        finally:
            cli._dispatch = original
        self.assertEqual(code, 130, "被取消必须与「不通过」区分开")

    def test_sbom_generate_is_idempotent_via_cli(self) -> None:
        first, _ = self.run_cli("sbom", "generate")
        second, out = self.run_cli("sbom", "generate")
        self.assertEqual((first, second), (EXIT_OK, EXIT_OK))
        payload = json.loads(self.run_cli("sbom", "generate", "--json")[1])
        self.assertFalse(payload["meta"]["changed"], "重复提交不应再改文件")


if __name__ == "__main__":
    unittest.main()
