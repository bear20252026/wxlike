#!/usr/bin/env python3
"""#53 防回潮：统计非测试生产代码里的 unwrap()/expect()，并与基线做 diff。

背景见 `docs/audit-progress.md` 与 issue #53。逐处清理完不等于以后不会长回来：
下一轮并行开发只要在请求路径上顺手写一个 `unwrap()`，计数就会原样反弹。
这个脚本负责把反弹**看见**——默认非阻断，计数回潮时退出码仍是 0：

- 只统计生产代码：`crates/*/src`、`server/src`、`ui/src-tauri/src`；
  跳过 `tests/`、`examples/`、`benches/` 与 `#[cfg(test)]` 修饰的整项。
- 基线在 `scripts/unwrap-baseline.json`，**只许单向下降**：`--update` 拒绝写回更大的总数。
- CI 里跑 `--details` 并把输出贴到 PR 评论；要接硬闸门时加 `--fail-on-increase`。

用法：
    python scripts/count_unwraps.py                     # 计数 + 与基线 diff
    python scripts/count_unwraps.py --details           # 附带各 crate 明细与回潮行号
    python scripts/count_unwraps.py --json              # 机器可读
    python scripts/count_unwraps.py --update            # 计数下降后收紧基线
    python scripts/count_unwraps.py --fail-on-increase  # 有回潮时退出 1
"""

from __future__ import annotations

import argparse
import collections
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BASELINE = ROOT / "scripts" / "unwrap-baseline.json"
ROOTS = ["crates", "server/src", "ui/src-tauri/src"]
SKIP_DIRS = {"target", "node_modules", ".git", "tests", "examples", "benches"}

CFG_TEST_RE = re.compile(r"#\[cfg\([^\]]*\btest\b[^\]]*\)\]")
UNWRAP_RE = re.compile(r"\.unwrap\s*\(\s*\)")
EXPECT_RE = re.compile(r"\.expect\s*\(")


def rust_files() -> list[str]:
    found: list[str] = []
    for root in ROOTS:
        base = ROOT / root
        if not base.exists():
            continue
        for path in base.rglob("*.rs"):
            rel = path.relative_to(ROOT).as_posix()
            parts = set(rel.split("/"))
            if parts & SKIP_DIRS:
                continue
            found.append(rel)
    return sorted(found)


def strip_code(line: str) -> str:
    """去掉行注释与字符串/字符字面量，只留结构性字符。

    口径与 `docs/bench/quality_probe.py` 一致：这样 `.unwrap()` 出现在
    字符串或注释里时不会被误算，`'a` 生命周期也不会被当成字符字面量。
    """
    out: list[str] = []
    i = 0
    n = len(line)
    in_str = False
    in_char = False
    while i < n:
        c = line[i]
        if in_str:
            if c == "\\":
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if in_char:
            if c == "\\":
                i += 2
                continue
            if c == "'":
                in_char = False
            i += 1
            continue
        if c == '"':
            in_str = True
            i += 1
            continue
        if c == "'":
            if i + 2 < n and line[i + 2] == "'":
                in_char = True
                i += 1
                continue
            i += 1
            continue
        if c == "/" and i + 1 < n and line[i + 1] == "/":
            break
        out.append(c)
        i += 1
    return "".join(out)


def skip_cfg_test_item(lines: list[str], start: int, first_code: int) -> int:
    """返回 `#[cfg(test)]` 修饰的整项最后一行下标。"""
    depth = 0
    started = False
    j = start
    while j < len(lines):
        code = strip_code(lines[j][first_code:] if j == start else lines[j])
        for ch in code:
            if ch == "{":
                depth += 1
                started = True
            elif ch == "}":
                depth -= 1
                if started and depth <= 0:
                    return j
            elif ch == ";" and not started and depth == 0:
                return j
        j += 1
    return len(lines) - 1


