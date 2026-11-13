"""wxlike 代码质量体检：函数长度、嵌套、魔法数字、重复块、职责混装。

刻意不依赖任何第三方库：只要 python 3.11+。
字符串/字符字面量用状态机跳过，避免正则转义把大括号数错。

口径：**只看生产代码**——不含 `tests/` 目录，也不含文件内 `#[cfg(test)]`
之后的模块。测试里的 `assert_eq!(x, 250)` 不是"魔法数字"，把它算进去会让
指标失去意义。

支持 `--diff` 模式：只扫 PR diff 涉及的文件，用于增量质量门禁。
"""
import os
import re
import json
import hashlib
import collections
import pathlib
import subprocess
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
ROOTS = ["crates", "server/src", "ui/src-tauri/src"]

TEST_MODULE_RE = re.compile(r"^\s*#\[cfg\(test\)\]")


def resolve_diff_base() -> str:
    """挑一个能代表"PR 基线"的 ref，别写死 `main...HEAD`。

    写死 `main...HEAD` 在两种环境下会失控：

    - 本地 `main` 滞后（多年没 fetch）时，merge-base 会退化成 HEAD，diff 变空
      ——门禁**静默放行**；或退化成一个无关旧点，把不相干的文件全扫进来。
    - 多 worktree 下 `main` 可能检出在别的 worktree 里，本地根本动不了它。

    CI 的 `audit` 任务在跑质量门之前会 `git fetch origin main:main`，所以那里
    `main` 是新的；本地优先 `origin/main`。`CI_BASE_SHA` 显式指定时最优先
    （与 coverage 任务 `CI_BASE_SHA` 的口径一致）。
    """
    env = os.environ.get("CI_BASE_SHA")
    if env:
        return env
    for candidate in ("origin/main", "main"):
        proc = subprocess.run(
            ["git", "rev-parse", "--verify", "--quiet", candidate],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )
        if proc.returncode == 0 and proc.stdout.strip():
            return candidate
    sys.exit(
        "找不到 diff 基线：CI_BASE_SHA 未设置，origin/main 与 main 都不存在。\n"
        "先 `git fetch origin main`，或显式设 CI_BASE_SHA=<merge-base sha>。"
    )


