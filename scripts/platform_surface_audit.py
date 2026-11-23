"""盘点 `Platform` 的公开方法面：哪些方法真的被 platform 之外的 crate 调用。

`Platform` 是组合根，不是业务服务；大量 `pub fn` 只是给同 crate 的 handler
用的。这个脚本把每个方法的可见性与外部调用点列出来，供“改 pub 为
pub(crate)”或“拆 façade”时做依据。

用法：
    python scripts/platform_surface_audit.py            # 人读的摘要
    python scripts/platform_surface_audit.py --json     # 机器可读

口径：
- 只统计 `impl Platform` 块里的方法，不把 free function / 其它 impl 混进来；
- “外部调用”指 `crates/platform` 之外的 Rust 文件里出现 `.method(` 或
  `Platform::method(`。同名方法可能来自别的类型，脚本给出位置供人工复核。
"""
from __future__ import annotations

import argparse
import json
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
PLATFORM_SRC = REPO_ROOT / "crates" / "platform" / "src"
SCAN_ROOTS = [REPO_ROOT / "crates", REPO_ROOT / "server" / "src"]

METHOD_RE = re.compile(r"^\s{4}pub(?:\(crate\))?\s+(?:async\s+)?fn\s+([A-Za-z0-9_]+)", re.M)
VIS_RE = re.compile(r"^\s{4}pub(\(crate\))?\s+(?:async\s+)?fn\s+([A-Za-z0-9_]+)", re.M)


def strip_literals(text: str) -> str:
    """去掉字符串/字符字面量，避免把 `{` `}` 算进 impl 块边界。"""
    text = re.sub(r'"(?:\\.|[^"\\])*"', '""', text)
    text = re.sub(r"'(?:\\.|[^'\\])*'", "''", text)
    return text


def platform_methods() -> list[dict[str, str]]:
    methods: list[dict[str, str]] = []
    for path in sorted(PLATFORM_SRC.rglob("*.rs")):
        text = strip_literals(path.read_text(encoding="utf-8"))
        for m in re.finditer(
            r"impl(?:<[^>]*>)?\s+(?:(?:[A-Za-z0-9_:<>]+)\s+for\s+)?Platform\b", text
        ):
            brace = text.find("{", m.end())
            if brace < 0:
                continue
            depth = 0
            end = len(text)
            for i in range(brace, len(text)):
                if text[i] == "{":
                    depth += 1
                elif text[i] == "}":
                    depth -= 1
                    if depth == 0:
                        end = i
                        break
            block = text[brace:end]
            for vm in VIS_RE.finditer(block):
                methods.append(
                    {
                        "method": vm.group(2),
                        "visibility": "pub(crate)" if vm.group(1) else "pub",
                        "file": str(path.relative_to(REPO_ROOT)),
                    }
                )
    return methods


def rust_files() -> list[Path]:
    files: list[Path] = []
    for root in SCAN_ROOTS:
        if not root.exists():
            continue
        for path in root.rglob("*.rs"):
            try:
                rel = path.relative_to(REPO_ROOT)
            except ValueError:
                continue
            rel = str(rel).replace("\\", "/")
            if rel.startswith("crates/platform/src/"):
                continue
            files.append(path)
    return files


def load_sources() -> list[tuple[Path, str]]:
    sources: list[tuple[Path, str]] = []
    for path in rust_files():
        try:
            sources.append((path, path.read_text(encoding="utf-8")))
        except UnicodeDecodeError:
            continue
    return sources


def external_calls(method: str, sources: list[tuple[Path, str]]) -> list[str]:
    call_re = re.compile(rf"(?:\.|Platform::)\s*{re.escape(method)}\s*\(")
    hits: list[str] = []
    for path, text in sources:
        for n, line in enumerate(text.splitlines(), 1):
            if call_re.search(line):
                hits.append(f"{path.relative_to(REPO_ROOT)}:{n}")
    return hits


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    files = load_sources()
    rows = []
    for m in platform_methods():
        hits = external_calls(m["method"], files)
        rows.append({**m, "external_calls": len(hits), "sites": hits[:8]})

    if args.json:
        print(json.dumps(rows, ensure_ascii=False, indent=1))
        return

    pub = sum(1 for r in rows if r["visibility"] == "pub")
    crate = len(rows) - pub
    print(f"Platform methods: {len(rows)} (pub={pub}, pub(crate)={crate})")
    print("外部有调用的 pub 方法：")
    for r in sorted(
        (r for r in rows if r["visibility"] == "pub" and r["external_calls"]),
        key=lambda r: (-r["external_calls"], r["method"]),
    ):
        print(f"  {r['method']:<28} {r['external_calls']:>3}  {r['file']}")
        for site in r["sites"]:
            print(f"      {site}")
    print("\n外部零调用的 pub 方法（可优先降为 pub(crate)）：")
    for r in sorted(
        (r for r in rows if r["visibility"] == "pub" and not r["external_calls"]),
        key=lambda r: (r["file"], r["method"]),
    ):
        print(f"  {r['method']:<28} {r['file']}")


if __name__ == "__main__":
    main()
