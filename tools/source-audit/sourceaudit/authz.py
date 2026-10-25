"""授权矩阵：源码路由 ↔ 矩阵 ↔ 越权用例 的三方核对（G17-03）。

这类门禁最容易退化成"一份没人更新的表格"。所以这里不做"人写矩阵、工具读矩阵"，
而是**从源码里扫出路由**，再要求矩阵与之逐条对齐：

* 源码里有、矩阵里没有 → 新端点没经过授权评审（`AUTHZ-UNREGISTERED`）；
* 矩阵里有、源码里没有 → 矩阵过期，读数会骗人（`AUTHZ-STALE`）；
* 两边都有但 `auth` 判定不一致 → 有人在矩阵里手改了鉴权结论（`AUTHZ-AUTH-DRIFT`）；
* 私有端点没有负向用例 → 没有证据证明它真的会拒绝（`AUTHZ-NO-TEST`）；
* 引用的用例在 tests/security 里不存在 → 证据是编的（`AUTHZ-GHOST-TEST`）。

`sync` 只做"把源码的新路由补进矩阵、保留人工字段"，绝不覆盖人写的内容。
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .model import Report, Severity
from .util import DataError, atomic_write, load_json, read_text, repo_root

MATRIX_SCHEMA = "wxlike.authz-matrix.v1"
DEFAULT_SWEEP_TEST = "unauth_sweep_rejects_every_private_route"
_METHODS = ("get", "post", "put", "delete", "patch", "head", "options")
_METHOD_RE = re.compile(r"\b(" + "|".join(_METHODS) + r")\s*\(")
_NAME_RE = re.compile(r"\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)")
_PATH_PARAM_RE = re.compile(r"\{([^}]+)\}")
_ROUTE_RE = re.compile(r"\.route\s*\(")
_LITERAL_RE = re.compile(r'"([^"]+)"')
_HANDLER_MARKERS = ("State(", "Authed", "ApiPath(", "ApiJson(", "Query(", "HeaderMap")

# 路径参数 → 对象类型（授权矩阵里的 `object` 列）
OBJECT_OF = {
    "gid": "group",
    "group_id": "group",
    "id": "object",
    "conv": "conversation",
    "conv_id": "conversation",
    "sid": "session",
    "mid": "media",
    "media_id": "media",
    "fid": "favorite",
    "uid": "user",
    "user": "user",
    "user_id": "user",
    "req_id": "friend_request",
    "msg_id": "message",
    "device_id": "device",
    "comment_id": "comment",
    "moment_id": "moment",
    "video_id": "video",
    "peer": "user",
    "token": "invite_token",
    "channel": "channel",
}


@dataclass(frozen=True)
class Route:
    method: str
    path: str
    handler: str
    module: str
    auth: str


def scan_routes(root: Path) -> list[Route]:
    """扫 `crates/platform/src/**/*.rs` 里所有 `.route(...)`。"""
    routes: list[Route] = []
    for path in sorted((root / "crates/platform/src").rglob("*.rs")):
        text = read_text(path)
        module = str(path.relative_to(root)).replace("\\", "/")
        for args in _route_calls(text):
            literal = _LITERAL_RE.search(args)
            if not literal:
                continue
            for method, handler, auth in _methods_of(args, text):
                routes.append(Route(method.upper(), literal.group(1), handler, module, auth))
    return routes


def _route_calls(text: str) -> list[str]:
    """取出每个 `.route( ... )` 的实参文本（括号配平，支持换行与链式）。"""
    out: list[str] = []
    for m in _ROUTE_RE.finditer(text):
        depth = 1
        i = start = m.end()
        while i < len(text) and depth:
            ch = text[i]
            if ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
            i += 1
        out.append(text[start : i - 1])
    return out


def _methods_of(args: str, file_text: str) -> list[tuple[str, str, str]]:
    """一条 `.route(...)` 里可能有多个方法（`get(a).post(b)`），也可能内联闭包。

    内联闭包（`get(|State(..), authed: Authed| async move {...})`）没有名字可查，
    但它**恰恰最容易被漏掉**——按名字扫路由的工具会视而不见。
    """
    hits = list(_METHOD_RE.finditer(args))
    out: list[tuple[str, str, str]] = []
    for idx, hit in enumerate(hits):
        end = hits[idx + 1].start() if idx + 1 < len(hits) else len(args)
        segment = args[hit.end() : end]
        named = _NAME_RE.match(segment)
        if named:
            handler = named.group(1)
            out.append((hit.group(1), handler, _handler_auth(file_text, handler)))
            continue
        if "admin_ok(" in segment or "x-admin-token" in segment:
            auth = "admin"
        elif "Authed" in segment:
            auth = "user"
        else:
            auth = "public"
        out.append((hit.group(1), "<inline>", auth))
    return out


def _handler_auth(file_text: str, handler: str) -> str:
    """从处理函数签名判定鉴权：`Authed` 提取器 = 用户态；`admin_ok(` = 管理面。

    同名陷阱：`Platform` 上常有同名的业务方法（`send_channel_message`、
    `mention_all` …），它们也写成 `pub async fn xxx(`。只按第一个同名签名匹配会把
    **方法**当成**路由处理器**，于是私有端点被误判成公开——这比漏扫更危险，
    因为"公开"会被写进矩阵，看起来还像是评审过的结论。
    所以这里收集所有同名签名，优先选带 axum 提取器的那个。
    """
    candidates = list(re.finditer(r"\basync fn " + re.escape(handler) + r"\s*\(", file_text))
    if not candidates:
        return "unknown"
    signatures = [_signature_at(file_text, m.end()) for m in candidates]
    handler_shaped = [item for item in signatures if any(marker in item[0] for marker in _HANDLER_MARKERS)]
    signature, end = (handler_shaped or signatures)[0]
    body = file_text[end : _next_fn(file_text, end)]
    if "require_admin(" in body or "admin_ok(" in body or "x-admin-token" in body:
        return "admin"
    if "Authed" in signature:
        return "user"
    return "public"


def _signature_at(text: str, start: int) -> tuple[str, int]:
    depth = 1
    i = start
    while i < len(text) and depth:
        ch = text[i]
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        i += 1
    return text[start : i - 1], i


def _next_fn(text: str, start: int) -> int:
    m = re.search(r"\n(?:pub )?(?:async )?fn ", text[start:])
    return start + m.start() if m else min(len(text), start + 8000)


def _matrix_path(root: Path) -> Path:
    return root / "security/threat-model/authz-matrix.json"


def load_matrix(root: Path) -> dict[str, Any]:
    data = load_json(_matrix_path(root))
    if data.get("schema") != MATRIX_SCHEMA:
        raise DataError(f"authz-matrix schema 必须是 {MATRIX_SCHEMA}")
    return data


def sync(root: Path, write: bool) -> Report:
    """补新路由、删失效路由，保留人工字段（object/scope/tests/notes/exempt）。"""
    report = Report("authz-sync")
    try:
        matrix = load_matrix(root)
        rows = list(matrix.get("routes", []))
    except DataError:
        matrix = {"schema": MATRIX_SCHEMA, "generated_from": "crates/platform/src", "routes": []}
        rows = []
    existing = {(r["method"], r["path"]): r for r in rows}
    routes = scan_routes(root)
    added = updated = 0
    new_rows = []
    for route in routes:
        row = existing.pop((route.method, route.path), None)
        if row is None:
            added += 1
            row = _default_row(route)
        elif row.get("handler") != route.handler or row.get("auth") != route.auth:
            updated += 1
            row.update({"handler": route.handler, "module": route.module, "auth": route.auth})
        new_rows.append(row)
    stale = list(existing.values())
    matrix["routes"] = sorted(new_rows, key=lambda r: (r["path"], r["method"]))
    changed = atomic_write(_matrix_path(root), _dump(matrix)) if write else False
    report.ok(
        "AUTHZ-SYNC",
        "routes",
        f"源码 {len(routes)} 条；新增 {added}、更新 {updated}、失效 {len(stale)}；写入={changed}",
    )
    report.meta.update(
        {"source_routes": len(routes), "added": added, "updated": updated, "stale": len(stale), "written": changed}
    )
    return report


def _default_row(route: Route) -> dict[str, Any]:
    params = _PATH_PARAM_RE.findall(route.path)
    obj = OBJECT_OF.get(params[-1], "object") if params else "endpoint"
    row: dict[str, Any] = {
        "method": route.method,
        "path": route.path,
        "handler": route.handler,
        "module": route.module,
        "auth": route.auth,
        "object": obj,
        "scope": "self" if not params else "owner",
        "tests": [] if route.auth == "public" else [DEFAULT_SWEEP_TEST],
    }
    if route.auth == "public":
        row["public_reason"] = "TODO：未填理由的公开端点在 check 时会失败"
    return row


def _dump(matrix: dict[str, Any]) -> str:
    return json.dumps(matrix, ensure_ascii=False, indent=2) + "\n"


def check(root: Path | None = None) -> Report:
    root = root or repo_root()
    matrix = load_matrix(root)
    rows = matrix.get("routes", [])
    report = Report("authz", meta={"matrix_rows": len(rows)})
    routes = scan_routes(root)
    by_key = {(r["method"], r["path"]): r for r in rows}
    source_keys = {(r.method, r.path) for r in routes}

    for route in routes:
        row = by_key.get((route.method, route.path))
        if row is None:
            report.fail(
                "AUTHZ-UNREGISTERED",
                f"{route.method} {route.path}",
                f"新端点未登记授权矩阵（{route.module}:{route.handler}）：先 sync 再补 tests",
                Severity.HIGH,
            )
            continue
        if row.get("auth") != route.auth:
            report.fail(
                "AUTHZ-AUTH-DRIFT",
                f"{route.method} {route.path}",
                f"矩阵写 auth={row.get('auth')!r}，源码是 {route.auth!r}（handler={route.handler}）",
                Severity.CRITICAL,
            )
    for key in by_key:
        if key not in source_keys:
            report.fail("AUTHZ-STALE", f"{key[0]} {key[1]}", "矩阵里有、源码里没有：矩阵过期", Severity.MEDIUM)

    tests = _test_names(root)
    for row in rows:
        subject = f"{row['method']} {row['path']}"
        if row.get("auth") == "public":
            reason = str(row.get("public_reason", ""))
            if not reason.strip() or "TODO" in reason:
                report.fail("AUTHZ-PUBLIC", subject, "公开端点必须写 public_reason（为什么不需要鉴权）", Severity.HIGH)
            continue
        exempt = row.get("exempt")
        if exempt:
            if not exempt.get("reason") or not exempt.get("issue"):
                report.fail("AUTHZ-EXEMPT", subject, "豁免必须写 reason + issue", Severity.MEDIUM)
            continue
        referenced = row.get("tests") or []
        if not referenced:
            report.fail("AUTHZ-NO-TEST", subject, "私有端点没有负向用例：无法证明它会拒绝", Severity.HIGH)
            continue
        for ghost in [t for t in referenced if t not in tests]:
            report.fail("AUTHZ-GHOST-TEST", subject, f"引用的用例 {ghost!r} 在 tests/security 里不存在", Severity.HIGH)

    kinds: dict[str, int] = {}
    for row in rows:
        kinds[row.get("auth", "unknown")] = kinds.get(row.get("auth", "unknown"), 0) + 1
    report.meta.update({"source_routes": len(routes), "kinds": kinds, "test_fns": len(tests)})
    if not report.blocking:
        report.ok(
            "AUTHZ-COVER",
            "routes",
            f"{kinds.get('user', 0)} 用户态 / {kinds.get('admin', 0)} 管理 / {kinds.get('public', 0)} 公开，全部有证据",
        )
    return report


def _test_names(root: Path) -> set[str]:
    names: set[str] = set()
    base = root / "tests/security/tests"
    if not base.is_dir():
        return names
    for path in base.rglob("*.rs"):
        names.update(re.findall(r"\bfn\s+([a-z0-9_]+)\s*\(", read_text(path)))
    return names
