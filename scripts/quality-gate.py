#!/usr/bin/env python3
"""质量闸门：**只允许变好**，但允许渐进式改进。

背景见 `docs/code-quality-audit.md`。审计给了四个基线数字，但"当时达标"
不等于"以后不退化"——没有闸门，下一轮并行开发会把数字原样还回去。

## 三种运行模式

1. **全量模式**（默认）：扫全项目，与基线逐项比对。任何一项变差就退出码 1。
2. **增量模式** `--diff`：只扫 PR diff 涉及的文件，检查这些文件有没有引入
   新的退化。这是 CI 日常运行的模式——存量问题不该阻塞每个 PR。
3. **容错模式** `--slack N`：允许 `base + N` 的退化，用于大型重构期间。

## 豁免机制

确实无法避免退化时，在 `docs/bench/quality-allowlist.json` 里登记位点并写明
理由。豁免是**显式的**、**有理由的**，不是"改不动就绕过"。

用法：
    python scripts/quality-gate.py               # 全量检查（CI 用这个）
    python scripts/quality-gate.py --diff        # 增量检查（只扫 PR diff）
    python scripts/quality-gate.py --slack 5     # 允许 base+5 的退化
    python scripts/quality-gate.py --update      # 把当前值写成新基线
    python scripts/quality-gate.py --explain     # 打印每个指标的口径
"""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROBE = ROOT / "docs" / "bench" / "quality_probe.py"
BASELINE = ROOT / "docs" / "bench" / "quality-baseline.json"
ALLOWLIST = ROOT / "docs" / "bench" / "quality-allowlist.json"

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

三种模式：
- 全量：扫全项目，任何变差都报错。适合发布前检查。
- 增量（--diff）：只扫 PR diff 涉及的文件。存量问题不阻塞新 PR。
- 容错（--slack N）：允许 base+N 的退化。适合大型重构期间。

