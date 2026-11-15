#!/usr/bin/env python3
"""契约发射器自测：Swift/Kotlin 与 TS/Dart 四端同源（无网络、无编译器）。

D5 给 codegen 加了 Swift/Kotlin 发射器。这里不编译产物（Windows CI 上没有
swiftc/kotlinc），而是把"四端必须同源"这条纪律变成可执行断言：

1. 四个发射器登记的 action id / capability 集合完全一致（都来自 parse_catalog）；
2. 五份共享类型（Money/DomainError/ActionRequest/ActionResult/EventEnvelope）
   在四端都出现；
3. 未知 status 安全失败：四端解码器都只认 ok/empty/failed，且走 null/nil 返回；
4. manifest 登记了 swift/kotlin 产物；
5. 生成是确定性的，且与已入库产物逐字节一致（--check 的语义）。
"""

from __future__ import annotations

import importlib.util
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
GENERATOR = ROOT / "tools" / "codegen" / "generate.py"

_spec = importlib.util.spec_from_file_location("wxlike_codegen", GENERATOR)
gen = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gen)


_ID = re.compile(r"""['"]([a-z][a-z0-9_]*\.[a-z][a-z0-9_.]*)['"]""")


def _action_ids(text: str, start: str, end: str) -> set[str]:
    """只从 action 表块里抽 id（兼容单/双引号）。

    不能全文扫 `域.动作` 形状：schema 描述里的 `object.access` 之类会被误当
    action id。各语言的表块边界不同，分别截取后再抽。
    """
    begin = text.index(start)
    finish = text.index(end, begin)
    return set(_ID.findall(text[begin:finish]))


class CodegenEmittersTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.schemas = gen.load_schemas()
        cls.digest = gen.digest_of(cls.schemas)
        cls.actions = gen.parse_catalog()
        cls.ts = gen.emit_ts(cls.schemas, cls.digest, cls.actions)
        cls.dart = gen.emit_dart(cls.schemas, cls.digest, cls.actions)
        cls.swift = gen.emit_swift(cls.schemas, cls.digest, cls.actions)
        cls.kotlin = gen.emit_kotlin(cls.schemas, cls.digest, cls.actions)
        cls.catalog_ids = {a["id"] for a in cls.actions}

    def test_four_emitters_share_action_ids(self):
        blocks = (
            ("ts", self.ts, "export const ACTIONS = [", "] as const;"),
            ("dart", self.dart, "const Map<String, String> actionCapabilities = {", "};"),
            ("swift", self.swift, "public let actionCapabilities: [String: String] = [", "\n]"),
            ("kotlin", self.kotlin, "val actionCapabilities: Map<String, String> = mapOf(", "\n)"),
        )
        for name, text, start, end in blocks:
            self.assertEqual(
                _action_ids(text, start, end),
                self.catalog_ids,
                f"{name} 发射的 action id 集合与命令目录不同源",
            )

    def test_capabilities_match_catalog(self):
        # 每条 action 的 capability 在 Swift/Kotlin 里都要与目录一致。
        for action in self.actions:
            pair = f'"{action["id"]}": "{action["capability"]}"'
            self.assertIn(pair, self.swift, f"swift 缺 {pair}")
            kt_pair = f'"{action["id"]}" to "{action["capability"]}"'
            self.assertIn(kt_pair, self.kotlin, f"kotlin 缺 {kt_pair}")

    def test_shared_types_present_in_all_languages(self):
        types = ("Money", "DomainError", "ActionRequest", "ActionResult", "EventEnvelope")
        for name, text in (
            ("ts", self.ts),
            ("dart", self.dart),
            ("swift", self.swift),
            ("kotlin", self.kotlin),
        ):
            for type_name in types:
                self.assertIn(type_name, text, f"{name} 缺共享类型 {type_name}")

    def test_safe_failure_decoder_in_all_languages(self):
        # 四端解码器都只认 ok/empty/failed，未知 status 走 null/nil（不抛）。
        for name, text, nullish in (
            ("ts", self.ts, "null"),
            ("dart", self.dart, "null"),
            ("swift", self.swift, "nil"),
            ("kotlin", self.kotlin, "null"),
        ):
            self.assertIn("decodeActionResult", text, f"{name} 缺解码器")
            for status in ("ok", "empty", "failed"):
                self.assertTrue(
                    f'"{status}"' in text or f"'{status}'" in text,
                    f"{name} 解码器缺 status {status}",
                )
            self.assertIn(nullish, text, f"{name} 解码器未走 {nullish} 安全失败")

    def test_digest_and_version_headers(self):
        for name, text in (
            ("swift", self.swift),
            ("kotlin", self.kotlin),
        ):
            self.assertIn(self.digest, text, f"{name} 头部缺摘要")
            self.assertIn(gen.SCHEMA_VERSION, text, f"{name} 缺 schema 版本")
            self.assertIn(gen.CONTRACT_VERSION, text, f"{name} 缺 contract 版本")

    def test_manifest_registers_new_artifacts(self):
        manifest = gen.emit_manifest(
            self.digest,
            [
                "ts/contract.ts",
                "dart/contract.dart",
                "swift/Contract.swift",
                "kotlin/Contract.kt",
                "actions.json",
            ],
        )
        self.assertIn("swift/Contract.swift", manifest)
        self.assertIn("kotlin/Contract.kt", manifest)

    def test_emission_is_deterministic(self):
        again_swift = gen.emit_swift(self.schemas, self.digest, self.actions)
        again_kotlin = gen.emit_kotlin(self.schemas, self.digest, self.actions)
        self.assertEqual(again_swift, self.swift)
        self.assertEqual(again_kotlin, self.kotlin)

    def test_checked_in_artifacts_match_generator(self):
        # 与 --check 同语义：已入库产物必须逐字节等于生成结果。
        outputs = gen.build_outputs()
        for path, text in outputs.items():
            self.assertTrue(path.is_file(), f"产物缺失：{path}")
            self.assertEqual(
                path.read_bytes(),
                text.encode("utf-8"),
                f"产物与契约不一致（重跑生成器）：{path}",
            )


if __name__ == "__main__":
    unittest.main()