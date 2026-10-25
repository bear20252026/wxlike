"""NOTICE 一致性：`THIRD-PARTY-NOTICES.md` 是否真的覆盖了"我们实际依赖的东西"。

判据取**直接依赖**（各 crate 的 `[dependencies]` / `[build-dependencies]`），
不取全量传递依赖：NOTICE 本来就是"承重清单"，把 692 个传递包抄进去没人会读。
但直接依赖少一个就是真的漏了——那是我们自己选择引入的。

缺的项不能靠"下次补"通过：必须登记进 `notice-waivers.json`，带 owner、issue、
到期日。到期自动变红，逼着人回来处理。
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from .model import Report, Severity
from .provenance import expires_ok
from .util import DataError, load_json, read_text, repo_root

WAIVER_SCHEMA = "wxlike.notice-waivers.v1"


def direct_dependencies(root: Path) -> dict[str, set[str]]:
    """crate 名 → 依赖它的内部包（用于报告里指认"谁引入的"）。"""
    import tomllib

    out: dict[str, set[str]] = {}
    manifests = sorted(root.glob("crates/*/Cargo.toml")) + [root / "server/Cargo.toml"]
    for manifest in manifests:
        if not manifest.is_file():
            continue
        try:
            data = tomllib.loads(read_text(manifest))
        except tomllib.TOMLDecodeError as exc:
            raise DataError(f"Cargo.toml 解析失败：{manifest}: {exc}") from exc
        owner = data.get("package", {}).get("name") or manifest.parent.name
        sections = [
            data.get("dependencies") or {},
            data.get("build-dependencies") or {},
        ]
        # `[target.'cfg(unix)'.dependencies]` 也是直接依赖：漏了它，
        # 平台专属依赖（libc/tempfile 之类）会凭空"消失"，NOTICE 检查就漏人。
        for target in (data.get("target") or {}).values():
            sections.append(target.get("dependencies") or {})
            sections.append(target.get("build-dependencies") or {})
        for section in sections:
            for name, spec in section.items():
                if name.startswith("wxlike"):
                    continue
                _ = spec
                out.setdefault(name, set()).add(owner)
    return out


def load_waivers(root: Path) -> dict[str, dict[str, Any]]:
    path = root / "third_party/reviews/notice-waivers.json"
    if not path.is_file():
        return {}
    data = load_json(path)
    if data.get("schema") != WAIVER_SCHEMA:
        raise DataError(f"notice-waivers schema 必须是 {WAIVER_SCHEMA}")
    waivers = {}
    for item in data.get("waivers", []):
        crate = item.get("crate")
        if not crate:
            raise DataError("notice-waivers 每项必须有 crate")
        waivers[crate] = item
    return waivers


def check(root: Path | None = None, notice: Path | None = None) -> Report:
    root = root or repo_root()
    notice = notice or root / "THIRD-PARTY-NOTICES.md"
    report = Report("notice", meta={"notice": notice.name})
    text = read_text(notice)
    lowered = text.lower()
    deps = direct_dependencies(root)
    waivers = load_waivers(root)

    if not deps:
        report.warn("NOTICE-EMPTY", "dependencies", "没解析到任何直接依赖：门禁失去意义，先修解析", Severity.HIGH)
        return report

    missing: list[str] = []
    for crate in sorted(deps):
        pattern = re.compile(rf"(?<![\w-]){re.escape(crate.lower())}(?![\w-])")
        if pattern.search(lowered):
            continue
        missing.append(crate)
        waiver = waivers.get(crate)
        if not waiver:
            report.fail(
                "NOTICE-MISSING",
                crate,
                f"直接依赖未登记（引入方：{', '.join(sorted(deps[crate]))}）",
                Severity.HIGH,
            )
            continue
        if not waiver.get("owner") or not waiver.get("issue"):
            report.fail("NOTICE-WAIVER", crate, "豁免必须写 owner 与 issue", Severity.HIGH)
            continue
        if not expires_ok(waiver.get("expires"), "notice-waivers.expires", report, crate):
            continue
        report.exempt(
            "NOTICE-WAIVER",
            crate,
            waiver.get("reason", "已登记待补"),
            owner=waiver["owner"],
            issue=waiver["issue"],
            expires=waiver["expires"],
        )

    # 反查：NOTICE 里提到、但依赖图里根本没有的包 → 过期条目（会误导评审与用户）
    known = set(deps) | _lock_names(root)
    for stale in sorted(waivers):
        if stale not in deps:
            report.warn(
                "NOTICE-STALE-WAIVER",
                stale,
                f"豁免项已不在直接依赖里（现在依赖图中：{'是' if stale in known else '否'}）：请删除豁免",
                Severity.LOW,
            )
    report.meta.update({"direct_deps": len(deps), "missing": len(missing), "waived": len(waivers)})
    if not missing:
        report.ok("NOTICE-COVER", "dependencies", f"{len(deps)} 个直接依赖全部有登记")
    return report


def _lock_names(root: Path) -> set[str]:
    import tomllib

    lock = root / "Cargo.lock"
    if not lock.is_file():
        return set()
    data = tomllib.loads(read_text(lock))
    return {p.get("name", "") for p in data.get("package", [])}
