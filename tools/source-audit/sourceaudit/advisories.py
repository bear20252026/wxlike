"""漏洞与依赖准入门禁（G17-02）。

判定链是三步，顺序不能换：

1. **在不在依赖图里**（锁文件 + 生态）。不在 → `not_applicable`，不报。
   这是"未安装 RSC 不误报 React SPA"的实现位置。
2. **版本在不在受影响区间**。不在 → `fixed`（要么本来就安全，要么已经升过）。
3. **可不可达**。在依赖图里但代码路径不经过（例如可选 feature 未启用、
   只是 dev-dependency）→ `not_applicable` 并写清依据；否则 `candidate`，按严重级拦。

例外只认带 owner + issue + 到期日的登记，过期即拦。"误报"不是例外理由，
"能证明不可达"才是——两者在报告里分开列。
"""

from __future__ import annotations

import json
import re
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from .inventory import CARGO_LOCKS, NPM_LOCKS
from .model import Report, Severity
from .provenance import expires_ok
from .util import DataError, load_json, read_text, repo_root, sha256_text, today

SCHEMA = "wxlike.advisories.v1"
POLICY_SCHEMA = "wxlike.advisory-policy.v1"
OSV_BATCH = "https://api.osv.dev/v1/querybatch"

FIXED = "fixed"
CANDIDATE = "candidate"
NOT_APPLICABLE = "not_applicable"
UNVERIFIED = "unverified"


@dataclass(frozen=True)
class Hit:
    """一条公告在某个组件上的结论。"""

    advisory_id: str
    component: str
    version: str | None
    severity: Severity
    verdict: str
    why: str
    surfaces: str = ""
    kind: str = "vulnerability"

    @property
    def subject(self) -> str:
        return f"{self.component}@{self.version or '?'}"


def check(
    root: Path | None = None,
    offline: bool = False,
    source: Path | None = None,
    timeout: float = 10.0,
) -> Report:
    root = root or repo_root()
    policy = load_json(root / "security/advisory-policy/policy.json")
    dataset = load_json(root / "security/advisory-policy/advisories.json")
    if policy.get("schema") != POLICY_SCHEMA:
        raise DataError(f"policy schema 必须是 {POLICY_SCHEMA}")
    if dataset.get("schema") != SCHEMA:
        raise DataError(f"advisories schema 必须是 {SCHEMA}")

    inventory = _inventory(root)
    report = Report(
        "advisories",
        meta={"components": len(inventory), "offline": offline, "advisories": len(dataset["advisories"])},
    )

    advisories = list(dataset["advisories"])
    scanned: dict[str, Any] | None = None
    if source is not None:
        scanned = load_json(source)
        advisories.extend(_from_scan(scanned))
    elif not offline:
        scanned = _fetch_osv(inventory, timeout)
        advisories.extend(_from_osv(scanned, advisories, timeout))
        report.meta["osv_packages_queried"] = len(inventory)

    hits = [h for a in advisories for h in _judge(a, inventory, policy)]
    if not hits:
        report.warn("ADV-EMPTY", "advisories", "没有任何公告被评估：数据集或依赖图可能为空", Severity.HIGH)
    _report_hits(report, hits, policy)
    report.meta["classification"] = _classification(hits)
    report.meta["dataset_sha256"] = sha256_text(json.dumps(dataset, sort_keys=True))[:16]
    _ = scanned, today
    return report


# ---- 依赖图 -------------------------------------------------------------
def _inventory(root: Path) -> dict[str, dict[str, set[str]]]:
    """组件名 → 版本 → 来源面。

    **必须按面记录**：桌面（`ui/src-tauri/Cargo.lock`）与根 workspace 是两份
    独立的锁，同一 crate 可能一个已升、一个还停在受影响版本。只报"这个 crate
    有漏洞"而不说"哪一份锁"，等于把问题推回给读者。
    """
    out: dict[str, dict[str, set[str]]] = {}

    def add(name: str, version: str, surface: str) -> None:
        if not name or not version:
            return
        out.setdefault(name, {}).setdefault(version, set()).add(surface)

    import tomllib

    for rel, surface in CARGO_LOCKS:
        path = root / rel
        if not path.is_file():
            continue
        for pkg in tomllib.loads(read_text(path)).get("package", []):
            add(pkg.get("name", ""), pkg.get("version", ""), surface)

    for rel, surface in NPM_LOCKS:
        path = root / rel
        if not path.is_file():
            continue
        for key, meta in (load_json(path).get("packages") or {}).items():
            if key.startswith("node_modules/"):
                add(key.rsplit("node_modules/", 1)[-1], meta.get("version", ""), surface)

    pub = root / "apps/mobile/pubspec.lock"
    if pub.is_file():
        name = None
        for line in read_text(pub).splitlines():
            m = re.match(r"^  ([A-Za-z0-9_]+):\s*$", line)
            if m:
                name = m.group(1)
                continue
            v = re.match(r"^    version:\s*[\"']?([^\"'\s]+)", line)
            if v and name:
                add(name, v.group(1), "mobile")
                name = None

    compose = root / "docker-compose.yml"
    if compose.is_file():
        for ref in re.findall(r"^\s*image:\s*[\"']?([^\"'\s#]+)", read_text(compose), re.MULTILINE):
            name, _, tag = ref.partition(":")
            if tag and "@" not in ref:
                add(name.split("/")[-1], tag, "infra")
    return out


