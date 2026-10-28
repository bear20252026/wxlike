#!/usr/bin/env python3
"""ADR-28 静态守卫：`async fn` 里不许持同步连接锁（issue #326，ADR-26 的前置）。

为什么要这条闸：`crates/platform/src/db.rs` 的 `Db` 是 `Arc<Mutex<Connection>>`，
rusqlite 是**同步**驱动。axum 的 handler 是 `async fn`，里面一句
`let mut c = self.db().lock();` + `c.transaction()` 就把整条 SQL 跑在 tokio worker
上——worker 被占住，同一 worker 上的别的任务只能排队。换 PostgreSQL / sqlx
**不会**自动治好它（池子同样会串行），所以 ADR-26 的顺序是"先解锁的用法、再换库"。

三条规则（都只看 `async fn` 的函数体；注释与字符串不参与判定）：

1. `direct-lock`（硬）：体内出现 `db().lock()`；或出现 `.lock()` 之后同一体内
   紧跟 `transaction()` → 违规。这就是卡上要求禁掉的那句。
2. `stale-waiver`（硬）：体内挂了 `TODO(ADR-28)` 却已经没有任何违规 → 违规。
   改造完要把标记删掉，免得"待办清单"越积越假。
3. `blocking-call`（只对本卡三条样板文件 `ENFORCED_FILES` 生效）：`async fn`
   在 await 上下文里直接调用"已知会同步上锁的函数"（先从函数体扫出种子，再沿
   `self.x()` / `p.x()` 调用关系做传递闭包），而没有包进
   `offload(..)` / `blocking(..)` / `spawn_blocking(..)` → 违规。
   `friends.rs:408` 那种"handler 调同步方法"的形状就是靠这条兜住的。

豁免：命中 1/3 的函数体里必须有 `TODO(ADR-28)`（本卡只改三条样板热点，其余同类
点位留显式待办，不许静默跳过）。

用法：
    python scripts/adr28_async_db_guard.py             # 扫本仓库，干净则 0
    python scripts/adr28_async_db_guard.py --root DIR  # 扫任意目录（注入自测用）
退出码：0 干净 / 1 有违规 / 2 守卫自己跑不起来。
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

# 生产代码目录；`tests/` 与 `#[cfg(test)]` 之后的模块不参与判定（测试跑单线程
# 运行时，形状与线上不同，且它们不是 await 上下文里的 handler）。
SCAN_ROOTS = (
    "crates/platform/src",
    "crates/message/src",
    "crates/net/src",
    "crates/core/src",
    "crates/features",
    "server/src",
)
SKIP_DIR_PARTS = ("/cmd/", "/mcp/", "/client", "/ffi/", "/features/media/", "/ui/", "/apps/", "/target/")
# 本卡三条样板热点（消息写 / 关系写 / 群管理写）：规则 3 在这些文件里整文件强制。
ENFORCED_FILES = (
    "crates/platform/src/friends.rs",
    "crates/platform/src/groups.rs",
    "crates/platform/src/groups_admin.rs",
)
# 规则 3 点名的样板函数：`communities.rs` 不是本卡独占文件，整文件强制会把 20 个
# 无关读接口一起卷进来，所以只把"消息写"这两条样板钉住——样板不许退化。
ENFORCED_FNS = {
    "crates/platform/src/communities.rs": {"send_channel_message", "send_thread_reply"},
}
WAIVER = "TODO(ADR-28)"

ASYNC_FN = re.compile(r"\basync\s+fn\s+([A-Za-z0-9_]+)")
SYNC_FN = re.compile(r"\bfn\s+([A-Za-z0-9_]+)\s*[(<]")
DB_LOCK = re.compile(r"\bdb\s*\(\s*\)\s*\.\s*lock\s*\(\s*\)")
ANY_LOCK = re.compile(r"\.\s*lock\s*\(\s*\)")
TRANSACTION = re.compile(r"\.\s*transaction\s*\(\s*\)|\.\s*unchecked_transaction\s*\(\s*\)")
# 只认平台句柄上的方法调用，否则 `headers.get(..)`、`Vec::is_empty(..)` 这类
# 同名通用方法会被误判成"同步查库"。
RECEIVER = r"(?:self|this|platform|state|p)"
# 搬进阻塞池的调用：这些括号里的同步代码不算违规。
OFFLOAD_CALL = re.compile(r"\b(?:offload|spawn_blocking|blocking)\s*\(")


def mask(text: str) -> str:
    """注释与字符串换成空格，长度与下标完全保持——规则只看代码形状。"""
    out = list(text)
    i, n = 0, len(text)
    state = "code"
    while i < n:
        ch = text[i]
        nxt = text[i + 1: i + 2]
        if state == "code":
            if ch == "/" and nxt == "/":
                state = "line"
            elif ch == "/" and nxt == "*":
                state = "block"
            elif ch in "\"'":
                state, quote = "str", ch
                out[i] = " "
                continue
            i += 1
            continue
        if state == "line":
            if ch == "\n":
                state = "code"
            else:
                out[i] = " "
            i += 1
            continue
        if state == "block":
            if ch == "*" and nxt == "/":
                out[i] = out[i + 1] = " "
                state, i = "code", i + 2
                continue
            out[i] = " " if ch != "\n" else ch
            i += 1
            continue
        # state == "str"
        if ch == "\\":
            out[i] = " "
            if i + 1 < n:
                out[i + 1] = " " if text[i + 1] != "\n" else text[i + 1]
            i += 2
            continue
        if ch == quote:
            state = "code"
        out[i] = " "
        i += 1
    return "".join(out)


def drop_test_modules(text: str) -> str:
    """`#[cfg(test)]` 之后整段换空（保住行号）。"""
    index = text.find("#[cfg(test)]")
    if index < 0:
        return text
    return text[:index] + "".join(ch if ch == "\n" else " " for ch in text[index:])


def balanced_end(text: str, open_index: int) -> int:
    closer = {"{": "}", "(": ")"}[text[open_index]]
    depth = 0
    for index in range(open_index, len(text)):
        if text[index] == text[open_index]:
            depth += 1
        elif text[index] == closer:
            depth -= 1
            if depth == 0:
                return index + 1
    return len(text)


def iter_fns(code: str, pattern: re.Pattern):
    """产出 (函数名, 行号, 签名下标, 体内下标区间)；下标对齐原文，注释也能一起看。"""
    for match in pattern.finditer(code):
        if code[max(0, match.start() - 6): match.start()] == "async ":
            continue  # `async fn` 交给 ASYNC_FN 那一路处理
        brace = code.find("{", match.end())
        if brace < 0:
            continue
        yield match.group(1), code[: match.start()].count("\n") + 1, match.start(), (brace, balanced_end(code, brace))


def remove_offloaded(code: str, start: int, end: int) -> str:
    body = code[start:end]
    while True:
        match = OFFLOAD_CALL.search(body)
        if not match:
            return body
        open_index = body.index("(", match.start())
        stop = balanced_end(body, open_index)
        head = match.start()
        while body[:head].rstrip().endswith("."):
            head = len(body[:head].rstrip()) - 1
        body = body[:head] + " " * (stop - head)


def direct_lock_violation(body: str) -> str:
    if DB_LOCK.search(body):
        return "体内出现 db().lock()"
    lock = ANY_LOCK.search(body)
    if lock and TRANSACTION.search(body[lock.end():]):
        return ".lock() 之后紧跟 transaction()"
    return ""


def production_files(root: pathlib.Path):
    for rel in SCAN_ROOTS:
        base = root / rel
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            posix = path.as_posix()
            if "/tests/" in posix or any(part in posix for part in SKIP_DIR_PARTS):
                continue
            yield path


def code_of(path: pathlib.Path) -> str:
    return mask(drop_test_modules(read(path)))


def read(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8")


HANDLE_CALL = re.compile(r"\b(?:%s)\s*\.\s*([A-Za-z0-9_]+)\s*\(" % RECEIVER)


def handle_calls(body: str) -> set:
    """体内所有 `句柄 . 方法(` 的方法名——一次扫描，之后只做集合运算。

    （把候选名拼成一条大正则再逐函数匹配，147 个文件要跑三分钟，守卫就没人愿意跑了。）
    """
    return set(HANDLE_CALL.findall(body))


def sync_db_names(files) -> set:
    """同步上锁的 `Platform` 方法名（种子 + 沿句柄调用的传递闭包），供规则 3 使用。

    种子只取 `crates/platform/src` 里带 `self` 接收者的方法——不然 `get` /
    `is_empty` 这类同名标准库方法会被当成"查库函数"。handler 调同步方法走的
    正是句柄这条路（`friends.rs:408` 就是 `platform.send_friend_request(..)`）。
    """
    bodies, seeds = {}, set()
    for path in files:
        code = code_of(path)
        platform_method = "/platform/src/" in path.as_posix()
        for name, _, sig, (start, end) in iter_fns(code, SYNC_FN):
            body = code[start:end]
            bodies.setdefault(name, []).append(handle_calls(body))
            locks = ANY_LOCK.search(body) or DB_LOCK.search(body)
            if locks and platform_method and "self" in code[sig:start]:
                seeds.add(name)
    for _ in range(12):
        grown = {n for n, calls in bodies.items() if any(c & seeds for c in calls)} - seeds
        if not grown:
            break
        seeds |= grown
    return seeds


def is_enforced(rel: str, name: str) -> bool:
    return rel in ENFORCED_FILES or name in ENFORCED_FNS.get(rel, ())


def check_async_fn(rel: str, name: str, line: int, raw_body: str, code_body: str, sync_names: set):
    """返回违规描述，或 None。"""
    has_waiver = WAIVER in raw_body
    problem = direct_lock_violation(code_body)
    leaked = ""
    if is_enforced(rel, name) and not problem:
        leaked = leaked_call(code_body, sync_names)
    if has_waiver:
        if problem or leaked:
            return None
        return f"{rel}:{line}: `{name}` 挂着 {WAIVER} 但体内已无违规——改造完请把标记删掉"
    if problem:
        return f"{rel}:{line}: async fn `{name}`：{problem}（await 上下文里持同步连接锁）"
    if leaked:
        return (f"{rel}:{line}: async fn `{name}`：await 上下文里直接调同步查库函数 "
                f"`{leaked}`，要包进 offload/blocking")
    return None


def leaked_call(body: str, sync_names: set) -> str:
    """挖掉 `offload(..)` / `blocking(..)` 之后，还剩哪些同步查库调用裸在 await 里。"""
    stripped = remove_offloaded(body, 0, len(body))
    hits = handle_calls(stripped) & sync_names
    return sorted(hits)[0] if hits else ""


def scan(root: pathlib.Path):
    files = list(production_files(root))
    sync_names = sync_db_names(files)
    findings = []
    for path in files:
        raw = read(path)
        code = mask(drop_test_modules(raw))
        rel = path.relative_to(root).as_posix()
        for name, line, sig, (start, end) in iter_fns(code, ASYNC_FN):
            # mask/drop_test_modules 都不改长度，所以 raw 与 code 下标一一对应：
            # 违规看 code，waiver 看含注释的原文。
            hit = check_async_fn(rel, name, line, raw[sig:end], code[start:end], sync_names)
            if hit:
                findings.append(hit)
    return findings


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="ADR-28：async 上下文禁持同步连接锁")
    parser.add_argument("--root", default=None, help="扫描根目录（默认仓库根）")
    args = parser.parse_args(argv)
    root = pathlib.Path(args.root).resolve() if args.root else pathlib.Path(__file__).resolve().parents[1]
    if not root.is_dir():
        print(f"目录不存在：{root}", file=sys.stderr)
        return 2
    findings = scan(root)
    for line in findings:
        print(f"ADR-28 违规：{line}", file=sys.stderr)
    print(f"ADR-28 守卫：扫描 {len(list(production_files(root)))} 个生产文件，违规 {len(findings)} 条")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
