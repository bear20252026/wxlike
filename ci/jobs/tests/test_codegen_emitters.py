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

    def test_params_parsed_from_catalog(self):
        by_id = {a["id"]: a for a in self.actions}
        # conv.history：conv 必填 Str，before_seq/limit 可选 NonNegativeInt。
        history = by_id["conv.history"]["params"]
        self.assertEqual([p["name"] for p in history], ["conv", "before_seq", "limit"])
        self.assertEqual([p["required"] for p in history], [True, False, False])
        self.assertEqual([p["type"] for p in history], ["Str", "NonNegativeInt", "NonNegativeInt"])
        # read_no_args 命令无参数。
        self.assertEqual(by_id["me.info"]["params"], [])
        # StrList 参数（group.create 的 member_ids）。
        create = by_id["group.create"]["params"]
        member_ids = [p for p in create if p["name"] == "member_ids"][0]
        self.assertEqual(member_ids["type"], "StrList")
        self.assertFalse(member_ids["required"])
        # 参数总数与目录同源（89 条 Param 声明）。
        self.assertEqual(sum(len(a["params"]) for a in self.actions), 89)

    def test_param_notes_emitted_in_swift_kotlin_dart(self):
        # conv.history 的 conv 参数 note 是「会话 id」，三端都应带上。
        note = "会话 id"
        self.assertIn(f'note: "{note}"', self.swift)
        self.assertIn(f'"{note}"', self.kotlin)
        self.assertIn(f'"{note}"', self.dart)
        # ParamSpec 三端都有 note 字段。
        self.assertIn("public let note: String", self.swift)
        self.assertIn("val note: String", self.kotlin)
        self.assertIn("final String note;", self.dart)

    def test_action_params_cover_all_actions_in_all_languages(self):
        blocks = (
            ("ts", self.ts, "export interface ActionParams {", "\n}"),
            ("swift", self.swift, "public let actionParams: [String: [ParamSpec]] = [", "\n]"),
            ("kotlin", self.kotlin, "val actionParams: Map<String, List<ParamSpec>> = mapOf(", "\n)"),
            ("dart", self.dart, "const Map<String, List<ParamSpec>> actionParams = {", "\n};"),
        )
        for name, text, start, end in blocks:
            self.assertEqual(
                _action_ids(text, start, end),
                self.catalog_ids,
                f"{name} 的参数表未覆盖全部命令",
            )

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

    def test_str_lit_escapes_interpolation_per_language(self):
        # Dart/Kotlin 双引号串里 $ 触发插值 → 必须转义；Swift 不转义 $。
        self.assertEqual(gen._str_lit("cost $5", "dart"), "cost \\$5")
        self.assertEqual(gen._str_lit("cost $5", "kotlin"), "cost \\$5")
        self.assertEqual(gen._str_lit("cost $5", "swift"), "cost $5")
        # Swift 的 \( 插值：反斜杠先转义成 \\，\( 变 \\( 即字面量。
        self.assertEqual(gen._str_lit("a\\(b)", "swift"), "a\\\\(b)")
        # 通用转义：引号/反斜杠/换行。
        self.assertEqual(gen._str_lit('q"q', "swift"), 'q\\"q')
        self.assertEqual(gen._str_lit("a\nb", "dart"), "a\\nb")

    def test_frame_decoders_present_in_all_languages(self):
        # S2：四端都有 decodeCoreFrame，且 result 帧复用 decodeActionResult。
        for name, text in (
            ("ts", self.ts),
            ("dart", self.dart),
            ("swift", self.swift),
            ("kotlin", self.kotlin),
        ):
            self.assertIn("decodeCoreFrame", text, f"{name} 缺帧解码器")
            self.assertIn("CoreFrame", text, f"{name} 缺帧类型")
            # 三种 kind 都要处理（Dart 用单引号，其余双引号）。
            for kind in ("result", "error", "fatal"):
                self.assertTrue(
                    f'"{kind}"' in text or f"'{kind}'" in text,
                    f"{name} 帧解码器缺 kind {kind}",
                )

    def test_frame_schema_is_loaded(self):
        # frame.schema.json 进了 schema 池（CoreFrame 类型同源）。
        self.assertIn("frame.schema.json", self.schemas)
        self.assertEqual(self.schemas["frame.schema.json"]["title"], "CoreFrame")


if __name__ == "__main__":
    unittest.main()