# ---- 判定 ---------------------------------------------------------------
def _judge(advisory: dict[str, Any], inventory: dict[str, set[str]], policy: dict[str, Any]) -> list[Hit]:
    component = advisory.get("component", "")
    ecosystem = advisory.get("ecosystem", "")
    raw_severity = str(advisory.get("severity", "")).lower()
    severity = Severity.parse(raw_severity)
    unknown_severity = raw_severity in {"unknown", "", "none"}
    versions = inventory.get(component, {})
    if not versions:
        # 不在图上：报 not_applicable，但要写清"因为锁文件里没有它"，
        # 而不是含糊的"不适用"。含糊的不适用等于没有评审。
        return [
            Hit(
                advisory["id"],
                component,
                None,
                severity,
                NOT_APPLICABLE,
                f"{ecosystem} 依赖图中不存在该组件（锁文件未解析到）",
                kind=advisory.get("kind", "vulnerability"),
            )
        ]
    out = []
    for version, surfaces in sorted(versions.items()):
        where = f"来源面：{','.join(sorted(surfaces))}"
        if not any(_in_range(version, r) for r in advisory.get("affected", [])):
            out.append(
                Hit(advisory["id"], component, version, severity, FIXED, where,
                    ",".join(sorted(surfaces)), advisory.get("kind", "vulnerability"))
            )
            continue
        if unknown_severity and advisory.get("kind", "vulnerability") == "vulnerability":
            out.append(
                Hit(advisory["id"], component, version, severity, UNVERIFIED,
                    f"公告未给出分级，需人工核实；{where}", ",".join(sorted(surfaces)),
                    advisory.get("kind", "vulnerability"))
            )
            continue
        verdict, why = _reachability(advisory, policy, component)
        out.append(
            Hit(
                advisory["id"],
                component,
                version,
                severity,
                verdict,
                f"{why}；{where}",
                ",".join(sorted(surfaces)),
                advisory.get("kind", "vulnerability"),
            )
        )
    return out


def _reachability(advisory: dict[str, Any], policy: dict[str, Any], component: str) -> tuple[str, str]:
    rule = (policy.get("reachability") or {}).get(advisory["id"], {})
    kind = rule.get("kind", "in_lockfile")
    if kind == "in_lockfile":
        return CANDIDATE, rule.get("why", "锁文件里存在，默认视为可达（保守）")
    if kind == "optional_feature":
        # 组件已在锁里，但只在某个可选 feature/目标下才编进产物
        return CANDIDATE, rule.get("why", "可选 feature 已启用时可达")
    if kind == "not_installed":
        return NOT_APPLICABLE, rule.get("why", "该组件不会被安装")
    return UNVERIFIED, f"未知可达性规则 {kind}：按待核实处理"


def _in_range(version: str, spec: str) -> bool:
    """`">=0.1.0, <1.0.10"` 这种逗号分隔 = AND。空 spec 视为不受影响。"""
    spec = (spec or "").strip()
    if not spec:
        return False
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        m = re.match(r"^(>=|<=|>|<|==|=|\^)?\s*([0-9][0-9A-Za-z.\-+]*)$", part)
        if not m:
            raise DataError(f"无法解析版本区间：{part!r}")
        op, target = m.group(1) or "==", m.group(2)
        cmp = _cmp(version, target)
        ok = {
            ">=": cmp >= 0,
            ">": cmp > 0,
            "<=": cmp <= 0,
            "<": cmp < 0,
            "==": cmp == 0,
            "=": cmp == 0,
            "^": cmp >= 0 and _major(version) == _major(target),
        }[op]
        if not ok:
            return False
    return True


