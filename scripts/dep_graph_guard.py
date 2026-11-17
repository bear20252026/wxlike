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
4. `unwired`（第三轮审计 2026-11-17：测而未接）：有测试的成员 crate，若不在
   任何**生产入口**（server/admin/cli/mcp/ffi/sfu + 工作区外的桌面宿主 ui）
   的生产依赖闭包内，就是「孤岛」——测试全绿但没有任何线上路径用到它。
   孤岛必须在 `wiring-baseline.json` 显式登记 SEAM（owner/task/close_when/
   evidence），否则违规；已接线的不许再登记（陈旧条目也违规）。基线只许
   收紧：接线一个就移除一个，不许悄悄加回。孤儿规则管「有没有人依赖」，
   本规则管「依赖它的是不是真实产品入口」——只被另一个孤岛依赖不算接线。

用法：
    python scripts/dep_graph_guard.py            # 扫本仓库，干净则 0
    python scripts/dep_graph_guard.py --update   # 把当前孤儿集写入基线（只许变小）
    python scripts/dep_graph_guard.py --update-wiring  # 登记当前孤岛 SEAM（只许变小）
    python scripts/dep_graph_guard.py --metadata FILE  # 用已有 cargo metadata JSON（自测用）
退出码：0 干净 / 1 有违规 / 2 守卫自己跑不起来。
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
BASELINE = ROOT / "scripts" / "dep_graph_baseline.json"
WIRING_BASELINE = ROOT / "scripts" / "wiring-baseline.json"

# 分层方向硬约束：key 不许在生产依赖里出现 value。
FORBIDDEN_EDGES = {
    "wxlike-platform": {"wxlike-net"},
}

# 入口 crate：天然没有生产入边，但不算孤儿（它们是组合根/独立产物）。
ROOT_CRATES = {"wxlike-server"}

# 生产入口（规则 4）：真实产品/工具的组合根。一个 crate 只有被这些入口的
# 生产依赖闭包覆盖才算「接线」；只被另一个孤岛依赖不算。
ENTRY_CRATES = {
    "wxlike-server",
    "wxlike-admin",
    "wxlike-cli",
    "wxlike-mcp",
    "wxlike-ffi",
    "wxlike-sfu",
}

# 工作区外的生产入口：桌面宿主（Tauri）。它不在 cargo workspace 里，
# 直接解析其 Cargo.toml 的 [dependencies] 中指向本仓库的 path 依赖。
EXTERNAL_ENTRIES = (ROOT / "ui" / "src-tauri" / "Cargo.toml",)

_TEST_ATTR = re.compile(r"#\s*\[(?:tokio::)?test\]")


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
    manifests: dict[str, pathlib.Path] = {}
    for m in members:
        p = by_id.get(m)
        if not p:
            continue
        if p.get("manifest_path"):
            manifests[p["name"]] = pathlib.Path(p["manifest_path"]).parent
        for d in p["dependencies"]:
            # 只算生产依赖；dev/build 依赖（kind != normal）不参与分层与环判定。
            if d.get("kind") not in (None, "normal"):
                continue
            if d["name"] in names:
                adj[p["name"]].add(d["name"])
    return names, adj, manifests


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


def external_entry_deps(names) -> set[str]:
    """工作区外生产入口（桌面宿主）指向本仓库成员的生产 path 依赖。

    只读 [dependencies] 段；dev/build 依赖不算接线。解析失败 → 守卫退出 2
    （fail-closed：入口清单读不了就不能宣称接线干净）。
    """
    deps: set[str] = set()
    for manifest in EXTERNAL_ENTRIES:
        if not manifest.is_file():
            raise FileNotFoundError(f"生产入口清单缺失：{manifest}")
        section = None
        for line in manifest.read_text(encoding="utf-8").splitlines():
            stripped = line.strip()
            if stripped.startswith("["):
                section = stripped
                continue
            if section != "[dependencies]" or not stripped or stripped.startswith("#"):
                continue
            name = stripped.split("=", 1)[0].strip()
            if name in names and "path" in stripped:
                deps.add(name)
    return deps


def reachable_from(entries, adj) -> set[str]:
    seen: set[str] = set()
    stack = [e for e in entries if e in adj]
    while stack:
        n = stack.pop()
        if n in seen:
            continue
        seen.add(n)
        stack.extend(adj[n] - seen)
    return seen


