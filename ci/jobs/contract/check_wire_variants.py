#!/usr/bin/env python3
"""Compare bincode enum order and struct field order between the real hub and
the browser wire mirror.

bincode serializes enum variants by declaration index and struct fields by
declaration order. Reordering either side compiles fine and the browser
decoder silently reads the wrong variant/field. The mirror lives in
``crates/client-web/src/wire.rs`` (the retired ``ui/src/web/wire.ts`` is gone
with C-02); this check is cheap and catches drift before it reaches a browser.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def rust_body(path: Path, kind: str, name: str) -> str:
    text = path.read_text(encoding="utf-8")
    match = re.search(rf"pub {kind} {name}\b[^{{]*\{{", text)
    if not match:
        raise ValueError(f"{path}: {kind} {name} not found")
    depth = 0
    body_start = match.end() - 1
    for idx in range(body_start, len(text)):
        if text[idx] == "{":
            depth += 1
        elif text[idx] == "}":
            depth -= 1
            if depth == 0:
                return text[body_start + 1 : idx]
    raise ValueError(f"{path}: {kind} {name} is unterminated")


def rust_enum(path: Path, name: str) -> list[str]:
    body = rust_body(path, "enum", name)
    body = re.sub(r"///.*", "", body)
    body = re.sub(r"//.*", "", body)
    return re.findall(r"^\s*([A-Z][A-Za-z0-9_]*)\s*(?:[({,])", body, re.MULTILINE)


def rust_fields(path: Path, name: str) -> list[str]:
    body = rust_body(path, "struct", name)
    body = re.sub(r"///.*", "", body)
    body = re.sub(r"//.*", "", body)
    return re.findall(r"^\s*pub\s+([a-z_][a-z0-9_]*)\s*:", body, re.MULTILINE)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    root = args.root

    server_wire = root / "crates/net/src/wire.rs"
    server_types = root / "crates/core/src/types.rs"
    browser_wire = root / "crates/client-web/src/wire.rs"

    enum_checks = [
        ("WireFrame", server_wire, browser_wire),
        ("MsgBody", server_types, browser_wire),
        ("Receipt", server_types, browser_wire),
    ]
    struct_checks = [("Envelope", server_types, browser_wire)]

    problems: list[str] = []
    for name, server, browser in enum_checks:
        try:
            left = rust_enum(server, name)
            right = rust_enum(browser, name)
        except ValueError as err:
            problems.append(str(err))
            continue
        if left != right:
            problems.append(f"{name}: hub {left} != browser {right}")
        else:
            print(f"{name}: {len(left)} variants in order")
    for name, server, browser in struct_checks:
        try:
            left = rust_fields(server, name)
            right = rust_fields(browser, name)
        except ValueError as err:
            problems.append(str(err))
            continue
        if left != right:
            problems.append(f"{name}: hub fields {left} != browser {right}")
        else:
            print(f"{name}: {len(left)} fields in order")
    for problem in problems:
        print(f"ERROR {problem}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
