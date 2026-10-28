"""来源审查：上游仓 → 片段 → 本地文件的三段式登记核对。

三段式是重点。只登记仓库（"我们参考了 Signal"）没有意义——签收单上必须能
回答"这一行代码是从哪个 commit 的哪个文件来的、改了什么"。所以 registry
同时记 `upstreams`（仓级）和 `files`（片段级），门禁逐条核对：

* 仓级：commit 必须是完整 SHA（40/64 位），url 必须是 https 永久地址；
* 片段级：`ported`/`adapted` 必须能在本地按 sha256 复现，必须有 patchlog；
* 排除项：`excluded` 必须命中 `exclusions.json` 里的规则，不能一句"不需要"。

许可证不是判据（G00 决定：本项目只做内部研究、不对外分发）。registry 里的
`license_spdx` / `license_evidence` 只剩"有就写上"的信息位，缺了不红。
这里核的是**来源能不能复查**：代码改了但登记没更新，是会让排查断掉的真问题。
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .model import Report, Severity
from .util import (
    DataError,
    is_hex,
    load_json,
    parse_date,
    read_text,
    repo_root,
    resolve_inside,
    sha256_file,
    today,
)

SCHEMA = "wxlike.provenance.v1"
PORTED = {"ported", "adapted"}
DECISIONS = PORTED | {"behavior_reference", "excluded"}


@dataclass
class Registry:
    upstreams: dict[str, dict[str, Any]]
    files: list[dict[str, Any]]
    raw: dict[str, Any]


def load_registry(path: Path) -> Registry:
    data = load_json(path)
    if not isinstance(data, dict) or data.get("schema") != SCHEMA:
        raise DataError(f"registry schema 必须是 {SCHEMA}，实际 {data.get('schema')!r}")
    upstreams: dict[str, dict[str, Any]] = {}
    for entry in data.get("upstreams", []):
        if not isinstance(entry, dict) or "id" not in entry:
            raise DataError("upstreams 里每项必须有 id")
        if entry["id"] in upstreams:
            raise DataError(f"upstream id 重复：{entry['id']}")
        upstreams[entry["id"]] = entry
    files = data.get("files", [])
    if not isinstance(files, list):
        raise DataError("files 必须是数组")
    return Registry(upstreams, files, data)


def permalink(upstream: dict[str, Any], upstream_path: str) -> str:
    """`…/blob/<full-sha>/<path>` —— 永不过期的定位方式（相对分支名而言）。"""
    base = upstream.get("url", "").rstrip("/")
    commit = upstream.get("commit", "")
    return f"{base}/blob/{commit}/{upstream_path.lstrip('/')}"


def check(root: Path | None = None, registry_path: Path | None = None) -> Report:
    root = root or repo_root()
    registry_path = registry_path or root / "third_party/reviews/registry.json"
    reg = load_registry(registry_path)
    exclusions = _load_exclusions(root)
    report = Report("provenance", meta={"registry": str(registry_path.relative_to(root))})

    _check_upstreams(report, reg)
    _check_files(report, root, reg, exclusions)

    if not reg.files:
        report.warn(
            "PROV-EMPTY",
            "registry.files",
            "片段登记为空：本仓库只要有 vendored 代码就必须登记（空表等于漏登记）",
            Severity.HIGH,
        )
    return report


def _load_exclusions(root: Path) -> dict[str, dict[str, Any]]:
    data = load_json(root / "third_party/reviews/exclusions.json")
    rules = data.get("rules", [])
    if not isinstance(rules, list):
        raise DataError("exclusions.json 的 rules 必须是数组")
    return {r["id"]: r for r in rules if "id" in r}


def _check_upstreams(report: Report, reg: Registry) -> None:
    for up_id, up in reg.upstreams.items():
        commit_ok = is_hex(up.get("commit", ""))
        if not commit_ok:
            report.fail(
                "PROV-UP-SHA",
                up_id,
                f"commit 必须是完整 SHA：{up.get('commit')!r}",
                Severity.HIGH,
            )
        url_ok = str(up.get("url", "")).startswith("https://")
        if not url_ok:
            report.fail("PROV-UP-URL", up_id, "url 必须是 https 永久地址", Severity.MEDIUM)
        # 许可不再作判据：`license_spdx` / `license_evidence` 有就带上、没有也不红。
        if commit_ok and url_ok:
            report.ok("PROV-UP-OK", up_id, f"{up.get('repo') or up.get('url', '')} @ {str(up.get('commit', ''))[:8]}")


def _check_files(
    report: Report,
    root: Path,
    reg: Registry,
    exclusions: dict[str, dict[str, Any]],
) -> None:
    seen: set[str] = set()
    for idx, entry in enumerate(reg.files):
        subject = (
            entry.get("path")
            or (f"{entry.get('upstream')}:{entry['upstream_path']}" if entry.get("upstream_path") else None)
            or f"files[{idx}]"
        )
        if subject in seen:
            report.fail("PROV-DUP", subject, "同一路径登记了两次", Severity.MEDIUM)
            continue
        seen.add(subject)
        decision = entry.get("decision")
        if decision not in DECISIONS:
            report.fail(
                "PROV-DECISION",
                subject,
                f"decision 必须是 {sorted(DECISIONS)} 之一，实际 {decision!r}",
                Severity.HIGH,
            )
            continue
        up_id = entry.get("upstream")
        if up_id not in reg.upstreams:
            report.fail("PROV-UP-MISSING", subject, f"upstream {up_id!r} 未在 upstreams 登记", Severity.HIGH)
            continue
        up = reg.upstreams[up_id]
        if decision == "excluded":
            _check_excluded(report, entry, subject, exclusions)
        elif decision == "behavior_reference":
            if not entry.get("notes"):
                report.fail("PROV-BREF-NOTE", subject, "行为参考必须写 notes（借了什么、没借什么）", Severity.LOW)
            else:
                report.ok("PROV-BREF", subject, "行为参考，无代码复制")
        else:
            _check_ported(report, root, entry, up, subject)


def _check_excluded(report: Report, entry: dict[str, Any], subject: str, exclusions: dict[str, Any]) -> None:
    rule_id = entry.get("rule")
    if rule_id not in exclusions:
        report.fail(
            "PROV-EXCL-RULE",
            subject,
            f"排除项必须引用 exclusions.json 里的规则 id，实际 {rule_id!r}",
            Severity.HIGH,
        )
        return
    if not entry.get("reason"):
        report.fail("PROV-EXCL-REASON", subject, "排除项必须写 reason（为什么不能用）", Severity.MEDIUM)
        return
    report.exempt(
        "PROV-EXCL",
        subject,
        f"按规则 {rule_id} 排除：{exclusions[rule_id].get('note', '')}",
        owner=entry.get("owner", "G17"),
        issue=entry.get("issue", "n/a"),
        expires=entry.get("expires", "2099-12-31"),
    )


def _check_ported(
    report: Report,
    root: Path,
    entry: dict[str, Any],
    upstream: dict[str, Any],
    subject: str,
) -> None:
    # 1) 本地可复现：路径必须在仓库内、必须存在、sha256 必须一致
    try:
        target = resolve_inside(root, entry["path"])
    except Exception as exc:  # UsageError 也算不合格登记
        report.fail("PROV-PATH", subject, f"路径不合格：{exc}", Severity.HIGH)
        return
    if not target.is_file():
        report.fail("PROV-MISSING", subject, "登记了 vendored 片段，但文件不存在", Severity.CRITICAL)
        return
    claimed = entry.get("local_sha256", "")
    actual = sha256_file(target)
    if claimed != actual:
        report.fail(
            "PROV-SHA-MISMATCH",
            subject,
            f"sha256 不符（登记 {claimed[:12]}… 实际 {actual[:12]}…）：代码改了但登记没更新",
            Severity.CRITICAL,
        )
    else:
        report.ok("PROV-SHA", subject, f"sha256 {actual[:12]}… 一致")

    # 2) 上游定位：完整 SHA + 路径 → 永久链接
    commit = entry.get("commit") or upstream.get("commit", "")
    if not entry.get("upstream_path"):
        report.fail("PROV-UP-PATH", subject, "缺 upstream_path：无法生成永久链接", Severity.HIGH)
    elif not is_hex(commit):
        report.fail("PROV-UP-SHA", subject, f"commit 必须是完整 SHA：{commit!r}", Severity.HIGH)
    else:
        link = permalink({**upstream, "commit": commit}, entry["upstream_path"])
        report.ok("PROV-LINK", subject, link)

    # 3) 改动说明：必须有，且不能是空壳
    patchlog = entry.get("patchlog")
    if not patchlog:
        report.fail("PROV-PATCHLOG", subject, "缺 patchlog：无法区分「原样复制」和「改过」", Severity.HIGH)
    else:
        try:
            log_path = resolve_inside(root, patchlog)
        except Exception as exc:
            report.fail("PROV-PATCHLOG", subject, f"patchlog 路径不合格：{exc}", Severity.HIGH)
            log_path = None
        if log_path is not None:
            if not log_path.is_file():
                report.fail("PROV-PATCHLOG", subject, f"patchlog 不存在：{patchlog}", Severity.HIGH)
            elif len(read_text(log_path).strip().splitlines()) < 3:
                report.fail("PROV-PATCHLOG", subject, f"patchlog 过于空泛：{patchlog}", Severity.MEDIUM)


def expires_ok(value: str | None, field: str, report: Report, subject: str) -> bool:
    """例外共用判定：必须有 owner、issue、未到期。"""
    if not value:
        report.fail("EXC-NO-EXPIRY", subject, f"{field} 缺到期日：例外不能永久", Severity.HIGH)
        return False
    try:
        if parse_date(value) < today():
            report.fail("EXC-EXPIRED", subject, f"{field} 已于 {value} 过期，必须重新评审", Severity.HIGH)
            return False
    except DataError as exc:
        report.fail("EXC-BAD-DATE", subject, str(exc), Severity.MEDIUM)
        return False
    return True