def production_lines(path: pathlib.Path) -> list[str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    out = list(lines)
    i = 0
    while i < len(lines):
        match = CFG_TEST_RE.search(lines[i])
        if not match:
            i += 1
            continue
        end = skip_cfg_test_item(lines, i, match.end())
        for k in range(i, end + 1):
            out[k] = ""
        i = end + 1
    return out


def scan() -> tuple[collections.Counter, dict[str, list[tuple[int, str]]]]:
    counts: collections.Counter = collections.Counter()
    sites: dict[str, list[tuple[int, str]]] = collections.defaultdict(list)
    for rel in rust_files():
        lines = production_lines(ROOT / rel)
        for lineno, line in enumerate(lines, 1):
            code = strip_code(line)
            for match in UNWRAP_RE.finditer(code):
                counts[rel] += 1
                sites[rel].append((lineno, "unwrap"))
            for match in EXPECT_RE.finditer(code):
                counts[rel] += 1
                sites[rel].append((lineno, "expect"))
    return counts, sites


def load_baseline() -> dict:
    if not BASELINE.exists():
        print(
            f"缺少基线 {BASELINE.relative_to(ROOT)}；先跑 "
            "`python scripts/count_unwraps.py --update` 生成",
            file=sys.stderr,
        )
        sys.exit(2)
    data = json.loads(BASELINE.read_text(encoding="utf-8"))
    if data.get("roots") != ROOTS:
        print(
            "基线统计范围与脚本不一致：\n"
            f"  基线 roots（{BASELINE.relative_to(ROOT)}）：{data.get('roots')}\n"
            f"  脚本 roots：{ROOTS}\n"
            "先确认是否故意扩大/缩小范围，再用 --update 重写基线。",
            file=sys.stderr,
        )
        sys.exit(2)
    data.setdefault("by_file", {})
    return data


def summarize_crates(by_file: dict[str, int]) -> dict[str, int]:
    totals: collections.Counter = collections.Counter()
    for path, count in by_file.items():
        parts = path.split("/")
        crate = "/".join(parts[:2]) if parts[0] == "crates" else parts[0]
        totals[crate] += count
    return dict(sorted(totals.items()))


def print_report(
    current: collections.Counter,
    sites: dict[str, list[tuple[int, str]]],
    baseline: dict,
    details: bool,
) -> None:
    base_by_file = {k: int(v) for k, v in baseline.get("by_file", {}).items()}
    now_by_file = {k: int(v) for k, v in current.items() if v}
    base_total = int(baseline.get("total", 0))
    now_total = sum(now_by_file.values())
    delta = now_total - base_total

    print("unwrap()/expect() 计数（非测试生产代码）")
    print(f"总计：基线 {base_total} → 当前 {now_total}（{delta:+d}）")
    if delta > 0:
        print("状态：回潮 ✗")
    elif delta < 0:
        print("状态：已清理 ✓")
    else:
        print("状态：持平")

    base_crates = summarize_crates(base_by_file)
    now_crates = summarize_crates(now_by_file)
    print("\n按 crate：")
    for crate in sorted(set(base_crates) | set(now_crates)):
        before = base_crates.get(crate, 0)
        after = now_crates.get(crate, 0)
        change = after - before
        suffix = "" if change == 0 else f"  {change:+d}"
        print(f"  {crate:<24}{before:>5} → {after:<5}{suffix}")

    increased = sorted(
        (path, base_by_file.get(path, 0), now_by_file.get(path, 0))
        for path in set(base_by_file) | set(now_by_file)
        if now_by_file.get(path, 0) > base_by_file.get(path, 0)
    )
    decreased = sorted(
        (path, base_by_file.get(path, 0), now_by_file.get(path, 0))
        for path in set(base_by_file) | set(now_by_file)
        if now_by_file.get(path, 0) < base_by_file.get(path, 0)
    )

    if increased:
        print("\n回潮（+）：")
        for path, before, after in increased:
            tag = "新增文件" if before == 0 else "增加"
            print(f"  {path}  {before} → {after}  +{after - before}（{tag}）")
            if details:
                for lineno, kind in sites.get(path, []):
                    print(f"    {path}:{lineno}  .{kind}()")
    if decreased:
        print("\n已清理（-）：")
        for path, before, after in decreased:
            print(f"  {path}  {before} → {after}  {after - before}")

    if details:
        print("\n各文件明细：")
        for path in sorted(now_by_file):
            print(f"  {path:<60}{now_by_file[path]:>4}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--details", action="store_true", help="附带各文件与回潮行号")
    parser.add_argument("--json", action="store_true", help="输出 JSON")
    parser.add_argument("--update", action="store_true", help="把当前计数写成新基线（只许下降）")
    parser.add_argument("--force", action="store_true", help="配合 --update 允许基线上升")
    parser.add_argument(
        "--fail-on-increase",
        action="store_true",
        help="计数比基线大时退出 1（默认非阻断）",
    )
    args = parser.parse_args()

    current, sites = scan()
    now_by_file = {k: int(v) for k, v in current.items() if v}
    now_total = sum(now_by_file.values())
    had_baseline = BASELINE.exists()
    baseline = load_baseline() if had_baseline else {"total": 0, "by_file": {}}
    base_total = int(baseline.get("total", 0))

    if args.update:
        if had_baseline and now_total > base_total and not args.force:
            print(
                f"拒绝更新：当前 {now_total} 比基线 {base_total} 多 "
                f"{now_total - base_total} 处。先清理回潮，或明确加 --force。",
                file=sys.stderr,
            )
            return 1
        data = {
            "roots": ROOTS,
            "total": now_total,
            "by_file": dict(sorted(now_by_file.items())),
        }
        BASELINE.write_text(
            json.dumps(data, ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
        )
        print(f"基线已写入：{BASELINE.relative_to(ROOT)}（{base_total} → {now_total}）")
        return 0

    if args.json:
        print(
            json.dumps(
                {
                    "roots": ROOTS,
                    "baseline_total": base_total,
                    "total": now_total,
                    "delta": now_total - base_total,
                    "by_file": dict(sorted(now_by_file.items())),
                },
                ensure_ascii=False,
                indent=1,
            )
        )
    else:
        print_report(current, sites, baseline, args.details)

    if args.fail_on_increase and now_total > base_total:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
