"""发布评审门禁：证据包 → pass / block（G17-05）。

这个门禁要防的不是"有 bug"，而是**用清单冒充验证**：
"安全清单全部勾选"和"这些东西真的被验过"是两件事。所以每条证据都要带
`kind`（怎么验的）与 `evidence`（验的证据在哪），`required` 的证据非 pass 即阻断。

三条硬规则：

1. **必需项非 pass 即阻断。** 不允许"unverified 但先发"。
2. **声明必须被证据覆盖。** 发布声明里列的每个支持平台，都要在 verified 列表里
   出现一次；不出现就是"说了没验"。
3. **阻塞必须可追责。** 每条 open blocker 要 owner + issue；高危还想放行必须有
   带到期日的例外。到期即阻断，不靠人记得。
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from .model import Report, Severity
from .provenance import expires_ok
from .util import DataError, load_json, repo_root

EVIDENCE_SCHEMA = "wxlike.release-evidence.v1"
BLOCKERS_SCHEMA = "wxlike.release-blockers.v1"
PASS, FAIL, BLOCKED, NOT_APPLICABLE, UNVERIFIED = "pass", "fail", "blocked", "not_applicable", "unverified"
STATUSES = {PASS, FAIL, BLOCKED, NOT_APPLICABLE, UNVERIFIED}
# 证据强度：`checklist` 是自报，不算验证；`test`/`artifact`/`device` 才是
KINDS = {"checklist", "test", "artifact", "device", "external"}
STRONG_KINDS = {"test", "artifact", "device"}


def check(root: Path | None = None, evidence: Path | None = None) -> Report:
    root = root or repo_root()
    evidence_path = evidence or root / "security/release-reviews/evidence/latest.json"
    bundle = load_json(evidence_path)
    if bundle.get("schema") != EVIDENCE_SCHEMA:
        raise DataError(f"evidence schema 必须是 {EVIDENCE_SCHEMA}")
    blockers = _load_blockers(root)
    exceptions = {e["id"]: e for e in bundle.get("exceptions", [])}

    report = Report(
        "release",
        meta={
            "release": (bundle.get("release") or {}).get("name", "?"),
            "reviewed_at": (bundle.get("release") or {}).get("reviewed_at", "?"),
            "evidence": str(evidence_path.relative_to(root)).replace("\\", "/"),
        },
    )

    _check_items(report, bundle, exceptions)
    _check_claims(report, bundle)
    _check_blockers(report, blockers, exceptions)
    _check_corpus_cross_reference(report, root, blockers)
    _summarize(report, bundle, blockers)
    return report


# ---- 证据项 -------------------------------------------------------------
def _check_items(report: Report, bundle: dict[str, Any], exceptions: dict[str, Any]) -> None:
    items = bundle.get("items") or []
    if not items:
        raise DataError("证据包没有任何 item：空清单不是通过")
    for item in items:
        subject = item.get("id", "<无 id>")
        status = item.get("status")
        required = bool(item.get("required", True))
        if status not in STATUSES:
            report.fail("REL-STATUS", subject, f"status 非法：{status!r}", Severity.HIGH)
            continue
        kind = item.get("kind", "checklist")
        if kind not in KINDS:
            report.fail("REL-KIND", subject, f"kind 非法：{kind!r}", Severity.MEDIUM)
            continue
        # 自报（checklist）不能当作验证证据，即使它写着 pass
        if status == PASS and kind == "checklist" and required:
            report.fail(
                "REL-SELF-REPORT",
                subject,
                "必需项用 checklist 自报就算通过：清单不能替代测试/产物/设备证据",
                Severity.HIGH,
            )
            continue
        if status == PASS:
            if not item.get("evidence"):
                report.fail("REL-NO-EVIDENCE", subject, "pass 但没有 evidence（证据在哪）", Severity.HIGH)
            else:
                report.ok("REL-PASS", subject, f"{kind}: {item['evidence']}")
            continue
        if status == NOT_APPLICABLE:
            if not item.get("reason"):
                report.fail("REL-NA-REASON", subject, "not_applicable 必须写 reason", Severity.MEDIUM)
            else:
                report.ok("REL-NA", subject, item["reason"])
            continue
        # fail / blocked / unverified
        exc = exceptions.get(subject)
        if exc:
            if not exc.get("owner") or not exc.get("issue"):
                report.fail("REL-EXC", subject, "例外缺 owner/issue", Severity.HIGH)
                continue
            if not expires_ok(exc.get("expires"), "release exceptions.expires", report, subject):
                continue
            report.exempt(
                "REL-EXEMPT",
                subject,
                f"{status}：{exc.get('reason', '')}",
                owner=exc["owner"],
                issue=exc["issue"],
                expires=str(exc["expires"]),
            )
            continue
        severity = Severity.HIGH if required else Severity.MEDIUM
        code = "REL-REQUIRED-FAIL" if required else "REL-OPTIONAL-FAIL"
        message = f"{status}：{item.get('notes', '未说明')}"
        owner, issue = item.get("owner"), item.get("issue")
        # 报告里必须能直接看到"谁去修、哪个 issue"——否则评审记录就是一张写着"未通过"的纸
        if required:
            report.fail(code, subject, message, severity, owner=owner, issue=issue)
        else:
            report.warn(code, subject, message, severity, owner=owner, issue=issue)


# ---- 发布声明 -----------------------------------------------------------
def _check_claims(report: Report, bundle: dict[str, Any]) -> None:
    claims = (bundle.get("claims") or {}).get("platforms") or []
    verified = {p["name"] for p in bundle.get("platforms", []) if p.get("verified")}
    for claimed in claims:
        if claimed not in verified:
            report.fail(
                "REL-CLAIM-UNVERIFIED",
                claimed,
                "发布声明里列了这个平台，但 verified 列表里没有它（说了没验）",
                Severity.HIGH,
            )
    for platform in bundle.get("platforms", []):
        if platform.get("verified") and not platform.get("evidence"):
            report.fail(
                "REL-PLATFORM-EVIDENCE",
                platform.get("name", "?"),
                "标 verified 但没有 evidence（自报不算）",
                Severity.HIGH,
            )


# ---- 阻塞项 -------------------------------------------------------------
def _load_blockers(root: Path) -> list[dict[str, Any]]:
    path = root / "security/release-reviews/blockers.json"
    data = load_json(path)
    if data.get("schema") != BLOCKERS_SCHEMA:
        raise DataError(f"blockers schema 必须是 {BLOCKERS_SCHEMA}")
    return list(data.get("blockers", []))


def _check_blockers(report: Report, blockers: list[dict[str, Any]], exceptions: dict[str, Any]) -> None:
    if not blockers:
        report.warn("REL-NO-BLOCKERS", "blockers", "阻塞列表为空：一个刚起步的项目没有阻塞项，先确认是不是漏登记", Severity.LOW)
        return
    for blocker in blockers:
        bid = blocker.get("id", "<无 id>")
        status = blocker.get("status", "open")
        if status == "closed":
            if not blocker.get("closed_evidence"):
                report.fail("REL-CLOSED-NO-EVIDENCE", bid, "标 closed 但没有 closed_evidence", Severity.HIGH)
            continue
        severity = Severity.parse(blocker.get("severity"))
        for field in ("owner", "issue"):
            if not blocker.get(field):
                report.fail("REL-BLOCKER-UNTRACKED", bid, f"阻塞项缺 {field}：没人认领的阻塞不是阻塞", Severity.HIGH)
        exc = exceptions.get(bid)
        if exc and exc.get("owner") and exc.get("issue"):
            if expires_ok(exc.get("expires"), "release exceptions.expires", report, bid):
                report.exempt("REL-BLOCKER-EXEMPT", bid, exc.get("reason", ""), exc["owner"], exc["issue"], str(exc["expires"]))
            continue
        if blocker.get("external"):
            # 外部阻塞：不计完成，但仍然阻断发布（不能用"不归我管"换放行）
            report.fail(
                "REL-BLOCKER-EXTERNAL",
                bid,
                f"{blocker.get('title', '')}（外部阻塞，不计完成）",
                severity,
                owner=blocker.get("owner"),
                issue=blocker.get("issue"),
            )
        else:
            report.fail(
                "REL-BLOCKER-OPEN",
                bid,
                blocker.get("title", ""),
                severity,
                owner=blocker.get("owner"),
                issue=blocker.get("issue"),
            )


# ---- 语料与阻塞的交叉核对 -----------------------------------------------
def _check_corpus_cross_reference(report: Report, root: Path, blockers: list[dict[str, Any]]) -> None:
    """语料里的 blocked 样本必须在 blockers.json 里有同名条目。

    否则就存在一条"能打崩解析器但没人认领"的路——那正是发布评审最该拦的东西。
    """
    manifest = root / "tests/security/corpus/manifest.json"
    if not manifest.is_file():
        report.warn("REL-CORPUS-MISSING", "corpus", "没有语料清单：解码面证据不完整", Severity.LOW)
        return
    data = load_json(manifest)
    known = {b.get("id"): b for b in blockers}
    for sample in data.get("samples", []):
        if sample.get("verdict") != "blocked":
            continue
        blocker_id = sample.get("blocker")
        if blocker_id not in known:
            report.fail(
                "REL-CORPUS-UNTRACKED",
                sample.get("file", "?"),
                f"语料里的崩溃样本引用了 {blocker_id!r}，但 blockers.json 里没有这条",
                Severity.HIGH,
            )
        elif known[blocker_id].get("status") == "closed":
            report.fail(
                "REL-CORPUS-CLOSED",
                sample.get("file", "?"),
                f"语料仍为 blocked，但 {blocker_id} 已标 closed：须重放成功并更新裁决",
                Severity.HIGH,
            )


def _summarize(report: Report, bundle: dict[str, Any], blockers: list[dict[str, Any]]) -> None:
    open_blockers = [b for b in blockers if b.get("status", "open") != "closed"]
    report.meta.update(
        {
            "items": len(bundle.get("items", [])),
            "required_items": sum(1 for i in bundle.get("items", []) if i.get("required", True)),
            "open_blockers": len(open_blockers),
            "critical_high_open": sum(
                1 for b in open_blockers if Severity.parse(b.get("severity")) in (Severity.CRITICAL, Severity.HIGH)
            ),
            "verdict": "BLOCK" if report.blocking else "PASS",
        }
    )


def snapshot(root: Path) -> str:
    """生成发布用的来源快照（人读）。`third_party/reviews/release/<date>.md`。"""
    registry = load_json(root / "third_party/reviews/registry.json")
    sbom = load_json(root / "third_party/reviews/sbom/wxlike.cdx.json")
    lines = ["## 复制进仓库的第三方代码", ""]
    for entry in registry.get("files", []):
        if entry.get("decision") in {"ported", "adapted"}:
            lines.append(
                f"- `{entry['path']}` ← `{entry.get('upstream')}` / `{entry.get('upstream_path')}` "
                f"（{entry.get('license_spdx')}，sha256 `{entry.get('local_sha256', '')[:12]}…`）"
            )
    lines.append("")
    lines.append(f"- SBOM：{len(sbom.get('components', []))} 个组件，serial `{sbom.get('serialNumber')}`")
    return "\n".join(lines) + "\n"
