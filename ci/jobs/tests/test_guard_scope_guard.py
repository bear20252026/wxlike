"""守卫的守卫（第四轮审计 N1 的结构性修复）：扫描范围必须覆盖所有 async+DB crate。

与 test_dep_graph_guard.py 同款三层：合成 fixture 注入红绿 + 真仓库必须绿。
不证自明不算证据——未登记 / 假覆盖 / 无理由豁免 / 陈旧条目，注入一条必须非零。
"""

from __future__ import annotations

import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts" / "guard_scope_guard.py"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GUARD = load("guard_scope_guard", SCRIPT)

# 一个最小 ADR-28 守卫替身：只暴露范围守卫用到的两个常量。
ADR28_STUB = '''
SCAN_ROOTS = ("crates/platform/src", "crates/admin/src")
SKIP_DIR_PARTS = ("/ffi/", "/ui/")
'''

MANIFEST = '''[package]
name = "{name}"
version = "0.1.0"

[dependencies]
tokio = {{ workspace = true }}
rusqlite = {{ workspace = true }}
'''


def make_root(entries, crates=("demo",), adr28=ADR28_STUB):
    root = pathlib.Path(tempfile.mkdtemp(prefix="guard-scope-"))
    (root / "scripts").mkdir()
    (root / "scripts" / "adr28_async_db_guard.py").write_text(adr28, encoding="utf-8")
    (root / "scripts" / "guard-scope-ledger.json").write_text(
        json.dumps({"schema": 1, "crates": entries}, ensure_ascii=False), encoding="utf-8"
    )
    for name in crates:
        src = root / "crates" / name / "src"
        src.mkdir(parents=True)
        (root / "crates" / name / "Cargo.toml").write_text(
            MANIFEST.format(name=name), encoding="utf-8"
        )
        (src / "lib.rs").write_text("// demo\n", encoding="utf-8")
    return root


def entry(name, status, src=None, reason=None):
    e = {
        "crate": name,
        "manifest": f"crates/{name}/Cargo.toml",
        "src": src or f"crates/{name}/src",
        "status": status,
    }
    if reason is not None:
        e["reason"] = reason
    return e


class GuardScopeTest(unittest.TestCase):
    def _run(self, root):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(root)],
            capture_output=True,
            text=True,
        )

    def test_unregistered_async_db_crate_goes_red(self):
        # demo 含 tokio+rusqlite 但台账为空 → 必须红。
        root = make_root([])
        red = self._run(root)
        self.assertNotEqual(red.returncode, 0, "未登记的 async+DB crate 竟然没报错")
        self.assertIn("未在", red.stderr)
        # 登记为 covered（src 在 SCAN_ROOTS 内）→ 回绿。
        (root / "scripts" / "guard-scope-ledger.json").write_text(
            json.dumps({"crates": [entry("demo", "covered")]}), encoding="utf-8"
        )
        # demo 的 src 不在 stub 的 SCAN_ROOTS 里，先把 stub 扩到 crates/demo/src
        (root / "scripts" / "adr28_async_db_guard.py").write_text(
            ADR28_STUB.replace('("crates/platform/src", "crates/admin/src")',
                               '("crates/platform/src", "crates/admin/src", "crates/demo/src")'),
            encoding="utf-8",
        )
        green = self._run(root)
        self.assertEqual(green.returncode, 0, green.stderr)

    def test_covered_but_not_scanned_goes_red(self):
        # 台账说 covered，但 src 不在 SCAN_ROOTS → 红（假覆盖）。
        root = make_root([entry("demo", "covered")])
        red = self._run(root)
        self.assertNotEqual(red.returncode, 0)
        self.assertIn("covered", red.stderr)

    def test_exempt_without_reason_goes_red(self):
        # exempt 但 src 实际被扫到、且没理由 → 红。
        stub = ADR28_STUB.replace(
            '("crates/platform/src", "crates/admin/src")',
            '("crates/platform/src", "crates/admin/src", "crates/demo/src")',
        )
        root = make_root([entry("demo", "exempt")], adr28=stub)
        red = self._run(root)
        self.assertNotEqual(red.returncode, 0)
        self.assertIn("理由", red.stderr)

    def test_exempt_consistent_with_skip_is_green(self):
        # src 被 SKIP 跳过 + 有理由 → 绿（豁免与事实一致）。
        stub = 'SCAN_ROOTS = ("crates",)\nSKIP_DIR_PARTS = ("/demo/",)\n'
        root = make_root([entry("demo", "exempt", reason="客户端形状，另行治理")], adr28=stub)
        green = self._run(root)
        self.assertEqual(green.returncode, 0, green.stderr)

    def test_stale_entry_goes_red(self):
        # 台账登记了一个不存在的 crate → 红（陈旧）。
        root = make_root([entry("demo", "covered"), entry("ghost", "covered")])
        red = self._run(root)
        self.assertNotEqual(red.returncode, 0)
        self.assertIn("陈旧", red.stderr)

    def test_dev_only_tokio_does_not_count(self):
        # tokio 只在 dev-dependencies → 不算生产 async+DB，无需登记。
        root = make_root([])
        (root / "crates" / "demo" / "Cargo.toml").write_text(
            '[package]\nname = "demo"\n\n[dependencies]\nrusqlite = { workspace = true }\n'
            '\n[dev-dependencies]\ntokio = { workspace = true }\n',
            encoding="utf-8",
        )
        green = self._run(root)
        self.assertEqual(green.returncode, 0, green.stderr)


class RealRepoScopeTest(unittest.TestCase):
    def test_real_repo_scope_is_clean(self):
        problems = GUARD.check(ROOT)
        self.assertEqual(problems, [], "真仓库的扫描范围台账与事实不符")

    def test_ledger_covers_admin_after_n1(self):
        # N1 的结构性修复必须留痕：admin 在台账里且是 covered。
        ledger = json.loads(
            (ROOT / "scripts" / "guard-scope-ledger.json").read_text(encoding="utf-8")
        )
        admin = next(e for e in ledger["crates"] if e["crate"] == "wxlike-admin")
        self.assertEqual(admin["status"], "covered")


if __name__ == "__main__":
    unittest.main()