def count_tests(crate_dir: pathlib.Path) -> int:
    """src/ 与 tests/ 下的 #[test]/#[tokio::test] 计数（含内联测试模块）。"""
    total = 0
    for pattern in ("src/**/*.rs", "tests/**/*.rs"):
        for f in crate_dir.glob(pattern):
            total += len(_TEST_ATTR.findall(f.read_text(encoding="utf-8", errors="replace")))
    return total


def find_unwired(names, adj, manifests) -> dict[str, int]:
    """有测试但不在任何生产入口闭包内的成员 → {crate: 测试数}。"""
    entries = (ENTRY_CRATES & names) | external_entry_deps(names)
    wired = reachable_from(entries, adj)
    unwired = {}
    # 入口自身不算孤岛（它们是产物，不是待接线的域）。
    for n in sorted(names - wired - ENTRY_CRATES):
        d = manifests.get(n)
        if d is None or not d.is_dir():
            # 合成 metadata（自测）没有真实目录：无测试可数，不算孤岛。
            continue
        tests = count_tests(d)
        if tests > 0:
            unwired[n] = tests
    return unwired


def load_wiring_ledger(path: pathlib.Path | None = None) -> dict:
    path = path or WIRING_BASELINE
    if not path.exists():
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    return {e["crate"]: e for e in data.get("seams", [])}


def check_wiring(unwired: dict[str, int], ledger: dict) -> list[str]:
    findings = []
    for n, tests in sorted(unwired.items()):
        entry = ledger.get(n)
        if entry is None:
            findings.append(
                f"unwired: {n} 有 {tests} 个测试但无任何生产入口依赖"
                "（接线，或 --update-wiring 登记 SEAM）"
            )
            continue
        for field in ("owner", "task", "close_when", "evidence"):
            if not str(entry.get(field, "")).strip():
                findings.append(f"unwired: {n} 的 SEAM 登记缺少 {field}（fail-closed）")
    for n in sorted(set(ledger) - set(unwired)):
        findings.append(f"unwired: {n} 已接线或不存在，SEAM 登记应移除（基线只许收紧）")
    return findings


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="工作区依赖图：环 / 分层方向 / 孤儿 / 孤岛接线")
    parser.add_argument("--metadata", help="已有 cargo metadata JSON（自测用）")
    parser.add_argument("--update", action="store_true", help="把当前孤儿集写入基线（只许变小）")
    parser.add_argument("--update-wiring", action="store_true",
                        help="把当前孤岛集登记为 SEAM 基线（只许变小；新增条目须补 owner/task/close_when/evidence）")
    parser.add_argument("--wiring-baseline", help="SEAM 台账路径覆盖（自测隔离用）")
    args = parser.parse_args(argv)
    wiring_path = pathlib.Path(args.wiring_baseline) if args.wiring_baseline else WIRING_BASELINE

    try:
        meta = load_metadata(args.metadata)
    except (SystemExit, FileNotFoundError, json.JSONDecodeError) as e:
        print(f"守卫跑不起来：{e}", file=sys.stderr)
        return 2

    names, adj, manifests = build_graph(meta)
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

    # 规则 4：孤岛接线（测而未接必须显式登记 SEAM）
    try:
        unwired = find_unwired(names, adj, manifests)
        ledger = load_wiring_ledger(wiring_path)
    except (FileNotFoundError, json.JSONDecodeError, KeyError) as e:
        print(f"守卫跑不起来：{e}", file=sys.stderr)
        return 2

    if args.update_wiring:
        if ledger and not set(unwired) <= set(ledger):
            added = sorted(set(unwired) - set(ledger))
            print(f"--update-wiring 拒绝变差：新增孤岛 {added}（先补 SEAM 登记再收紧）", file=sys.stderr)
            return 1
        seams = [ledger[n] for n in sorted(unwired)]
        wiring_path.write_text(
            json.dumps({"seams": seams}, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
        print(f"接线基线已更新：{len(seams)} 个 SEAM（只许收紧）")
        return 0

    findings.extend(check_wiring(unwired, ledger))

    if findings:
        print("依赖图守卫未通过：")
        for f in findings:
            print(f"  ✗ {f}")
        return 1
    print(
        f"依赖图干净：{len(names)} 成员，无环，分层方向合规，"
        f"孤儿 {len(orphans)} 个均在基线，孤岛 {len(unwired)} 个均已登记 SEAM。"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
