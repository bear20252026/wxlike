#!/usr/bin/env python3
"""生成 Dart 绑定（UniFFI）——**生成物不手改**。

    python tools/codegen/dart/gen_dart.py            # 写到 apps/mobile/lib/src/rust/
    python tools/codegen/dart/gen_dart.py --out DIR  # 写到指定目录（G03 交接时用）
    python tools/codegen/dart/gen_dart.py --check     # 重新生成到临时目录并与仓库比对

为什么还要一层包装：`crates/ffi/regenerate.sh` 是"人手动跑"的脚本（写死输出
目录、要 build cdylib、要 dart format）。CI 需要的是**可检查**的一步：
生成到临时目录、与仓库里的文件逐字节比对，不一致就红。

契约摘要（`tools/codegen/out/manifest.json`）会写进生成的 `manifest.txt`：
Dart 侧据此断言"绑定与契约同源"。
"""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[3]
FFI_DIR = ROOT / "crates" / "ffi"
DEFAULT_OUT = ROOT / "apps" / "mobile" / "lib" / "src" / "rust"
MANIFEST = ROOT / "tools" / "codegen" / "out" / "manifest.json"
LIB_NAME = {
    "Linux": "libwxlike_ffi.so",
    "Darwin": "libwxlike_ffi.dylib",
    "Windows": "wxlike_ffi.dll",
}


def contract_stamp() -> str:
    if not MANIFEST.is_file():
        return "manifest.json 缺失（先跑 tools/codegen/generate.py）"
    doc = json.loads(MANIFEST.read_text(encoding="utf-8"))
    return f"{doc['schema_version']} digest={doc['digest']} contract={doc['contract']}"


def cdylib_path() -> pathlib.Path:
    target = pathlib.Path(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    doc = json.loads(target.read_text() if hasattr(target, "read_text") else target)
    target_dir = pathlib.Path(doc["target_directory"])
    platform = subprocess.run(["uname", "-s"], capture_output=True, text=True).stdout.strip()
    name = LIB_NAME.get(platform, "wxlike_ffi.dll")
    return target_dir / "debug" / name


def build_and_generate(out_dir: pathlib.Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["cargo", "build", "-p", "wxlike-ffi", "--features", "bindgen,dart"],
        cwd=ROOT,
        check=True,
    )
    lib = cdylib_path()
    if not lib.is_file():
        raise SystemExit(f"找不到 cdylib：{lib}")
    subprocess.run(
        [
            "cargo",
            "run",
            "-q",
            "-p",
            "wxlike-ffi",
            "--features",
            "bindgen,dart",
            "--bin",
            "uniffi-bindgen-dart",
            "--",
            "generate",
            str(lib),
            "--out-dir",
            str(out_dir),
            "--config",
            str(FFI_DIR / "uniffi.toml"),
        ],
        cwd=ROOT,
        check=True,
    )
    (out_dir / "manifest.txt").write_text(contract_stamp() + "\n", encoding="utf-8")


def dart_files(directory: pathlib.Path) -> dict[str, bytes]:
    if not directory.is_dir():
        return {}
    return {
        path.name: path.read_bytes()
        for path in sorted(directory.glob("*.dart"))
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="生成 Dart 绑定")
    parser.add_argument("--out", type=pathlib.Path, default=DEFAULT_OUT)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    if args.check:
        with tempfile.TemporaryDirectory() as tmp:
            fresh = pathlib.Path(tmp)
            build_and_generate(fresh)
            expected = dart_files(fresh)
            actual = dart_files(args.out)
            missing = sorted(set(expected) - set(actual))
            stale = sorted(name for name in expected if actual.get(name) != expected[name])
            if missing or stale:
                print("Dart 绑定已过期：", file=sys.stderr)
                for name in missing:
                    print(f"  - 缺失 {name}", file=sys.stderr)
                for name in stale:
                    print(f"  - 与 FFI 接口不一致 {name}", file=sys.stderr)
                return 1
        print("Dart 绑定与 FFI 接口一致")
        return 0

    build_and_generate(args.out)
    print(f"生成到 {args.out}（{contract_stamp()}）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
