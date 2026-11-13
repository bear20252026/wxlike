#!/usr/bin/env python3
"""契约生成器：**一次生成，三端同源**。

    python tools/codegen/generate.py            # 生成到 tools/codegen/out/
    python tools/codegen/generate.py --check    # 干净 checkout 上必须无 diff
    python tools/codegen/generate.py --update-lock   # 契约变更后更新兼容基线

产物：

    out/manifest.json     schema 版本 + 源文件摘要（生成物的"出生证明"）
    out/ts/contract.ts    TypeScript 类型 + 解码器
    out/dart/contract.dart Dart 类型 + 解码器
    out/actions.json      Action Catalog → CLI / MCP 描述

三条规则，写在最前面：

1. **生成物不手改**。改了会在下一次 `--check` 里被冲掉，并且 CI 会红。
2. **每个产物头部都带 schema 版本与摘要**。摘要对不上 = 产物比契约旧。
3. **未知 action / 未知枚举必须能安全失败**：TS 返回 `null`、Dart 返回
   `null`，调用方据此跳过——而不是抛异常把整条流打断。

依赖：只用标准库（CI 里 setup-python 之后直接能跑，不需要 pip install）。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCHEMA_DIR = ROOT / "crates" / "types" / "contract"
OUT_DIR = ROOT / "tools" / "codegen" / "out"
LOCK_FILE = ROOT / "tools" / "codegen" / "contract.lock.json"
CATALOG_DIR = ROOT / "crates" / "cmd" / "src" / "catalog"
# `mod.rs` 是装配（`CATALOG` 常量），`spec.rs` 是类型定义；只有域文件里是命令
# 数据。这是 catalog 模块自己写死的划分（见 mod.rs 的模块说明），不是猜测。
CATALOG_NON_DATA = {"mod.rs", "spec.rs"}

_COMMAND_ID = re.compile(r"^[a-z][a-z0-9_]*\.[a-z][a-z0-9_.]*$")

SCHEMA_VERSION = "contract-v1"
CONTRACT_VERSION = "1.0"
DIGEST_LEN = 16


def load_schemas() -> dict[str, dict]:
    schemas = {}
    for path in sorted(SCHEMA_DIR.glob("*.schema.json")):
        schemas[path.name] = json.loads(path.read_text(encoding="utf-8"))
    if not schemas:
        raise SystemExit(f"没有找到任何 schema：{SCHEMA_DIR}")
    return schemas


def digest_of(schemas: dict[str, dict]) -> str:
    """对所有 schema 的（文件名 + 规范化内容）做 SHA-256，取前 16 位。"""
    hasher = hashlib.sha256()
    for name in sorted(schemas):
        hasher.update(name.encode("utf-8"))
        hasher.update(
            json.dumps(schemas[name], ensure_ascii=False, sort_keys=True).encode("utf-8")
        )
    return hasher.hexdigest()[:DIGEST_LEN]


def _mask_rust(text: str) -> str:
    """把注释与字符串字面量的**内容**替换成空格（保留长度与换行）。

    生成器要在源码里找 `spec(` / `read_no_args(` 的调用点，而每个域文件的
    模块文档里就写着这两个名字（"每条命令一个 `spec(...)` 调用"）。不遮掉
    注释，就会把文档里的括号当成调用。

    保留长度是为了让遮罩后的偏移能直接拿去原串上取参数。
    """
    out = list(text)
    i, n = 0, len(text)

    def blank(index: int) -> None:
        if 0 <= index < n and out[index] != "\n":
            out[index] = " "

    while i < n:
        ch = text[i]
        if ch == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    blank(i)
                    blank(i + 1)
                    i += 2
                    continue
                if text[i] == '"':
                    break
                blank(i)
                i += 1
            i += 1
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "/":
            while i < n and text[i] != "\n":
                blank(i)
                i += 1
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "*":
            blank(i)
            blank(i + 1)
            i += 2
            while i < n:
                if text[i] == "*" and i + 1 < n and text[i + 1] == "/":
                    blank(i)
                    blank(i + 1)
                    i += 2
                    break
                blank(i)
                i += 1
            continue
        i += 1
    return "".join(out)


def _string_literals(text: str) -> list[str]:
    """按出现顺序返回 `text` 里字符串字面量的内容（处理 `\\"` 转义）。"""
    out: list[str] = []
    i, n = 0, len(text)
    while i < n:
        if text[i] != '"':
            i += 1
            continue
        i += 1
        buf: list[str] = []
        while i < n:
            ch = text[i]
            if ch == "\\" and i + 1 < n:
                buf.append(text[i + 1])
                i += 2
                continue
            if ch == '"':
                break
            buf.append(ch)
            i += 1
        out.append("".join(buf))
        i += 1
    return out


def _balanced(text: str, open_index: int) -> str:
    """返回 `text[open_index]` 处 `(` 到匹配 `)` 之间的内容（跳过字符串）。"""
    depth = 0
    i, n = open_index, len(text)
    while i < n:
        ch = text[i]
        if ch == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                    continue
                if text[i] == '"':
                    break
                i += 1
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return text[open_index + 1 : i]
        i += 1
    raise SystemExit("命令目录解析失败：括号不匹配")


def parse_catalog() -> list[dict]:
    """从 `crates/cmd/src/catalog/` 的域文件里抽出命令目录。

    为什么解析 Rust 源码而不是维护一份 JSON：目录**只能有一处定义**。
    多一份副本就一定会漂移，而漂移的表现是"CLI 说支持、执行器没实现"。

    定义形式（#329 起 catalog 按域拆成 `catalog/<域>.rs`，每条命令一次构造器
    调用）：`spec(id, summary, params, capability, …)`，以及它的简写
    `read_no_args(id, summary, scope)`（能力恒为 Read）。生成器只取前两个
    字符串字面量当 id / summary，能力取 `spec` 实参里的 `Capability::X`。
    `with_host(spec(…), HostCapability::X)` 这种包一层的照取不误——里面的
    `spec(` 也是调用点。

    两道闸，都是为了"格式变了要吵"：

    1. **一条都解析不出来就报错**。旧的 `len(ids) == len(summaries) ==
       len(capabilities)` 在 0==0==0 时也成立，于是解析器与源码脱节时会
       静默生成空列表——跑一次就把三端命令表清空。这是本函数最不能出的
       错，所以单独拦。
    2. **与 `CATALOG` 装配的条数对账**。域文件里加了一条命令却没进装配表
       （或反过来），这里会红，而不是等运行时才发现。
    """
    if not CATALOG_DIR.is_dir():
        raise SystemExit(f"找不到命令目录：{CATALOG_DIR}")
    commands: list[dict] = []
    for path in sorted(CATALOG_DIR.glob("*.rs")):
        if path.name in CATALOG_NON_DATA:
            continue
        text = path.read_text(encoding="utf-8")
        masked = _mask_rust(text)
        for match in re.finditer(r"\b(spec|read_no_args)\s*\(", masked):
            # `const fn spec(` 是定义，不是调用点
            if text[: match.start()].rstrip().endswith("fn"):
                continue
            body = _balanced(text, match.end() - 1)
            literals = _string_literals(body)
            if len(literals) < 2:
                # 注释/文档里提到的 `spec(…)` 已被 `_mask_rust` 遮成空格，
                # 不会走到这里。真走到这里 = 一个真实调用点没有字面量 id /
                # summary，那就是目录写法变了——报错，不要静默跳过。
                raise SystemExit(
                    f"命令目录解析失败：{path.name} 的 {match.group(1)}(…) "
                    "里抽不到 id / summary 字符串字面量"
                )
            command_id, summary = literals[0], literals[1]
            if not _COMMAND_ID.match(command_id):
                raise SystemExit(
                    f"命令目录解析失败：{path.name} 抽出非法命令 id {command_id!r}"
                    "（命令 id 的形状是 `域.动作`）"
                )
            if match.group(1) == "read_no_args":
                capability = "read"
            else:
                found = re.search(r"Capability::(\w+)", body)
                if not found:
                    raise SystemExit(
                        f"命令目录解析失败：{path.name} 的 spec(…) 里找不到 Capability::"
                    )
                capability = found.group(1).lower()
            commands.append(
                {"id": command_id, "summary": summary, "capability": capability}
            )

    if not commands:
        raise SystemExit(
            "命令目录解析出 0 条命令。这比生成一个空列表更危险——空列表会让"
            "三端命令表全部清空。解析器与 catalog 的定义形式不一致了，请修"
            " tools/codegen/generate.py 的 parse_catalog()。"
        )
    _reconcile_with_assembly(commands)
    return commands


def _reconcile_with_assembly(commands: list[dict]) -> None:
    """与 `catalog/mod.rs` 里 `CATALOG` 装配的条数对账。

    域文件里那条命令没进装配表（或装配表多了一条），生成物会少一条/多一条，
    而这条偏差只有在运行时才看得出来——所以在这里就拦住。
    """
    mod_rs = CATALOG_DIR / "mod.rs"
    if not mod_rs.is_file():
        raise SystemExit(f"找不到命令目录装配：{mod_rs}")
    text = mod_rs.read_text(encoding="utf-8")
    block = re.search(r"pub const CATALOG[^=]*=\s*&\[(.*?)\];", text, re.S)
    if not block:
        raise SystemExit("命令目录解析失败：mod.rs 里找不到 CATALOG 装配")
    assembled = re.findall(r"::([A-Za-z_][A-Za-z0-9_]*)", re.sub(r"//[^\n]*", "", block.group(1)))
    if len(assembled) != len(commands):
        raise SystemExit(
            f"命令目录解析失败：域文件里解析出 {len(commands)} 条命令，"
            f"而 mod.rs 的 CATALOG 装配了 {len(assembled)} 条。两者必须一致"
            "——新增命令要同时加 spec(…) 与装配项。"
        )


def header(comment: str, digest: str) -> str:
    return (
        f"{comment} 由 tools/codegen/generate.py 生成，**不要手改**。\n"
        f"{comment} schema: {SCHEMA_VERSION}  摘要: {digest}  contract: {CONTRACT_VERSION}\n"
        f"{comment} 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。\n"
    )


def ts_type(schema: dict) -> str:
    if "enum" in schema:
        return " | ".join(json.dumps(value) for value in schema["enum"])
    kind = schema.get("type")
    if kind == "string":
        return "string"
    if kind == "integer":
        return "number"
    if kind == "boolean":
        return "boolean"
    if kind == "object":
        return "Record<string, unknown>"
    return "unknown"


def ts_object(name: str, schema: dict) -> str:
    required = set(schema.get("required", []))
    lines = [f"export interface {name} {{"]
    for field, spec in schema.get("properties", {}).items():
        optional = "" if field in required else "?"
        lines.append(f"  {field}{optional}: {ts_type(spec)};")
    lines.append("}")
    return "\n".join(lines)


def emit_ts(schemas: dict[str, dict], digest: str, actions: list[dict]) -> str:
    parts = [header("//", digest), "", "/* eslint-disable */", ""]
    parts.append(f'export const SCHEMA_VERSION = "{SCHEMA_VERSION}" as const;')
    parts.append(f'export const CONTRACT_VERSION = "{CONTRACT_VERSION}" as const;')
    parts.append(f'export const SCHEMA_DIGEST = "{digest}" as const;')
    parts.append("")
    parts.append("export type ActionId = string;")
    parts.append("export type EntryId = string;")
    parts.append("export type Cursor = string;")
    parts.append("")

    money = schemas["money.schema.json"]
    parts.append(ts_object("Money", money))
    parts.append("")
    error = schemas["error.schema.json"]
    parts.append(ts_object("DomainError", error))
    parts.append("")
    action_defs = schemas["action.schema.json"]["definitions"]
    parts.append(ts_object("ActionRequest", action_defs["ActionRequest"]))
    parts.append("")
    parts.append(ts_object("ActionResult", action_defs["ActionResult"]))
    parts.append("")
    parts.append(ts_object("EventEnvelope", schemas["event.schema.json"]))
    parts.append("")

    parts.append("/** 已登记的命令（与 crates/cmd 的目录同源）。 */")
    parts.append("export const ACTIONS = [")
    for action in actions:
        parts.append(f'  {{ id: "{action["id"]}", capability: "{action["capability"]}" }},')
    parts.append("] as const;")
    parts.append("")
    parts.append("/** 已知 action id 集合。 */")
    parts.append("export const KNOWN_ACTIONS: ReadonlySet<string> = new Set(ACTIONS.map((a) => a.id));")
    parts.append("")
    parts.append(
        """/**
 * 解码一条命令结果。
 *
 * 未知 `status` 返回 `null`：服务端先上线新状态、旧客户端后升级是常态，
 * 这时候应当"跳过并记一笔"，不是抛异常把整条渲染链打断。
 */
