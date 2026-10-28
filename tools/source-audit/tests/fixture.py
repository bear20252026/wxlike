"""测试夹具：造一个最小可审计仓库。

用 tmp 目录而不是真仓库，是为了让"负向用例"能改坏数据——真仓库里的
registry 必须一直是好的，否则门禁本身没有意义。
"""

from __future__ import annotations

import hashlib
import json
import shutil
import tempfile
from pathlib import Path

SRC_FILE = "console.log('vendored');\n"


class FakeRepo:
    def __init__(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="sourceaudit-"))
        (self.root / "crates/demo/src").mkdir(parents=True)
        (self.root / "crates/demo/src/lib.rs").write_text("// demo\n", encoding="utf-8")
        (self.root / "crates/demo/Cargo.toml").write_text(
            '[package]\nname = "wxlike-demo"\nversion = "0.1.0"\nedition = "2021"\n\n'
            '[dependencies]\nserde = "1"\n',
            encoding="utf-8",
        )
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/demo"]\n', encoding="utf-8")
        (self.root / "Cargo.lock").write_text(
            'version = 3\n\n[[package]]\nname = "serde"\nversion = "1.0.200"\n'
            'checksum = "' + "a" * 64 + '"\n',
            encoding="utf-8",
        )
        (self.root / "ui/src/vendor").mkdir(parents=True)
        self.vendored = self.root / "ui/src/vendor/x.ts"
        # 必须按字节写：文本模式在 Windows 会把换行翻成 CRLF，sha256 立刻对不上
        self.vendored.write_bytes(SRC_FILE.encode("utf-8"))
        (self.root / "third_party/reviews/patches").mkdir(parents=True)
        (self.root / "third_party/reviews/patches/UP-DEMO-x.md").write_text(
            "# UP-DEMO x.ts\n\n- 改了什么：无（原样）\n- 复核：sha256\n", encoding="utf-8"
        )
        self.write_json("third_party/reviews/exclusions.json", {"rules": [{"id": "EXCL-ENTERPRISE", "note": "专有子树"}]})
        self.registry = {
            "schema": "wxlike.provenance.v1",
            "upstreams": [
                {
                    "id": "UP-DEMO",
                    "repo": "demo/demo",
                    "url": "https://github.com/demo/demo",
                    "commit": "b" * 40,
                    "license": "MIT",
                    "license_spdx": "MIT",
                    "license_evidence": "https://github.com/demo/demo/blob/" + "b" * 40 + "/LICENSE",
                }
            ],
            "files": [
                {
                    "path": "ui/src/vendor/x.ts",
                    "upstream": "UP-DEMO",
                    "upstream_path": "src/x.ts",
                    "decision": "ported",
                    "license_spdx": "MIT",
                    "local_sha256": hashlib.sha256(SRC_FILE.encode()).hexdigest(),
                    "patchlog": "third_party/reviews/patches/UP-DEMO-x.md",
                }
            ],
        }
        self.write_json("third_party/reviews/registry.json", self.registry)

    # ---- 操作 -------------------------------------------------------------
    def write_json(self, rel: str, data: object) -> Path:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        return path

    def load_json(self, rel: str):
        return json.loads((self.root / rel).read_text(encoding="utf-8"))

    def rm(self, rel: str) -> None:
        path = self.root / rel
        if path.is_dir():
            shutil.rmtree(path)
        elif path.exists():
            path.unlink()

    def cleanup(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)
