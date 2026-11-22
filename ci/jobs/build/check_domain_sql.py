#!/usr/bin/env python3
"""domain 层 SQL 防回潮门禁。

`crates/platform/src/domain/*.rs` 的约定是「只放业务规则与类型，SQL 一律走
`domain/repo/*`」。迁移是分批的，所以这里用**逐文件基线**而不是一刀切：

- 已在基线里的文件，SQL 数量只能下降，不能上升；
- 新文件出现任何 SQL 都失败（说明又开了一条绕过 repo 的路径）；
- 基线只允许单向收紧；`--update` 默认拒绝上升。

统计口径：只数生产代码的 SQL 字符串开头（SELECT/INSERT/UPDATE/DELETE），
普通双引号与 `r#"..."` 原始字符串都算，遇到第一个 root 级 `#[cfg(test)]`
就停止，测试夹具里的 SQL 不计入。
"""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parents[3]
DOMAIN_ROOT = REPO_ROOT / "crates" / "platform" / "src" / "domain"
BASELINE = REPO_ROOT / "ci" / "jobs" / "policy" / "domain-sql-baseline.json"
SQL_RE = re.compile(r'(?:r#*)?"\s*(?:SELECT|INSERT|UPDATE|DELETE)\b')
TEST_MODULE_RE = re.compile(r"^\s*#\[cfg\(test\)\]", re.M)


def production_prefix(text: str) -> str:
    match = TEST_MODULE_RE.search(text)
    return text[: match.start()] if match else text


def scan() -> dict[str, int]:
    rows: dict[str, int] = {}
    for path in sorted(DOMAIN_ROOT.glob("*.rs")):
        if path.name == "mod.rs":
            continue
        count = len(SQL_RE.findall(production_prefix(path.read_text(encoding="utf-8"))))
        if count:
            rows[path.name] = count
    return rows


def load_baseline() -> dict[str, int]:
    if not BASELINE.exists():
        sys.exit(f"missing baseline: {BASELINE}")
    data = json.loads(BASELINE.read_text(encoding="utf-8"))
    return data.get("files", {})


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--update", action="store_true", help="write a tighter baseline")
    parser.add_argument("--force", action="store_true", help="allow baseline to grow")
    args = parser.parse_args()

    current = scan()
    baseline = load_baseline()
    failures: list[str] = []

    for name, count in current.items():
        old = baseline.get(name, 0)
        if count > old:
            failures.append(f"{name}: {old} -> {count} (domain SQL increased)")

    for name, count in current.items():
        if name not in baseline:
            failures.append(f"{name}: new domain SQL file with {count} statement(s)")

    if args.update:
        total = sum(current.values())
        old_total = sum(baseline.values())
        if total > old_total and not args.force:
            sys.exit(f"refusing to raise baseline: {old_total} -> {total} (use --force)")
        BASELINE.write_text(
            json.dumps({"total": total, "files": current}, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8",
        )
        print(f"updated {BASELINE}: total={total}, files={len(current)}")
        return 0

    if failures:
        print("domain SQL gate failed:", file=sys.stderr)
        for item in failures:
            print(f"  - {item}", file=sys.stderr)
        return 1

    total = sum(current.values())
    print(f"domain SQL gate ok: total={total}, files={len(current)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
