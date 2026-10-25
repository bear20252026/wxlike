#!/usr/bin/env python3
"""Compare bincode enum order between Rust and the browser codec.

bincode serializes enum variants by declaration index. A reordered Rust enum
compiles fine and the browser decoder silently reads the wrong variant. This
check is cheap and catches exactly that drift before it reaches a browser.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def rust_enum(path: Path, name: str) -> list[str]:
    text = path.read_text(encoding="utf-8")
    match = re.search(rf"pub enum {name}\b[^{{]*\{{", text)
    if not match:
        raise ValueError(f"{path}: enum {name} not found")
    depth = 0
    body_start = match.end() - 1
    for idx in range(body_start, len(text)):
        if text[idx] == "{":
            depth += 1
        elif text[idx] == "}":
            depth -= 1
            if depth == 0:
                body = text[body_start + 1 : idx]
                break
    else:
        raise ValueError(f"{path}: enum {name} is unterminated")
    body = re.sub(r"///.*", "", body)
    body = re.sub(r"//.*", "", body)
    variants = re.findall(r"^\s*([A-Z][A-Za-z0-9_]*)\s*(?:[({,])", body, re.MULTILINE)
    return variants


def ts_union(path: Path, name: str) -> list[str]:
    text = path.read_text(encoding="utf-8")
    # The union arms contain semicolons, so stopping at the first ";" truncates
    # at variant 0. A TypeScript type block ends at a blank line.
    match = re.search(rf"export type {name}\s*=\s*(.*?)\n\s*\n", text, re.DOTALL)
    if not match:
        raise ValueError(f"{path}: union {name} not found or not followed by a blank line")
    body = match.group(1)
    return re.findall(r'readonly t:\s*"([A-Za-z0-9_]+)"', body)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    root = args.root

    checks = [
        ("WireFrame", root / "crates/net/src/wire.rs", "WireFrame", root / "ui/src/web/wire.ts"),
        ("MsgBody", root / "crates/core/src/types.rs", "MsgBody", root / "ui/src/web/wire.ts"),
        ("Receipt", root / "crates/core/src/types.rs", "Receipt", root / "ui/src/web/wire.ts"),
    ]
    problems: list[str] = []
    for name, rust_path, rust_name, ts_path in checks:
        try:
            rust = rust_enum(rust_path, rust_name)
            ts = ts_union(ts_path, name)
        except ValueError as err:
            problems.append(str(err))
            continue
        if rust != ts:
            problems.append(f"{name}: Rust {rust} != browser {ts}")
        else:
            print(f"{name}: {len(rust)} variants in order")
    for problem in problems:
        print(f"ERROR {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