豁免：在 docs/bench/quality-allowlist.json 里登记位点并写明理由。
"""


def run_probe(diff_mode: bool = False) -> dict:
    """跑探针。增量模式下只扫 diff 涉及的文件。"""
    cmd = [sys.executable, str(PROBE)]
    if diff_mode:
        cmd.append("--diff")
    proc = subprocess.run(
        cmd,
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


def load_allowlist() -> dict:
    """读取豁免名单。文件不存在时返回空名单。"""
    if not ALLOWLIST.exists():
        return {}
    try:
        data = json.loads(ALLOWLIST.read_text(encoding="utf-8"))
        # 过滤掉以 _ 开头的注释键
        return {k: v for k, v in data.items() if not k.startswith("_")}
    except json.JSONDecodeError as err:
        print(f"豁免名单格式错误：{err}", file=sys.stderr)
        sys.exit(2)


def resolve_diff_base() -> str:
    """挑一个能代表"PR 基线"的 ref，别写死 `main...HEAD`。

    写死 `main...HEAD` 在两种环境下会失控：

    - 本地 `main` 滞后（多年没 fetch）时，merge-base 会退化成 HEAD，diff 变空
      ——门禁**静默放行**；或退化成一个无关旧点，把不相干的文件全扫进来。
    - 多 worktree 下 `main` 可能检出在别的 worktree 里，本地根本动不了它。

    CI 的 `audit` 任务在跑质量门之前会 `git fetch origin main:main`，所以那里
    `main` 是新的；本地优先 `origin/main`。`CI_BASE_SHA` 显式指定时最优先
    （与 coverage 任务 `CI_BASE_SHA` 的口径一致）。

    与 `docs/bench/quality_probe.py` 的 `resolve_diff_base()` 保持一致——
    那边的 `diff_files()` 才是真正决定扫哪些文件的，两边基线不同会各说各话。
    """
    env = os.environ.get("CI_BASE_SHA")
    if env:
        return env
    for candidate in ("origin/main", "main"):
        proc = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", candidate],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        if proc.returncode == 0 and proc.stdout.strip():
            return candidate
    print(
        "找不到 diff 基线：CI_BASE_SHA 未设置，origin/main 与 main 都不存在。\n"
        "先 `git fetch origin main`，或显式设 CI_BASE_SHA=<merge-base sha>。",
        file=sys.stderr,
    )
    sys.exit(2)


def get_diff_files() -> set[str]:
    """拿到 PR diff 涉及的文件列表。基线挑选见 `resolve_diff_base`。"""
    base = resolve_diff_base()
    proc = subprocess.run(
        ["git", "diff", "--name-only", f"{base}...HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        print(f"git diff {base}...HEAD 失败：", file=sys.stderr)
        print(proc.stderr[-2000:], file=sys.stderr)
        sys.exit(2)
    out = proc.stdout.strip()
    # 空 diff 要返回空集：`"".split("\n")` 会给出 `[""]`，于是"0 个文件"
    # 被当成 1 个名字为空的文件，打印与计数都会误导人。
    return set(out.split("\n")) if out else set()


def main() -> int:
    if "--explain" in sys.argv:
        print(EXPLAIN)
        return 0

    diff_mode = "--diff" in sys.argv
    slack = 0
    if "--slack" in sys.argv:
        idx = sys.argv.index("--slack")
        if idx + 1 < len(sys.argv):
            slack = int(sys.argv[idx + 1])

    current = run_probe(diff_mode)

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
    allowlist = load_allowlist()
    failures: list[str] = []

    # 增量模式下，probe 已经只扫 diff 文件，这里只打印 diff 文件列表
    if diff_mode:
        diff_files = get_diff_files()
        print(f"增量模式：只检查 {len(diff_files)} 个 diff 文件")
        for f in sorted(diff_files)[:10]:
            print(f"  {f}")
        if len(diff_files) > 10:
            print(f"  ... 等共 {len(diff_files)} 个")
        print()

    print(f"{'指标':<22}{'基线':>8}{'当前':>8}{'容错':>8}{'变化':>8}")
    for key, label, _ in METRICS:
        before, now = base.get(key, 0), current.get(key, 0)
        # 豁免：从当前值里减去豁免的位点数量
        allowed = allowlist.get(key, [])
        if isinstance(allowed, list):
            now = max(0, now - len(allowed))
        if diff_mode:
            # 增量模式：diff 文件的指标值通常远小于全量基线，
            # 只检查"新增"部分（now - before 的正值），负值视为 0。
            delta = max(0, now - before)
        else:
            delta = now - before
        mark = "  " if delta <= slack else " ✗"
        slack_str = f"+{slack}" if slack else ""
        print(f"{label:<22}{before:>8}{now:>8}{slack_str:>8}{delta:>+8}{mark}")
        if delta > slack:
            failures.append(
                f"{label}：{before} → {now}（+{delta}，容错 +{slack}）"
            )

    # 名单比对：数量没变，但"哪个函数长"变了，也要能看出来
    # 增量模式下，基线是全量的，比较无意义——只检查新增
    if not diff_mode:
        base_long = set(base.get("long_fn_keys", []))
        now_long = set(current.get("long_fn_keys", []))
        allowed_long = set(allowlist.get("long_fn_keys", []))
        for key in sorted(now_long - base_long - allowed_long):
            failures.append(f"新增长函数（>100 行）：{key}")
        for key in sorted(base_long - now_long):
            print(f"  ✓ 已拆掉：{key}")

    print()
    if failures:
        print("质量闸门未通过：")
        for item in failures:
            print(f"  ✗ {item}")
        print()
        if slack:
            print(f"容错率 +{slack} 已用尽。")
        print("要么把改动改小，要么按 docs/code-quality-audit.md 的修法拆开。")
        print("确实无法避免时，在 docs/bench/quality-allowlist.json 登记豁免并写明理由。")
        return 1

    if slack and any(current.get(k, 0) > base.get(k, 0) for k, _, _ in METRICS):
        print(f"质量闸门通过（容错 +{slack} 内）。")
    else:
        print("质量闸门通过（没有指标变差）。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
