"""SBOM：按锁文件生成 CycloneDX 1.6（Rust / npm / Dart / 容器镜像）。

三条硬要求写在实现里：

1. **只认锁文件**（`Cargo.lock`、`package-lock.json`、`pubspec.lock`、Dockerfile
   / compose）。`Cargo.toml` 只有版本范围，用它生成 SBOM 等于写"大概是这个"。
2. **可复现**：不写生成时间、serialNumber 由组件集合派生。同一份锁跑两次必须
   字节相同——否则"重复生成"这件事无法被验证，diff 会永远噪声。
3. **许可未知就写 NOASSERTION**，不猜。SBOM 的价值在于"能追溯"，不在"填满"。
"""

from __future__ import annotations

import hashlib
import json
import re
import tomllib
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

from .model import Report, Severity
from .util import DataError, atomic_write, load_json, read_text, repo_root

SPEC_VERSION = "1.6"
TOOL_NAME = "wxlike-source-audit"
_NAMESPACE = uuid.UUID("6f9619ff-8b86-d011-b42d-00c04fc964ff")
_FROM_RE = re.compile(r"^\s*FROM\s+(?:--platform=\S+\s+)?([^\s]+)", re.IGNORECASE | re.MULTILINE)
_IMAGE_RE = re.compile(r"^\s*image:\s*[\"']?([^\"'\s#]+)", re.IGNORECASE | re.MULTILINE)


@dataclass(frozen=True)
class Component:
    type: str
    name: str
    version: str
    purl: str
    source: str
    checksum: str | None = None

    def to_dict(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "type": self.type,
            "bom-ref": self.purl,
            "name": self.name,
            "version": self.version,
            "purl": self.purl,
            "licenses": [{"license": {"name": "NOASSERTION"}}],
            "properties": [{"name": "wxlike:sbom:source", "value": self.source}],
        }
        if self.checksum:
            out["hashes"] = [{"alg": "SHA-256", "content": self.checksum}]
        return out

    def sort_key(self) -> tuple[str, str, str, str]:
        return (self.source, self.name, self.version, self.purl)


def collect(root: Path) -> list[Component]:
    comps: list[Component] = []
    comps.extend(_cargo(root))
    comps.extend(_npm(root))
    comps.extend(_dart(root))
    comps.extend(_images(root))
    # 去重（同一包可能被多个锁文件覆盖），保留首个来源并排序 → 可复现
    uniq: dict[str, Component] = {}
    for c in comps:
        uniq.setdefault(c.purl, c)
    return sorted(uniq.values(), key=Component.sort_key)


def _cargo(root: Path) -> Iterable[Component]:
    lock = root / "Cargo.lock"
    if not lock.is_file():
        return []
    data = tomllib.loads(read_text(lock))
    out = []
    for pkg in data.get("package", []):
        name, version = pkg.get("name"), pkg.get("version")
        if not name or not version:
            continue
        out.append(
            Component(
                type="library",
                name=name,
                version=version,
                purl=f"pkg:cargo/{name}@{version}",
                source="cargo-lock",
                checksum=_checksum(pkg.get("checksum")),
            )
        )
    return out


def _checksum(value: Any) -> str | None:
    """Cargo.lock 里的 checksum 是 sha256 的十六进制；不是就不要编一个。"""
    if isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value):
        return value
    return None


def _npm(root: Path) -> Iterable[Component]:
    out = []
    for rel in ("ui/package-lock.json", "design/ui-v2/package-lock.json"):
        path = root / rel
        if not path.is_file():
            continue
        data = load_json(path)
        for key, meta in (data.get("packages") or {}).items():
            if not key or not key.startswith("node_modules/"):
                continue
            name = meta.get("name") or key.rsplit("node_modules/", 1)[-1]
            version = meta.get("version")
            if not version:
                continue
            out.append(
                Component(
                    type="library",
                    name=name,
                    version=version,
                    purl=f"pkg:npm/{name.replace('/', '%2F')}@{version}",
                    source=f"npm-lock:{rel}",
                )
            )
    return out


def _dart(root: Path) -> Iterable[Component]:
    """`pubspec.lock` 是 YAML，但结构极其固定；用 15 行状态机解析，
    比为它引一个 YAML 依赖更划算（CI 的 Python 环境不想再装包）。"""
    path = root / "apps/mobile/pubspec.lock"
    if not path.is_file():
        return []
    out = []
    name: str | None = None
    for line in read_text(path).splitlines():
        if re.match(r"^  [A-Za-z0-9_]+:\s*$", line):
            name = line.strip().rstrip(":")
            continue
        m = re.match(r"^    version:\s*[\"']?([^\"'\s]+)[\"']?", line)
        if m and name and name != "root":
            version = m.group(1)
            out.append(
                Component(
                    type="library",
                    name=name,
                    version=version,
                    purl=f"pkg:pub/{name}@{version}",
                    source="pubspec-lock",
                )
            )
            name = None
    return out


