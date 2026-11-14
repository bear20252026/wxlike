#!/usr/bin/env python3
"""工作区依赖图守卫：循环依赖 / 分层方向 / 孤儿模块（架构审计 2026-11-14 的缺口）。

为什么要这条闸：审计发现「模块边界有编译期分层锁，但**循环依赖与孤儿 crate
没有自动化检测**」——cargo-deny 的 bans 只是 warn，`cargo metadata --locked`
只验 lock 闭合。依赖方向单向靠人工看 Cargo.toml，时间一长就会悄悄漂移。

三条规则（只看**生产**依赖，dev/build 依赖不参与——测试驱动平台打真 hub 是
合法的 dev-dep，不算分层违规）：

1. `cycle`（硬）：workspace 成员之间的生产依赖图里出现环 → 违规。
2. `layering`（硬）：`forbidden_edges` 里登记的方向出现生产依赖 → 违规。
   当前钉死 `wxlike-platform → wxlike-net`：业务数据面不许依赖传输层，
   事件经 `trait EventSink` 反向注入（见 platform/lib.rs）。
3. `orphan`（基线只降）：无生产入边的成员视为孤儿候选。合法叶子（独立入口
   bin / FFI / 被排除的 ui 等）登记在 `baseline.json`；**新增**孤儿（不在基线）
   → 违规。基线只许收紧：移除一个孤儿后不许再悄悄加回来。

用法：
    python scripts/dep_graph_guard.py            # 扫本仓库，干净则 0
    python scripts/dep_graph_guard.py --update   # 把当前孤儿集写入基线（只许变小）
    python scripts/dep_graph_guard.py --metadata FILE  # 用已有 cargo metadata JSON（自测用）
退出码：0 干净 / 1 有违规 / 2 守卫自己跑不起来。
"""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
BASELINE = ROOT / "scripts" / "dep_graph_baseline.json"

# 分层方向硬约束：key 不许在生产依赖里出现 value。
FORBIDDEN_EDGES = {
    "wxlike-platform": {"wxlike-net"},
}

# 入口 crate：天然没有生产入边，但不算孤儿（它们是组合根/独立产物）。
ROOT_CRATES = {"wxlike-server"}


def load_metadata(metadata_file: str | None) -> dict:
    if metadata_file:
        return json.loads(pathlib.Path(metadata_file).read_text(encoding="utf-8-sig"))
    proc = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
        cwd=ROOT, capture_output=True, text=True,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(2)
    return json.loads(proc.stdout)


def build_graph(meta: dict):
    members = set(meta["workspace_members"])
    by_id = {p["id"]: p for p in meta["packages"]}
    names = {by_id[m]["name"] for m in members if m in by_id}
    adj: dict[str, set[str]] = {n: set() for n in names}
    for m in members:
        p = by_id.get(m)
        if not p:
            continue
        for d in p["dependencies"]:
            # 只算生产依赖；dev/build 依赖（kind != normal）不参与分层与环判定。
            if d.get("kind") not in (None, "normal"):
                continue
            if d["name"] in names:
                adj[p["name"]].add(d["name"])
    return names, adj


def find_cycles(names, adj):
    color: dict[str, int] = {}
    cycles: list[list[str]] = []

    def visit(n, path):
        color[n] = 1
        for x in sorted(adj[n]):
            if color.get(x) == 1:
                cycles.append(path[path.index(x):] + [x])
            elif x not in color:
                visit(x, path + [x])
        color[n] = 2

    for n in sorted(names):
        if n not in color:
            visit(n, [n])
    return cycles


def find_orphans(names, adj):
    incoming = {n: 0 for n in names}
    for n in names:
        for x in adj[n]:
            incoming[x] += 1
    return {n for n in names if incoming[n] == 0 and n not in ROOT_CRATES}


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="工作区依赖图：环 / 分层方向 / 孤儿")
    parser.add_argument("--metadata", help="已有 cargo metadata JSON（自测用）")
    parser.add_argument("--update", action="store_true", help="把当前孤儿集写入基线（只许变小）")
    args = parser.parse_args(argv)

    try:
        meta = load_metadata(args.metadata)
    except (SystemExit, FileNotFoundError, json.JSONDecodeError) as e:
        print(f"守卫跑不起来：{e}", file=sys.stderr)
        return 2

    names, adj = build_graph(meta)
    findings: list[str] = []

    # 规则 1：环
    for cyc in find_cycles(names, adj):
        findings.append("cycle: " + " -> ".join(cyc))

    # 规则 2：分层方向
    for src, forbidden in FORBIDDEN_EDGES.items():
        for dst in sorted(adj.get(src, set()) & forbidden):
            findings.append(f"layering: {src} -> {dst}（生产依赖违反分层方向）")

    # 规则 3：孤儿基线只降
    orphans = find_orphans(names, adj)
    baseline = set()
    if BASELINE.exists():
        baseline = set(json.loads(BASELINE.read_text(encoding="utf-8")).get("orphans", []))

    if args.update:
        if baseline and not orphans <= baseline:
            added = sorted(orphans - baseline)
            print(f"--update 拒绝变差：新增孤儿 {added}", file=sys.stderr)
            return 1
        BASELINE.write_text(
            json.dumps({"orphans": sorted(orphans)}, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
        print(f"基线已更新：{len(orphans)} 个孤儿（只许收紧）")
        return 0

    new_orphans = sorted(orphans - baseline)
    for n in new_orphans:
        findings.append(f"orphan: {n} 无生产入边且不在基线（接线或登记 baseline）")

    if findings:
        print("依赖图守卫未通过：")
        for f in findings:
            print(f"  ✗ {f}")
        return 1
    print(f"依赖图干净：{len(names)} 成员，无环，分层方向合规，孤儿 {len(orphans)} 个均在基线。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
