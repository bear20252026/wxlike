#!/usr/bin/env python3
"""守卫的守卫：ADR-28 扫描范围必须覆盖所有"async + DB"生产 crate（第四轮审计 N1）。

为什么需要这条闸：N1（admin 在 async handler 里持同步 DB 锁）之所以漏网，不是
规则不够严，而是 `crates/admin` 根本不在 `adr28_async_db_guard.py` 的 SCAN_ROOTS
里——**守卫的扫描范围本身没有守卫**。新增一个 crate 时，没人强制它登记进扫描域，
于是"守卫盲区"可以静默地再发生一次。

这条闸做三件事（fail-closed）：

1. **全集对账**：扫所有 Cargo.toml 的 `[dependencies]`（不含 dev/build），凡同时
   含 `tokio` 与 `rusqlite` 的 crate，必须在 `scripts/guard-scope-ledger.json`
   登记。未登记 → 红。这覆盖 workspace 成员，也覆盖 workspace 外的桌面壳
   （ui/src-tauri）——盲区不分内外。
2. **覆盖核实**：登记为 `covered` 的 crate，其 src 必须真的落在 ADR-28 守卫的
   SCAN_ROOTS 里、且不被 SKIP_DIR_PARTS 跳过。台账说"覆盖了"但实际没扫 → 红。
3. **豁免核实**：登记为 `exempt` 的 crate 必须给非空理由，且其 src 确实被跳过
   （豁免要与事实一致）；理由缺失 → 红。陈旧条目（台账里的 crate 已不再含
   async+DB，或 manifest 不存在）→ 红，逼着台账跟着代码走。

范围定义直接 import ADR-28 守卫本体（单一事实源），不另抄一份 SCAN_ROOTS——
抄一份就会漂移，而漂移正是本闸要防的事。

用法：
    python scripts/guard_scope_guard.py             # 扫本仓库，干净则 0
    python scripts/guard_scope_guard.py --root DIR  # 扫任意目录（注入自测用）
退出码：0 干净 / 1 有违规 / 2 守卫自己跑不起来。
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import pathlib
import re
import sys

LEDGER_REL = "scripts/guard-scope-ledger.json"
ADR28_REL = "scripts/adr28_async_db_guard.py"

# Cargo.toml 里只认生产依赖段；dev-dependencies / build-dependencies 不算
# "生产 async+DB"（测试与构建脚本不在 await 上下文里跑 handler）。
DEP_SECTION = re.compile(r"^\s*\[([^\]]+)\]\s*$")
TOKIO_DEP = re.compile(r"^\s*tokio\s*(=|\.)")
RUSQLITE_DEP = re.compile(r"^\s*rusqlite\s*(=|\.)")
NAME_LINE = re.compile(r'^\s*name\s*=\s*"([^"]+)"')


def load_adr28(root: pathlib.Path):
    path = root / ADR28_REL
    if not path.is_file():
        return None
    spec = importlib.util.spec_from_file_location("adr28_guard_for_scope", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def production_async_db_crates(root: pathlib.Path):
    """产出 (crate 名, manifest 相对路径, src 相对路径)。

    src 推断：manifest 同级的 `src/`。workspace 外的 ui/src-tauri 同样适用。
    跳过 target/ 与 node_modules/ 里的 vendored 清单。
    """
    for manifest in sorted(root.rglob("Cargo.toml")):
        posix = manifest.as_posix()
        if "/target/" in posix or "/node_modules/" in posix:
            continue
        try:
            text = manifest.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        name = None
        section = None
        has_tokio = has_rusqlite = False
        for line in text.splitlines():
            sec = DEP_SECTION.match(line)
            if sec:
                section = sec.group(1).strip().lower()
                continue
            if section == "package" and name is None:
                m = NAME_LINE.match(line)
                if m:
                    name = m.group(1)
            if section == "dependencies":
                if TOKIO_DEP.match(line):
                    has_tokio = True
                if RUSQLITE_DEP.match(line):
                    has_rusqlite = True
        if name and has_tokio and has_rusqlite:
            rel_manifest = manifest.relative_to(root).as_posix()
            src = (manifest.parent / "src").relative_to(root).as_posix()
            yield name, rel_manifest, src


def is_scanned(adr28, src: str) -> bool:
    """src 是否真的被 ADR-28 守卫扫到（在 SCAN_ROOTS 内且不被 SKIP 跳过）。"""
    under_root = any(src == r or src.startswith(r + "/") for r in adr28.SCAN_ROOTS)
    skipped = any(part in ("/" + src + "/") for part in adr28.SKIP_DIR_PARTS)
    return under_root and not skipped


def check(root: pathlib.Path):
    adr28 = load_adr28(root)
    if adr28 is None:
        return [f"找不到 ADR-28 守卫本体：{ADR28_REL}（范围定义无从核对）"]
    ledger_path = root / LEDGER_REL
    if not ledger_path.is_file():
        return [f"缺少扫描范围台账：{LEDGER_REL}"]
    try:
        ledger = json.loads(ledger_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as err:
        return [f"台账读不出来：{LEDGER_REL}: {err}"]
    entries = ledger.get("crates")
    if not isinstance(entries, list):
        return [f"台账形状不对：{LEDGER_REL} 缺 crates 数组"]

    by_name = {}
    problems = []
    for entry in entries:
        name = entry.get("crate")
        if not name:
            problems.append(f"台账有一条缺 crate 名：{entry}")
            continue
        if name in by_name:
            problems.append(f"台账重复登记：{name}")
        by_name[name] = entry

    actual = {name: (manifest, src) for name, manifest, src in production_async_db_crates(root)}

    # 1. 全集对账：每个 async+DB crate 必须登记。
    for name, (manifest, src) in sorted(actual.items()):
        if name not in by_name:
            problems.append(
                f"{name}（{manifest}）生产依赖同时含 tokio+rusqlite，但未在 "
                f"{LEDGER_REL} 登记——新增 crate 必须声明 covered 或 exempt，不许静默漏扫"
            )

    # 2/3. 逐条核实台账与事实一致。
    for name, entry in sorted(by_name.items()):
        status = entry.get("status")
        src = entry.get("src")
        manifest = entry.get("manifest")
        if name not in actual:
            problems.append(
                f"台账陈旧：{name} 已不再含生产 async+DB（或 {manifest} 不存在），请删登记"
            )
            continue
        real_manifest, real_src = actual[name]
        if manifest != real_manifest or src != real_src:
            problems.append(
                f"台账与事实不符：{name} 登记 manifest={manifest} src={src}，"
                f"实际 manifest={real_manifest} src={real_src}"
            )
            continue
        scanned = is_scanned(adr28, real_src)
        if status == "covered":
            if not scanned:
                problems.append(
                    f"{name} 台账说 covered，但 {real_src} 不在 ADR-28 SCAN_ROOTS 内"
                    "或被 SKIP_DIR_PARTS 跳过——把它加进扫描域，或改 exempt 并给理由"
                )
        elif status == "exempt":
            reason = (entry.get("reason") or "").strip()
            if not reason:
                problems.append(f"{name} 登记 exempt 但没给理由——豁免必须可追责")
            if scanned:
                problems.append(
                    f"{name} 台账说 exempt，但 {real_src} 实际已被扫描——"
                    "豁免与事实不符：改 covered 并删理由"
                )
        else:
            problems.append(f"{name} 的 status={status!r} 不认识（只能 covered / exempt）")

    return problems


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="守卫的守卫：ADR-28 扫描范围覆盖所有 async+DB crate")
    parser.add_argument("--root", default=None, help="扫描根目录（默认仓库根）")
    args = parser.parse_args(argv)
    root = pathlib.Path(args.root).resolve() if args.root else pathlib.Path(__file__).resolve().parents[1]
    if not root.is_dir():
        print(f"目录不存在：{root}", file=sys.stderr)
        return 2
    problems = check(root)
    for line in problems:
        print(f"扫描范围违规：{line}", file=sys.stderr)
    n = len(list(production_async_db_crates(root)))
    print(f"扫描范围守卫：{n} 个 async+DB crate，违规 {len(problems)} 条")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())