def _images(root: Path) -> Iterable[Component]:
    out = []
    dockerfile = root / "Dockerfile"
    if dockerfile.is_file():
        for ref in _FROM_RE.findall(read_text(dockerfile)):
            comp = _image_component(ref, "dockerfile")
            if comp:
                out.append(comp)
    compose = root / "docker-compose.yml"
    if compose.is_file():
        for ref in _IMAGE_RE.findall(read_text(compose)):
            comp = _image_component(ref, "docker-compose")
            if comp:
                out.append(comp)
    return out


def _image_component(ref: str, source: str) -> Component | None:
    if ref.lower() == "scratch":
        return None
    # `rust:1-bookworm AS build` 的别名已经在正则里排除；这里是镜像引用本身
    if "@sha256:" in ref:
        name, digest = ref.split("@sha256:", 1)
        return Component("container", name.split("/")[-1], "sha256:" + digest[:16], f"pkg:docker/{name}?digest=sha256:{digest}", source)
    name, _, tag = ref.partition(":")
    return Component("container", name.split("/")[-1], tag or "latest", f"pkg:docker/{name}@{tag or 'latest'}", source)


def build(root: Path) -> dict[str, Any]:
    comps = collect(root)
    if not comps:
        raise DataError("没有从任何锁文件解析出组件：SBOM 不能是空表")
    digest = hashlib.sha256("\n".join(c.purl for c in comps).encode("utf-8")).hexdigest()
    sources = sorted({c.source.split(":")[0] for c in comps})
    return {
        "bomFormat": "CycloneDX",
        "specVersion": SPEC_VERSION,
        "serialNumber": f"urn:uuid:{uuid.uuid5(_NAMESPACE, digest)}",
        "version": 1,
        "metadata": {
            "tools": [{"name": TOOL_NAME, "version": "1.0"}],
            "component": {
                "type": "application",
                "name": "wxlike",
                "bom-ref": "pkg:generic/wxlike",
            },
            "properties": [
                {"name": "wxlike:sbom:component-count", "value": str(len(comps))},
                {"name": "wxlike:sbom:sources", "value": ",".join(sources)},
                {"name": "wxlike:sbom:reproducible", "value": "serialNumber derived from component purls"},
            ],
        },
        "components": [c.to_dict() for c in comps],
    }


def render(bom: dict[str, Any]) -> str:
    """一行一个组件的 JSON。

    为什么不用 `indent=2` 全量展开：777 个组件的缩进版本是 2 万多行，
    每次依赖变化 `git diff` 都变成一堵墙，评审会直接跳过。一行一个组件时，
    改动就是"哪几行变了"，人真的会看。
    """
    head = {k: v for k, v in bom.items() if k != "components"}
    lines = ["{"]
    for key, value in head.items():
        lines.append(f'  "{key}": {json.dumps(value, ensure_ascii=False, sort_keys=True)},')
    lines.append('  "components": [')
    comps = [json.dumps(c, ensure_ascii=False, sort_keys=True) for c in bom["components"]]
    lines.append(",\n".join(f"    {c}" for c in comps))
    lines.append("  ]")
    lines.append("}")
    return "\n".join(lines) + "\n"


def write(root: Path, out: Path) -> tuple[bool, int]:
    bom = build(root)
    changed = atomic_write(out, render(bom))
    return changed, len(bom["components"])


def check(root: Path | None = None, committed: Path | None = None) -> Report:
    root = root or repo_root()
    committed = committed or root / "third_party/reviews/sbom/wxlike.cdx.json"
    report = Report("sbom", meta={"committed": str(committed.relative_to(root))})
    if not committed.is_file():
        report.fail("SBOM-MISSING", str(committed.name), "仓库里没有已提交的 SBOM：生成一次并提交", Severity.HIGH)
        return report
    fresh = build(root)
    stored = load_json(committed)
    fresh_refs = {c["bom-ref"] for c in fresh["components"]}
    stored_refs = {c["bom-ref"] for c in stored.get("components", [])}
    added = sorted(fresh_refs - stored_refs)
    removed = sorted(stored_refs - fresh_refs)
    for ref in added[:20]:
        report.fail("SBOM-DRIFT-ADD", ref, "锁文件里有、SBOM 里没有：依赖变了但 SBOM 没重生成", Severity.MEDIUM)
    for ref in removed[:20]:
        report.fail("SBOM-DRIFT-REMOVE", ref, "SBOM 里有、锁文件里没有：SBOM 过期", Severity.MEDIUM)
    if not added and not removed:
        report.ok("SBOM-SYNC", "components", f"{len(fresh_refs)} 个组件一致")
    report.meta["components"] = len(fresh_refs)
    return report