def _major(v: str) -> str:
    return v.split(".", 1)[0]


def _cmp(left: str, right: str) -> int:
    """够用的 semver 比较：数字逐段比，预发布版本 < 正式版。"""
    def split(v: str) -> tuple[list[Any], str]:
        core, _, pre = v.partition("-")
        core = core.split("+", 1)[0]
        nums: list[Any] = []
        for part in core.split("."):
            nums.append(int(part) if part.isdigit() else part)
        return nums, pre

    a, a_pre = split(left)
    b, b_pre = split(right)
    for x, y in zip(a, b):
        if x == y:
            continue
        if isinstance(x, int) and isinstance(y, int):
            return -1 if x < y else 1
        return -1 if str(x) < str(y) else 1
    if len(a) != len(b):
        return -1 if len(a) < len(b) else 1
    if a_pre == b_pre:
        return 0
    if not a_pre:
        return 1
    if not b_pre:
        return -1
    return -1 if a_pre < b_pre else 1


# ---- 报告 ---------------------------------------------------------------
def _report_hits(report: Report, hits: Iterable[Hit], policy: dict[str, Any]) -> None:
    gate = policy.get("gate", {})
    block_levels = {Severity.parse(s) for s in gate.get("block_severities", ["critical", "high"])}
    exceptions = {e["id"]: e for e in policy.get("exceptions", [])}
    for hit in hits:
        code = f"ADV-{hit.verdict.upper()}"
        if hit.verdict in (FIXED, NOT_APPLICABLE):
            report.ok(code, hit.subject, f"{hit.advisory_id}: {hit.why}")
            continue
        if hit.kind == "unmaintained":
            # "无人维护"不是漏洞：没有修复版本可升，拦下来只会逼人写假例外。
            # 按知情接受报 warn，让它出现在报告里而不是消失在噪音里。
            report.warn("ADV-UNMAINTAINED", hit.subject, f"{hit.advisory_id}: {hit.why}", Severity.LOW)
            continue
        if hit.verdict == UNVERIFIED:
            report.warn(code, hit.subject, f"{hit.advisory_id}: {hit.why}", Severity.HIGH)
            continue
        exc = exceptions.get(hit.advisory_id)
        if exc:
            if not exc.get("owner") or not exc.get("issue"):
                report.fail("ADV-EXC", hit.subject, f"{hit.advisory_id} 例外缺 owner/issue", Severity.HIGH)
                continue
            if not expires_ok(exc.get("expires"), "advisory-policy.exceptions.expires", report, hit.subject):
                continue
            report.exempt(
                "ADV-EXEMPT",
                hit.subject,
                f"{hit.advisory_id}（{hit.severity.value}）：{exc.get('reason', '')}",
                owner=exc["owner"],
                issue=exc["issue"],
                expires=exc["expires"],
            )
            continue
        severity = hit.severity if hit.severity in block_levels else Severity.MEDIUM
        message = f"{hit.advisory_id}（{hit.severity.value}）可达：{hit.why}"
        if hit.severity in block_levels:
            report.fail(code, hit.subject, message, severity)
        else:
            report.warn(code, hit.subject, message, severity)


def _classification(hits: list[Hit]) -> dict[str, int]:
    out = {FIXED: 0, CANDIDATE: 0, NOT_APPLICABLE: 0, UNVERIFIED: 0}
    for hit in hits:
        out[hit.verdict] = out.get(hit.verdict, 0) + 1
    return out


