#!/usr/bin/env python3
"""把 cargo-llvm-cov 产出的 lcov.info 汇总成一段 markdown（T-F1 覆盖率 job 用）。

刻意只做汇总，不做阈值判断：T-F1 要求先跑两周收集实测基线，阈值由集成方在
Wave 3 依据实测数据确定。在此之前这个 job 只负责让数字在 PR 上可见。

用法：
    python scripts/coverage_summary.py lcov.info
    python scripts/coverage_summary.py lcov.info --output coverage-summary.md
    python scripts/coverage_summary.py lcov.info --strict   # 缺文件/无数据时退出 1
"""

from __future__ import annotations

import argparse
import pathlib
import sys

THRESHOLD_NOTE = (
    "阈值尚未设置：T-F1 要求先积累两周实测基线，由集成方在 Wave 3 定阈值；"
    "在此之前本 job 只上报、不阻断。"
)


def parse_lcov(path: pathlib.Path) -> tuple[int, int, int]:
    files = 0
    lines_found = 0
    lines_hit = 0
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if raw.startswith("SF:"):
            files += 1
        elif raw.startswith("LF:"):
            lines_found += int(raw[3:].strip() or 0)
        elif raw.startswith("LH:"):
            lines_hit += int(raw[3:].strip() or 0)
    return files, lines_found, lines_hit


def render_missing(reason: str) -> str:
    return "\n".join(
        [
            "## 覆盖率（非阻断）",
            "",
            f"本次未生成可用报告：{reason}",
            "",
            "本 job 不阻断合并；编译/测试失败由 `test-linux` 拦截。",
            "",
        ]
    )


def render(files: int, found: int, hit: int) -> str:
    if found <= 0:
        return render_missing("lcov.info 里没有行覆盖数据")
    pct = hit * 100.0 / found
    return "\n".join(
        [
            "## 覆盖率（非阻断）",
            "",
            "| 指标 | 值 |",
            "|---|---|",
            f"| 行覆盖 | **{pct:.2f}%** |",
            f"| 命中 / 总行 | {hit:,} / {found:,} |",
            f"| 统计文件 | {files:,} |",
            "",
            THRESHOLD_NOTE,
            "",
            "报告见 artifact `coverage-lcov`。",
            "",
        ]
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lcov", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path, help="同时写入这个 markdown 文件")
    parser.add_argument("--strict", action="store_true", help="报告缺失时退出 1")
    args = parser.parse_args()

    if not args.lcov.exists():
        text = render_missing(f"找不到 {args.lcov}")
        ok = False
    else:
        files, found, hit = parse_lcov(args.lcov)
        text = render(files, found, hit)
        ok = found > 0

    print(text)
    if args.output:
        args.output.write_text(text, encoding="utf-8")
    if args.strict and not ok:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