def diff_files() -> set[str]:
    """拿到 PR diff 涉及的文件列表。基线挑选见 `resolve_diff_base`。"""
    base = resolve_diff_base()
    proc = subprocess.run(
        ["git", "diff", "--name-only", f"{base}...HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        # 返回空集 = 门禁静默放行，比报错危险得多
        sys.exit(f"git diff {base}...HEAD 失败：{proc.stderr.strip()[-500:]}")
    out = proc.stdout.strip()
    # 空 diff 要返回空集：`"".split("\n")` 会给出 `[""]`，扫描面变成"1 个名字
    # 为空的文件"，比 0 个更难排查。
    return set(out.split("\n")) if out else set()


def rust_files():
    diff_mode = "--diff" in sys.argv
    only = diff_files() if diff_mode else None
    for root in ROOTS:
        for dp, _dn, fn in os.walk(root):
            if any(s in dp for s in ("target", "node_modules", ".git")):
                continue
            for f in fn:
                if not f.endswith(".rs"):
                    continue
                p = os.path.join(dp, f).replace("\\", "/")
                if "/tests/" in p:
                    continue
                if only is not None and p not in only:
                    continue
                yield p


def production_lines(path):
    """生产代码的行：截到第一个 `#[cfg(test)]` 之前。"""
    try:
        lines = open(path, encoding="utf-8").read().splitlines()
    except Exception:
        return []
    for i, line in enumerate(lines):
        if TEST_MODULE_RE.match(line):
            return lines[:i]
    return lines


def strip_code(line):
    """去掉行注释与字符串/字符字面量内容，只留结构性字符。"""
    out = []
    i = 0
    n = len(line)
    in_str = False
    in_char = False
    while i < n:
        c = line[i]
        if in_str:
            if c == "\\":
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if in_char:
            if c == "\\":
                i += 2
                continue
            if c == "'":
                in_char = False
            i += 1
            continue
        if c == '"':
            in_str = True
            i += 1
            continue
        if c == "'":
            # 区分生命周期 'a 与字符字面量 'x'
            if i + 2 < n and line[i + 2] == "'":
                in_char = True
                i += 1
                continue
            i += 1
            continue
        if c == "/" and i + 1 < n and line[i + 1] == "/":
            break
        out.append(c)
        i += 1
    return "".join(out)


fn_re = re.compile(
    r"^(\s*)(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?"
    r"(?:unsafe\s+)?(?:extern\s+\"[^\"]*\"\s+)?fn\s+(\w+)"
)

fns = []
files_info = []
for path in rust_files():
    lines = production_lines(path)
    if not lines:
        continue
    files_info.append((path, len(lines)))
    i = 0
    while i < len(lines):
        m = fn_re.match(lines[i])
        if not m:
            i += 1
            continue
        name = m.group(2)
        j = i
        while j < len(lines) and "{" not in strip_code(lines[j]):
            j += 1
        if j >= len(lines):
            break
        depth = 0
        started = False
        maxdepth = 0
        end = j
        for k in range(j, min(len(lines), j + 5000)):
            code = strip_code(lines[k])
            for ch in code:
                if ch == "{":
                    depth += 1
                    started = True
                    maxdepth = max(maxdepth, depth)
                elif ch == "}":
                    depth -= 1
            if started and depth <= 0:
                end = k
                break
        body = "\n".join(lines[i:end + 1])
        fns.append({
            "file": path,
            "name": name,
            "line": i + 1,
            "len": end - i + 1,
            "nest": max(0, maxdepth - 1),
            "sql": bool(re.search(r"\b(SELECT|INSERT INTO|UPDATE .* SET|DELETE FROM)\b", body)),
            "http": ("StatusCode" in body or "Json(" in body or "HeaderMap" in body),
            "io": ("std::fs::" in body or "File::" in body),
            "pub": bool(re.match(r"\s*pub\b", lines[i])),
        })
        i = end + 1

# 魔法数字：两位以上、非 0/1/10/100/1000、且不在 const/static 行
num_re = re.compile(r"(?<![\w.])(\d{2,})(?![\w.])")
BORING = {0, 1, 2, 10, 16, 100, 1000, 24, 60, 3600, 1024, 255,
          200, 201, 202, 206, 400, 401, 403, 404, 409, 416, 422, 429, 500, 502}
magic = collections.Counter()
magic_sites = collections.defaultdict(list)
for path in rust_files():
    lines = production_lines(path)
    for n, line in enumerate(lines, 1):
        s = line.strip()
        if s.startswith("//"):
            continue
        is_const = bool(re.match(r"(pub(\([^)]*\))?\s+)?(const|static)\b", s))
        if is_const:
            continue
        for mm in num_re.finditer(strip_code(line)):
            v = int(mm.group(1))
            if v in BORING:
                continue
            magic[v] += 1
            if len(magic_sites[v]) < 5:
                magic_sites[v].append(f"{path}:{n}")

# 重复块：8 行窗口，跨文件比对（同文件内重复另算）
win = 8
seen = {}
dups = []
for path in rust_files():
    raw = production_lines(path)
    norm = [re.sub(r"\s+", " ", l).strip() for l in raw]
    for i in range(0, max(0, len(norm) - win)):
        chunk = [l for l in norm[i:i + win] if l and not l.startswith("//")]
        if len(chunk) < win - 1:
            continue
        # 全是 `use` 的窗口不算重复：二十个模块共享同一段导入头是 Rust 的常态，
        # 它不产生"改一处要改二十处"的维护成本（改的是 crate 路径，一次全局替换）。
        # 真正的重复是逻辑重复，而逻辑块里不可能只有 import。
        if all(l.startswith(("use ", "pub use ")) for l in chunk):
            continue
        h = hashlib.md5("\n".join(chunk).encode()).hexdigest()
        if h in seen and seen[h][0] != path:
            dups.append([seen[h], [path, i + 1]])
        elif h not in seen:
            seen[h] = [path, i + 1]

# 依赖图（区分生产依赖与 dev 依赖：反向边在测试里是正常的，不算架构缺陷）
deps = {}
dev_deps = {}
for crate in sorted(os.listdir("crates")):
    tom = os.path.join("crates", crate, "Cargo.toml")
    if not os.path.exists(tom):
        continue
    txt = open(tom, encoding="utf-8").read()
    dev_part = txt.split("[dev-dependencies]")[1] if "[dev-dependencies]" in txt else ""
    prod_part = txt.split("[dev-dependencies]")[0]
    deps[crate] = sorted(set(re.findall(r"wxlike-(\w+)\s*=\s*\{\s*path", prod_part)))
    dev_deps[crate] = sorted(set(re.findall(r"wxlike-(\w+)\s*=\s*\{\s*path", dev_part)))

report = {
    "total_files": len(files_info),
    "total_lines": sum(f[1] for f in files_info),
    "fn_total": len(fns),
    "count_len_gt30": sum(1 for r in fns if r["len"] > 30),
    "count_len_gt60": sum(1 for r in fns if r["len"] > 60),
    "count_len_gt100": sum(1 for r in fns if r["len"] > 100),
    "count_nest_gt3": sum(1 for r in fns if r["nest"] > 3),
    "count_mixed_sql_http": sum(1 for r in fns if r["sql"] and r["http"]),
    # 闸门用总量：top-20 只是展示，判断"有没有变差"要看全部
    "magic_total": sum(magic.values()),
    "magic_unique": len(magic),
    # 超长函数的"身份证"：数量不变但换了个函数变长，也要能看出来
    "long_fn_keys": sorted(
        f"{r['file']}::{r['name']}" for r in fns if r["len"] > 100
    ),
    "deep_fn_keys": sorted(
        f"{r['file']}::{r['name']}" for r in fns if r["nest"] > 3
    ),
    "largest_files": sorted(files_info, key=lambda x: -x[1])[:20],
    "long_fns": sorted([r for r in fns if r["len"] > 60], key=lambda r: -r["len"])[:35],
    "deep_fns": sorted([r for r in fns if r["nest"] > 3], key=lambda r: (-r["nest"], -r["len"]))[:25],
    "mixed_fns": sorted([r for r in fns if r["sql"] and r["http"]], key=lambda r: -r["len"])[:20],
    "magic_top": magic.most_common(20),
    "magic_sites": {str(k): magic_sites[k] for k, _ in magic.most_common(20)},
    "dup_windows": len(dups),
    "dup_sample": dups[:12],
    "deps": deps,
    "dev_deps": dev_deps,
}
print(json.dumps(report, ensure_ascii=False, indent=1))
