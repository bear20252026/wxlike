#!/usr/bin/env python3
"""质量闸门：**只允许变好**。

背景见 `docs/code-quality-audit.md`。审计给了四个基线数字，但"当时达标"
不等于"以后不退化"——没有闸门，下一轮并行开发会把数字原样还回去。

这个脚本做三件事：

1. 跑 `docs/bench/quality_probe.py`，拿到当前指标；
2. 与 `docs/bench/quality-baseline.json` 逐项比对；
3. 任何一项**变差**就退出码 1，并打印差在哪、差了多少。

刻意只卡"变差"，不卡"绝对值"：存量 47 个长函数不该让 CI 立刻红，
否则所有人第一件事是加 `#[allow]`。等它们被拆掉，用 `--update` 把基线降下来，
闸门就自动收紧——**基线只会单向下降**。

用法：
    python scripts/quality-gate.py            # 检查（CI 用这个）
    python scripts/quality-gate.py --update   # 把当前值写成新基线（只许变好）
    python scripts/quality-gate.py --explain  # 打印每个指标的口径
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE = ROOT / "docs" / "bench" / "quality_probe.py"
BASELINE = ROOT / "docs" / "bench" / "quality-baseline.json"

# (键, 中文名, 口径说明)。数值型指标：**越小越好**。
METRICS = [
    ("count_len_gt60", "函数 >60 行", "单函数行数（含签名行，不含注释剥离后的空行）"),
    ("count_len_gt100", "函数 >100 行", "同上；超过 100 行的函数另有名单比对"),
    ("count_nest_gt3", "嵌套 >3 层", "函数体内大括号最大深度减一（跳过字符串字面量）"),
    ("dup_windows", "跨文件重复窗口", "8 行窗口归一化后跨文件命中次数"),
    ("magic_total", "魔法数字总数", "两位以上、非 const/static 行、非常见值（0/1/10/100…）"),
]

EXPLAIN = """
口径（都是启发式，别当编译器级结论）：

- 只统计生产代码：不含 `tests/` 目录，不含 `#[cfg(test)]` 之后的模块。
- 嵌套按大括号算，跳过字符串与字符字面量；`match` 的多行模式会略微高估。
- 重复窗口是 8 行归一化后做哈希，跨文件比对；同文件内重复不计。
- 魔法数字排除了 const/static 行与 0/1/2/10/16/24/60/100/1000 等常见值，
  以及 HTTP 状态码（它们是协议常量，不是"魔法"）。
- `long_fn_keys` 比对的是 `文件::函数名`：数量不变但换了个函数变长，同样算退化。
"""


def run_probe() -> dict:
    proc = subprocess.run(
        [sys.executable, str(PROBE)],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        print("探针执行失败：", file=sys.stderr)
        print(proc.stderr[-2000:], file=sys.stderr)
        sys.exit(2)
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as err:
        print(f"探针输出不是 JSON：{err}", file=sys.stderr)
        sys.exit(2)


def main() -> int:
    if "--explain" in sys.argv:
        print(EXPLAIN)
        return 0

    current = run_probe()

    if "--update" in sys.argv:
        if not BASELINE.exists():
            BASELINE.write_text(
                json.dumps(current, ensure_ascii=False, indent=1), encoding="utf-8"
            )
            print(f"已写入基线：{BASELINE.relative_to(ROOT)}")
            return 0
        base = json.loads(BASELINE.read_text(encoding="utf-8"))
        worse = [k for k, _, _ in METRICS if current[k] > base[k]]
        if worse:
            print("拒绝更新：以下指标比基线更差，先把它们修回来：")
            for key in worse:
                print(f"  {key}: 基线 {base[key]} → 当前 {current[key]}")
            return 1
        BASELINE.write_text(
            json.dumps(current, ensure_ascii=False, indent=1), encoding="utf-8"
        )
        print(f"基线已收紧：{BASELINE.relative_to(ROOT)}")
        for key, label, _ in METRICS:
            if current[key] < base[key]:
                print(f"  {label}: {base[key]} → {current[key]}")
        return 0

    if not BASELINE.exists():
        print(
            f"缺少基线 {BASELINE.relative_to(ROOT)}；先跑 "
            "`python scripts/quality-gate.py --update` 生成",
            file=sys.stderr,
        )
        return 2

    base = json.loads(BASELINE.read_text(encoding="utf-8"))
    failures: list[str] = []

    print(f"{'指标':<22}{'基线':>8}{'当前':>8}{'变化':>8}")
    for key, label, _ in METRICS:
        before, now = base[key], current[key]
        delta = now - before
        mark = "  " if delta <= 0 else " ✗"
        print(f"{label:<22}{before:>8}{now:>8}{delta:>+8}{mark}")
        if delta > 0:
            failures.append(f"{label}：{before} → {now}（+{delta}）")

    # 名单比对：数量没变，但"哪个函数长"变了，也要能看出来
    base_long = set(base.get("long_fn_keys", []))
    now_long = set(current.get("long_fn_keys", []))
    for key in sorted(now_long - base_long):
        failures.append(f"新增长函数（>100 行）：{key}")
    for key in sorted(base_long - now_long):
        print(f"  ✓ 已拆掉：{key}")

    print()
    if failures:
        print("质量闸门未通过：")
        for item in failures:
            print(f"  ✗ {item}")
        print()
        print("要么把改动改小，要么按 docs/code-quality-audit.md 的修法拆开。")
        print("确实无法避免时，在评审里说明理由，再用 --update 收紧基线是**不允许**的。")
        return 1

    print("质量闸门通过（没有指标变差）。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
