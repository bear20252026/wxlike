"""底层工具：仓库定位、文件哈希、JSON 读取、越界拒绝、原子写。

这里的每个函数都**只做一件事**，因为门禁脚本出错时的代价是"看起来通过了"。
所以宁可多抛 `DataError`，也不做"猜一个默认值继续跑"。
"""

from __future__ import annotations

import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any

# 退出码约定（CI 与本地都要能区分"策略拦了"和"工具自己坏了"）：
# 0 通过 · 2 参数/用法错误 · 3 策略不通过 · 4 数据缺失/网络不可达/超时
EXIT_OK = 0
EXIT_USAGE = 2
EXIT_BLOCK = 3
EXIT_DATA = 4

_HEX_RE = re.compile(r"^[0-9a-f]+$")


class AuditError(Exception):
    """工具无法给出结论（区别于"结论是不通过"）。"""

    exit_code = EXIT_DATA


class UsageError(AuditError):
    exit_code = EXIT_USAGE


class DataError(AuditError):
    exit_code = EXIT_DATA


def repo_root(start: Path | None = None) -> Path:
    """从 start 向上找带 `Cargo.toml` + `crates/` 的目录。

    不接受"当前目录就算根"的猜测：门禁在错误的根上跑，所有相对路径都会
    指向不存在的文件，最后表现成"全绿"。
    """
    here = (start or Path(__file__).resolve()).resolve()
    for cand in [here, *here.parents]:
        if (cand / "Cargo.toml").is_file() and (cand / "crates").is_dir():
            return cand
    raise DataError(f"找不到仓库根（从 {here} 向上无 Cargo.toml + crates/）")


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise DataError(f"文件不存在：{path}") from exc
    except UnicodeDecodeError as exc:
        raise DataError(f"文件不是 UTF-8：{path}（{exc}）") from exc


def load_json(path: Path) -> Any:
    """读 JSON。文件缺失/语法错都是 DataError——不是"空结果"。"""
    raw = read_text(path)
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise DataError(f"JSON 语法错误：{path}:{exc.lineno}:{exc.colno} {exc.msg}") from exc


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def is_hex(value: str, lengths: tuple[int, ...] = (40, 64)) -> bool:
    return isinstance(value, str) and len(value) in lengths and bool(_HEX_RE.match(value))


def resolve_inside(root: Path, relative: str) -> Path:
    """把 registry 里的相对路径解析成仓库内路径；越界一律拒绝。

    这是"未授权"面的核心：登记表是数据文件，数据文件不能成为读写仓库外
    任意文件的手段。符号链接也要解析后判（`resolve()` 已经跟随链接）。
    """
    if not relative or relative.strip() != relative:
        raise UsageError(f"非法路径：{relative!r}")
    if Path(relative).is_absolute() or relative.startswith(("\\", "/")) or re.match(r"^[A-Za-z]:", relative):
        raise UsageError(f"必须是仓库内相对路径：{relative}")
    root = root.resolve()
    target = (root / relative).resolve()
    try:
        target.relative_to(root)
    except ValueError as exc:
        raise UsageError(f"路径越界（不在仓库内）：{relative} -> {target}") from exc
    return target


def atomic_write(path: Path, text: str) -> bool:
    """原子写；内容相同则不落盘。返回是否真的写了。

    返回 False 是"重复提交"的判据：同一输入跑两次，第二次必须是 False，
    否则说明输出里混进了时间戳/随机数，产物无法复现。
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    data = text.encode("utf-8")
    if path.exists() and path.read_bytes() == data:
        return False
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_bytes(data)
    os.replace(tmp, path)
    return True


def parse_date(value: str) -> "datetime.date":
    from datetime import date

    try:
        return date.fromisoformat(value)
    except (TypeError, ValueError) as exc:
        raise DataError(f"日期格式必须 YYYY-MM-DD：{value!r}") from exc


def today() -> "datetime.date":
    from datetime import date

    return date.today()