export function decodeActionResult(raw: unknown): ActionResult | null {
  if (raw === null || typeof raw !== "object") return null;
  const value = raw as Record<string, unknown>;
  if (typeof value.action_id !== "string") return null;
  if (value.status !== "ok" && value.status !== "empty" && value.status !== "failed") return null;
  return value as unknown as ActionResult;
}

/** 金额：字符串 → BigInt（超过 2^53 也不能丢精度）。 */
export function moneyToMinor(money: Money): bigint {
  return BigInt(money.minor);
}
"""
    )
    return "\n".join(parts) + "\n"


def dart_type(schema: dict) -> str:
    if "enum" in schema:
        return "String"
    kind = schema.get("type")
    if kind == "string":
        return "String"
    if kind == "integer":
        return "int"
    if kind == "boolean":
        return "bool"
    if kind == "object":
        return "Map<String, dynamic>"
    return "dynamic"


def dart_class(name: str, schema: dict) -> str:
    required = set(schema.get("required", []))
    fields = schema.get("properties", {})
    lines = [f"class {name} {{"]
    for field, spec in fields.items():
        kind = dart_type(spec)
        lines.append(f"  final {kind}{'?' if field not in required else ''} {field};")
    lines.append("")
    lines.append(f"  const {name}({{")
    for field in fields:
        lines.append(f"    this.{field},")
    lines.append("  });")
    lines.append("")
    lines.append(f"  factory {name}.fromJson(Map<String, dynamic> json) {{")
    lines.append(f"    return {name}(")
    for field, spec in fields.items():
        kind = dart_type(spec)
        if kind == "int":
            # JSON 里数字字段可能是 int；金额/序列号是字符串，单独处理
            lines.append(f"      {field}: json['{field}'] as int?,")
        elif kind == "Map<String, dynamic>":
            lines.append(
                f"      {field}: json['{field}'] as Map<String, dynamic>?,"
            )
        elif kind == "bool":
            lines.append(f"      {field}: json['{field}'] as bool?,")
        else:
            lines.append(f"      {field}: json['{field}'] as String?,")
    lines.append("    );")
    lines.append("  }")
    lines.append("")
    lines.append("  Map<String, dynamic> toJson() => {")
    for field in fields:
        lines.append(f"    '{field}': {field},")
    lines.append("  };")
    lines.append("}")
    return "\n".join(lines)


def emit_dart(schemas: dict[str, dict], digest: str, actions: list[dict]) -> str:
    parts = [header("//", digest), "", "// ignore_for_file: unused_field", ""]
    parts.append(f"const String schemaVersion = '{SCHEMA_VERSION}';")
    parts.append(f"const String contractVersion = '{CONTRACT_VERSION}';")
    parts.append(f"const String schemaDigest = '{digest}';")
    parts.append("")
    parts.append(dart_class("Money", schemas["money.schema.json"]))
    parts.append("")
    parts.append(dart_class("DomainError", schemas["error.schema.json"]))
    parts.append("")
    action_defs = schemas["action.schema.json"]["definitions"]
    parts.append(dart_class("ActionRequest", action_defs["ActionRequest"]))
    parts.append("")
    parts.append(dart_class("ActionResult", action_defs["ActionResult"]))
    parts.append("")
    parts.append(dart_class("EventEnvelope", schemas["event.schema.json"]))
    parts.append("")
    parts.append("/// 已登记的命令（与 crates/cmd 的目录同源）。")
    parts.append("const Map<String, String> actionCapabilities = {")
    for action in actions:
        parts.append(f"  '{action['id']}': '{action['capability']}',")
    parts.append("};")
    parts.append("")
    parts.append(
        """/// 解码一条命令结果；未知 status 返回 null（跳过，不抛）。