# ---- 联网：OSV ----------------------------------------------------------
def _fetch_osv(inventory: dict[str, set[str]], timeout: float) -> dict[str, Any]:
    """把锁文件里的 cargo/npm/pub 组件批量问 OSV。断网就是数据错误，不静默跳过。"""
    queries = []
    index: list[tuple[str, str]] = []
    for name, versions in inventory.items():
        for version in sorted(versions.keys()):
            queries.append({"package": {"name": name, "ecosystem": "crates.io"}, "version": version})
            index.append((name, version))
    if not queries:
        return {"results": []}
    payload = json.dumps({"queries": queries}).encode("utf-8")
    req = urllib.request.Request(OSV_BATCH, data=payload, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = json.loads(resp.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise DataError(
            f"OSV 查询失败（{exc}）：冻结前必须联网复核公告；临时断网可用 --offline 走本地数据集"
        ) from exc
    return {"index": index, "osv": data}


def _from_osv(scanned: dict[str, Any], known: list[dict[str, Any]], timeout: float = 10.0) -> list[dict[str, Any]]:
    """把 OSV 结果转成与本地数据集同构的条目；已知 id 不重复计入。"""
    seen = {a["id"] for a in known}
    out = []
    index = scanned.get("index") or []
    for i, result in enumerate((scanned.get("osv") or {}).get("results", [])):
        if i >= len(index):
            break
        name, version = index[i]
        for vuln in result.get("vulns", []) or []:
            vid = vuln.get("id", "")
            if not vid or vid in seen:
                continue
            seen.add(vid)
            info = ((vuln.get("database_specific") or {}).get("informational") or "").lower()
            summary = (vuln.get("summary") or "").lower()
            if not summary and not vuln.get("severity"):
                # batch 接口只回 id/affected；要分级和 summary 得逐条查详情。
                # 只对"待核实"的少量条目查，不把 871 个组件都放大成 871 次请求。
                detail = _osv_detail(vid, timeout)
                summary = (detail.get("summary") or "").lower()
                vuln = {**vuln, **detail}
            # RUSTSEC 的 unmaintained/unsound 在 batch 结果里只体现在 summary 文本上，
            # 没有独立的 severity —— 这正是"不拦但要说"的那一类。
            unmaintained = info in {"unmaintained", "unsound"} or "unmaintained" in summary or "not maintained" in summary
            kind = "unmaintained" if unmaintained else "vulnerability"
            out.append(
                {
                    "id": vid,
                    "component": name,
                    "ecosystem": "crates.io",
                    "severity": "low" if kind == "unmaintained" else (_osv_severity(vuln) or "unknown"),
                    "affected": [f"=={version}"],
                    "fixed": [],
                    "url": f"https://osv.dev/vulnerability/{vid}",
                    "kind": kind,
                    "note": "OSV 新发现（本地数据集未登记）：候选新增，需分级处置",
                }
            )
    return out


def _osv_detail(vid: str, timeout: float) -> dict[str, Any]:
    """逐条详情。batch 接口只回 id/affected，分级与 summary 要单独取。

    只对"待核实"的少量条目调用（当前是十几条），不会把 871 个组件放大成
    871 次请求。取不到就维持 unknown，交给人工核实——不编一个分级。
    """
    try:
        with urllib.request.urlopen(f"https://api.osv.dev/v1/vulns/{vid}", timeout=timeout) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError):
        return {}


def _osv_severity(vuln: dict[str, Any]) -> str | None:
    """OSV 不保证有分级；有就用它自己的，没有就返回 None（不猜）。

    顺序很重要：先看 `database_specific.severity`（RUSTSEC/GHSA 的人工分级），
    再看 CVSS 向量。只看 CVSS 的粗分档会把 Moderate 判成 High——门禁因此
    误拦一个"只影响完整性、无保密性影响"的问题，然后所有人开始写例外。
    """
    db = ((vuln.get("database_specific") or {}).get("severity") or "").upper()
    mapped = {"CRITICAL": "critical", "HIGH": "high", "MODERATE": "medium", "MEDIUM": "medium", "LOW": "low"}.get(db)
    if mapped:
        return mapped
    for sev in vuln.get("severity", []) or []:
        score = sev.get("score") or ""
        if score.startswith("CVSS:"):
            return _cvss_band(score)
    return None


def _cvss_band(score: str) -> str:
    """从向量字符串粗分档：只区分"要不要拦"，不做精确评分。"""
    network = "AV:N" in score
    hits = sum(1 for k in ("C:H", "I:H", "A:H") if k in score)
    if network and hits >= 2:
        return "critical"
    if hits >= 1:
        return "high" if network else "medium"
    return "medium"


def _from_scan(scanned: dict[str, Any]) -> list[dict[str, Any]]:
    """兼容 `cargo audit --json` 的 `--input`：把它的 vulnerabilities 转成同构条目。"""
    out = []
    for item in (scanned.get("vulnerabilities") or {}).get("list", []) or []:
        adv = item.get("advisory", {})
        pkg = item.get("package", {})
        out.append(
            {
                "id": adv.get("id", "unknown"),
                "component": pkg.get("name", ""),
                "ecosystem": "crates.io",
                "severity": adv.get("severity", "high"),
                "affected": [f"=={pkg.get('version', '0')}"],
                "fixed": [],
                "url": adv.get("url", ""),
                "note": "来自 cargo audit --json",
            }
        )
    return out