ActionResult? decodeActionResult(Object? raw) {
  if (raw is! Map<String, dynamic>) return null;
  final status = raw['status'];
  if (status != 'ok' && status != 'empty' && status != 'failed') return null;
  if (raw['action_id'] is! String) return null;
  return ActionResult.fromJson(raw);
}

/// 金额：字符串 → BigInt（Dart web 上 int 是 double，必须走 BigInt）。
BigInt moneyToMinor(Money money) => BigInt.parse(money.minor!);

/// 这条命令认不认识。
bool isKnownAction(String actionId) => actionCapabilities.containsKey(actionId);
"""
    )
    return "\n".join(parts) + "\n"


def emit_actions(actions: list[dict], digest: str) -> str:
    doc = {
        "schema_version": SCHEMA_VERSION,
        "digest": digest,
        "contract": CONTRACT_VERSION,
        "actions": actions,
    }
    return json.dumps(doc, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def emit_manifest(digest: str, files: list[str]) -> str:
    doc = {
        "schema_version": SCHEMA_VERSION,
        "digest": digest,
        "contract": CONTRACT_VERSION,
        "generated_by": "tools/codegen/generate.py",
        "files": sorted(files),
    }
    return json.dumps(doc, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def build_outputs() -> dict[pathlib.Path, str]:
    schemas = load_schemas()
    digest = digest_of(schemas)
    actions = parse_catalog()
    outputs = {
        OUT_DIR / "ts" / "contract.ts": emit_ts(schemas, digest, actions),
        OUT_DIR / "dart" / "contract.dart": emit_dart(schemas, digest, actions),
        OUT_DIR / "actions.json": emit_actions(actions, digest),
        OUT_DIR / "manifest.json": emit_manifest(
            digest, ["ts/contract.ts", "dart/contract.dart", "actions.json"]
        ),
    }
    return outputs


def write_all(outputs: dict[pathlib.Path, str]) -> None:
    for path, text in outputs.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        # `newline="\n"` 是必须的：`write_text` 默认按 `os.linesep` 翻译，
        # Windows 上会写出 CRLF。生成物是三端共用的**逐字节**基线，行尾漂一次
        # `--check` 就在所有平台变红，而且 `git diff` 里是一整片噪声。
        with path.open("w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
        print(f"写出 {path.relative_to(ROOT)}")


def check(outputs: dict[pathlib.Path, str]) -> int:
    stale = []
    for path, text in outputs.items():
        if not path.is_file():
            stale.append(f"{path.relative_to(ROOT)}: 缺失")
            continue
        # 按**字节**比，不按解码后的 str 比：`read_text` 会把 CRLF 折成 LF，
        # 那样行尾漂移会被悄悄放过。生成物必须逐字节等于生成结果。
        if path.read_bytes() != text.encode("utf-8"):
            actual = path.read_bytes().count(b"\r\n")
            stale.append(
                f"{path.relative_to(ROOT)}: 与契约不一致（重跑生成器）"
                + (f"（含 {actual} 个 CRLF 行尾）" if actual else "")
            )
    if stale:
        print("生成物已过期：", file=sys.stderr)
        for line in stale:
            print(f"  - {line}", file=sys.stderr)
        return 1
    print(f"生成物与契约一致（{len(outputs)} 个文件）")
    return 0


def field_signature(schemas: dict[str, dict]) -> dict[str, dict[str, str]]:
    """把契约拍平成 `类型 → {字段: 类型}`，用于兼容性比对。"""
    flat: dict[str, dict[str, str]] = {}
    for name, schema in schemas.items():
        if "definitions" in schema:
            for type_name, definition in schema["definitions"].items():
                flat[type_name] = {
                    field: ts_type(spec)
                    for field, spec in definition.get("properties", {}).items()
                }
        elif "properties" in schema:
            flat[schema["title"]] = {
                field: ts_type(spec) for field, spec in schema["properties"].items()
            }
    return flat


def compatibility(schemas: dict[str, dict]) -> tuple[list[str], list[str]]:
    """返回（破坏性变更, 兼容性变更）。

    规则只有一条：**删字段、改类型 = 破坏**（旧客户端会读到不存在/不同类型
    的字段）；加字段 = 兼容（旧客户端忽略未知字段）。加"必填"字段也算破坏，
    因为旧客户端不会发它。
    """
    if not LOCK_FILE.is_file():
        return [], ["没有兼容基线（首次生成）"]
    lock = json.loads(LOCK_FILE.read_text(encoding="utf-8"))
    previous: dict[str, dict[str, str]] = lock.get("types", {})
    current = field_signature(schemas)
    breaking: list[str] = []
    compatible: list[str] = []
    for type_name, fields in previous.items():
        if type_name not in current:
            breaking.append(f"{type_name}: 整个类型被删除")
            continue
        for field, kind in fields.items():
            if field not in current[type_name]:
                breaking.append(f"{type_name}.{field}: 字段被删除")
            elif current[type_name][field] != kind:
                breaking.append(
                    f"{type_name}.{field}: 类型从 {kind} 变成 {current[type_name][field]}"
                )
        for field in current[type_name]:
            if field not in fields:
                compatible.append(f"{type_name}.{field}: 新增字段")
    for type_name in current:
        if type_name not in previous:
            compatible.append(f"{type_name}: 新增类型")
    return breaking, compatible


def update_lock(schemas: dict[str, dict]) -> None:
    doc = {
        "schema_version": SCHEMA_VERSION,
        "contract": CONTRACT_VERSION,
        "types": field_signature(schemas),
    }
    LOCK_FILE.write_text(
        json.dumps(doc, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(f"更新兼容基线 {LOCK_FILE.relative_to(ROOT)}")


def main() -> int:
    parser = argparse.ArgumentParser(description="契约生成器")
    parser.add_argument("--check", action="store_true", help="只检查，不写文件")
    parser.add_argument(
        "--update-lock", action="store_true", help="契约变更后更新兼容基线"
    )
    parser.add_argument(
        "--accept-breaking",
        action="store_true",
        help="确认破坏性变更（必须先升 contract 主版本）",
    )
    args = parser.parse_args()

    schemas = load_schemas()
    outputs = build_outputs()
    breaking, compatible = compatibility(schemas)
    for line in compatible:
        print(f"兼容变更：{line}")
    if breaking:
        print("破坏性变更：", file=sys.stderr)
        for line in breaking:
            print(f"  - {line}", file=sys.stderr)
        if not args.accept_breaking:
            print(
                "拒绝生成：破坏性变更必须先升 contract 主版本，再带 --accept-breaking 重跑",
                file=sys.stderr,
            )
            return 1

    if args.check:
        return check(outputs)
    write_all(outputs)
    if args.update_lock:
        update_lock(schemas)
    return 0


if __name__ == "__main__":
    sys.exit(main())
