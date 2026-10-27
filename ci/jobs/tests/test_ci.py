#!/usr/bin/env python3
"""Unit tests for the CI control plane (no network, no Docker)."""

from __future__ import annotations

import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
JOBS = ROOT / "ci" / "jobs"


def load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def load_optional(name: str, path: Path):
    return load(name, path) if path.is_file() else None


select_targets = load("select_targets", JOBS / "build" / "select_targets.py")
gate = load("gate", JOBS / "build" / "gate.py")
device = load_optional("check_device_evidence", JOBS / "mobile" / "check_device_evidence.py")
apk_gate = load_optional("check_apk", JOBS / "mobile" / "check_apk.py")


class PathSelectionTest(unittest.TestCase):
    def setUp(self):
        self.policy = json.loads((JOBS / "policy" / "paths.json").read_text(encoding="utf-8"))

    def test_docs_only_selects_nothing(self):
        result = select_targets.classify(["docs/guide.md", "design/ui-v2/a.png"], self.policy)
        self.assertFalse(result["full"])
        self.assertFalse(any(result["targets"].values()))

    def test_contract_change_forces_full_matrix(self):
        result = select_targets.classify(["contracts/proto/seed.csv"], self.policy)
        self.assertTrue(result["full"])
        self.assertTrue(all(result["targets"].values()))

    def test_unknown_path_forces_full_matrix(self):
        result = select_targets.classify(["mystery/new.bin"], self.policy)
        self.assertTrue(result["full"])

    def test_server_change_selects_rust_only(self):
        result = select_targets.classify(["server/src/main.rs"], self.policy)
        self.assertFalse(result["full"])
        self.assertTrue(result["targets"]["rust"])
        self.assertFalse(result["targets"]["ui"])


class GateTest(unittest.TestCase):
    def needs(self, results: dict[str, str]) -> dict:
        required = json.loads((JOBS / "policy" / "required-checks.json").read_text(encoding="utf-8"))["required"]
        return {name: {"result": results.get(name, "success")} for name in required if name != "gate"}

    def run_gate(self, needs: dict) -> subprocess.CompletedProcess:
        env = dict(os.environ, NEEDS_JSON=json.dumps(needs))
        env.pop("GITHUB_STEP_SUMMARY", None)
        return subprocess.run([sys.executable, str(JOBS / "build" / "gate.py")], env=env, capture_output=True, text=True)

    def test_all_success_passes(self):
        self.assertEqual(self.run_gate(self.needs({})).returncode, 0)

    def test_path_selected_skips_pass(self):
        self.assertEqual(self.run_gate(self.needs({"ui": "skipped", "flutter": "skipped"})).returncode, 0)

    def test_failure_blocks(self):
        result = self.run_gate(self.needs({"test": "failure"}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("not green", result.stderr)

    def test_abandoned_matrix_leg_blocks(self):
        # 矩阵里有一条被取消时 needs 给的是 `abandoned`，不是 failure/cancelled。
        # 2026-10-26 native (macos-latest) 就是这样让 gate 判绿的。
        result = self.run_gate(self.needs({"native": "abandoned"}))
        self.assertEqual(result.returncode, 1)
        self.assertIn("abandoned", result.stderr)

    def test_missing_required_job_blocks(self):
        needs = self.needs({})
        needs.pop("audit")
        self.assertEqual(self.run_gate(needs).returncode, 1)


@unittest.skipUnless(device is not None, "G16-03 assets not in this branch")
class DeviceEvidenceTest(unittest.TestCase):
    def test_accepts_complete_evidence_and_rejects_placeholder(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "push.png").write_bytes(b"png")
            matrix = {
                "devices": [{"id": "dev1", "platform": "android", "model": "Pixel", "os_version": "14"}],
                "features": ["push"],
            }
            evidence = {
                "schema_version": 1,
                "build": {"git_sha": "a" * 40, "version": "1.0.0+1", "artifact_sha256": "b" * 64},
                "devices": [
                    {
                        "id": "dev1",
                        "platform": "android",
                        "model": "Pixel",
                        "os_version": "14",
                        "results": [
                            {
                                "feature": "push",
                                "status": "pass",
                                "evidence": "push.png",
                                "observed_at": "2026-10-24T10:00:00Z",
                            }
                        ],
                    }
                ],
            }
            matrix_path = root / "matrix.json"
            evidence_path = root / "evidence.json"
            matrix_path.write_text(json.dumps(matrix), encoding="utf-8")
            evidence_path.write_text(json.dumps(evidence), encoding="utf-8")
            argv = ["--evidence", str(evidence_path), "--matrix", str(matrix_path)]
            self.assertEqual(device.main(argv), 0)

            evidence["devices"][0]["results"][0]["status"] = "fail"
            evidence_path.write_text(json.dumps(evidence), encoding="utf-8")
            self.assertEqual(device.main(argv), 1)

            evidence["devices"][0]["results"][0]["status"] = "pass"
            evidence["devices"][0]["model"] = "TBD"
            evidence_path.write_text(json.dumps(evidence), encoding="utf-8")
            self.assertEqual(device.main(argv), 1)


@unittest.skipUnless((JOBS / "desktop" / "make_update_manifest.py").is_file(), "G16-02 assets not in this branch")
class ManifestMergeTest(unittest.TestCase):
    def test_merge_requires_signatures(self):
        script = JOBS / "desktop" / "make_update_manifest.py"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "linux.json").write_text(
                json.dumps(
                    {
                        "version": "1.0.0",
                        "pub_date": "2026-10-24T00:00:00Z",
                        "platforms": {"linux-x86_64": {"url": "https://x/a", "size": 1, "sha256": "a" * 64}},
                        "artifacts": [],
                    }
                ),
                encoding="utf-8",
            )
            out = root / "merged.json"
            unsigned = subprocess.run(
                [sys.executable, str(script), "--merge", str(root / "linux.json"), "--require-signature", "--output", str(out)],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(unsigned.returncode, 0)

            data = json.loads((root / "linux.json").read_text(encoding="utf-8"))
            data["platforms"]["linux-x86_64"]["signature"] = "sig"
            (root / "linux.json").write_text(json.dumps(data), encoding="utf-8")
            signed = subprocess.run(
                [sys.executable, str(script), "--merge", str(root / "linux.json"), "--require-signature", "--output", str(out)],
                capture_output=True,
                text=True,
            )
            self.assertEqual(signed.returncode, 0, signed.stderr)
            self.assertIn("linux-x86_64", json.loads(out.read_text(encoding="utf-8"))["platforms"])


class WireVariantTest(unittest.TestCase):
    def test_rust_and_browser_orders_match(self):
        result = subprocess.run(
            [sys.executable, str(JOBS / "contract" / "check_wire_variants.py")],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # check_wire_variants prints "<Enum>: N variants in order" only for
        # matching pairs; the exact N is contract data (it grows as variants
        # are added), so assert the match, not the count.
        for name in ("WireFrame", "MsgBody", "Receipt"):
            self.assertIn(f"{name}: ", result.stdout)
            self.assertIn("variants in order", result.stdout)


class ContractStructureTest(unittest.TestCase):
    def test_seed_and_contracts_parse(self):
        result = subprocess.run(
            [sys.executable, str(JOBS / "contract" / "check_contracts.py")],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("seed rows=331", result.stdout)


class ComposeTest(unittest.TestCase):
    def test_composed_workflows_are_current_and_policy_clean(self):
        result = subprocess.run(
            [sys.executable, str(JOBS / "compose.py"), "--check"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


@unittest.skipUnless((ROOT / "deploy" / "production" / "check-log-redaction.py").is_file(), "G16-05 assets not in this branch")
class RedactionTest(unittest.TestCase):
    def test_self_test(self):
        result = subprocess.run(
            [sys.executable, str(ROOT / "deploy" / "production" / "check-log-redaction.py"), "--self-test"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


@unittest.skipUnless(apk_gate is not None, "check_apk.py not in this branch")
class NativeLibGateTest(unittest.TestCase):
    """#292：release 产物里少一个 libwxlike_ffi.so 就必须判红（**能红**才算门禁）。

    这些用例不依赖 Android SDK：打包面在 aapt/apksigner 之前跑，所以门禁在
    任何机器上都是同一条判据。
    """

    ABI_DIRS = ["lib/arm64-v8a", "lib/armeabi-v7a", "lib/x86_64"]

    def zip_artifact(self, path: Path, prefixes: list[str], *, ffi: list[str] | None = None, bundle: bool = False) -> Path:
        """Write a >1MB archive shaped like a Flutter APK/AAB.

        `prefixes` 是能跑的 ABI 目录；`ffi` 决定其中哪几个真的打进了
        `libwxlike_ffi.so`（默认全打）。AAB 把它们嵌在 `base/` 下。
        """
        with_ffi = set(prefixes if ffi is None else ffi)
        with zipfile.ZipFile(path, "w", zipfile.ZIP_STORED) as archive:
            for prefix in prefixes:
                head = f"base/{prefix}" if bundle else prefix
                archive.writestr(f"{head}/libapp.so", b"\x7fELF")
                archive.writestr(f"{head}/libflutter.so", b"\x7fELF")
                if prefix in with_ffi:
                    archive.writestr(f"{head}/libwxlike_ffi.so", b"\x7fELF")
            # check_apk 用体积挡"根本没打出来"的产物，fixture 得过这条线。
            archive.writestr("assets/pad.bin", bytes(1_200_000))
        return path

    def run_gate(self, apk: Path, output: Path, aab: Path | None = None) -> subprocess.CompletedProcess:
        argv = [
            sys.executable,
            str(JOBS / "mobile" / "check_apk.py"),
            "--apk", str(apk),
            "--expected-version", "1.0.0+1",
            "--require-native-lib", "libwxlike_ffi.so",
            "--output", str(output),
        ]
        if aab is not None:
            argv += ["--aab", str(aab)]
        return subprocess.run(argv, cwd=ROOT, capture_output=True, text=True)

    def test_apk_without_the_native_lib_is_red(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            apk = self.zip_artifact(root / "app-release.apk", self.ABI_DIRS, ffi=[])
            result = self.run_gate(apk, root / "evidence.json")
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("libwxlike_ffi.so is missing", result.stderr)
            self.assertIn("the native cross-compile step did not run", result.stderr)
            self.assertFalse((root / "evidence.json").exists(), "a red gate must not emit evidence")

    def test_one_missing_abi_is_enough_to_be_red(self):
        # 只补一个 ABI 也不行：每一个能跑的 ABI 都得带上这颗 .so。
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            apk = self.zip_artifact(root / "app-release.apk", self.ABI_DIRS, ffi=["lib/arm64-v8a"])
            result = self.run_gate(apk, root / "evidence.json")
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            # 精确到"缺的就是这两个"，而不是消息里恰好出现过这两个目录名。
            self.assertIn("is missing for ['lib/armeabi-v7a', 'lib/x86_64']", result.stderr)

    def test_aab_without_the_native_lib_is_red(self):
        # APK 绿不代表 AAB 绿：上架走的是 AAB，它把库放在 base/lib/<abi>/。
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            apk = self.zip_artifact(root / "app-release.apk", self.ABI_DIRS)
            aab = self.zip_artifact(root / "app-release.aab", self.ABI_DIRS, ffi=[], bundle=True)
            result = self.run_gate(apk, root / "evidence.json", aab=aab)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("app-release.aab", result.stderr)
            self.assertIn("libwxlike_ffi.so is missing", result.stderr)

    def test_complete_artifacts_clear_the_packaging_gate(self):
        # 正向对照：库齐了就得放行，否则上面三条红的是脚本自己坏了。
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            apk = self.zip_artifact(root / "app-release.apk", self.ABI_DIRS)
            aab = self.zip_artifact(root / "app-release.aab", self.ABI_DIRS, bundle=True)
            with zipfile.ZipFile(apk) as archive:
                entries = set(archive.namelist())
            self.assertEqual(apk_gate.require_native_libs(apk, entries, ["libwxlike_ffi.so"]), sorted(self.ABI_DIRS))
            with zipfile.ZipFile(aab) as archive:
                bundle_entries = set(archive.namelist())
            self.assertEqual(
                apk_gate.require_native_libs(aab, bundle_entries, ["libwxlike_ffi.so"]),
                sorted(f"base/{d}" for d in self.ABI_DIRS),
            )

    def test_abis_without_a_dart_snapshot_are_not_required(self):
        # 插件 AAR 会把自家 .so 铺满所有 ABI 目录，但没 libapp.so 的目录跑不起来，
        # 不该拿它们来要求 ffi 库——与 #292 之前 CI 修复的口径保持一致。
        entries = {
            "lib/arm64-v8a/libapp.so",
            "lib/arm64-v8a/libwxlike_ffi.so",
            "lib/armeabi-v7a/libplugin.so",
            "lib/x86_64/libplugin.so",
        }
        self.assertEqual(apk_gate.runtime_lib_dirs(entries), ["lib/arm64-v8a"])
        apk_gate.require_native_libs(Path("fixture.apk"), entries, ["libwxlike_ffi.so"])


class MergedManifestScopeTest(unittest.TestCase):
    """android 作业里"合并清单必须带 INTERNET"这条，搜索范围必须锁在 app 模块内。

    真事：#303 上这条红过，而 APK 是 65.8MB 正常构建、权限也没丢。原因是步骤写成
    `find apps/mobile/build -name AndroidManifest.xml -path "*release*" | head -1`——
    每个 library 模块（jni、jni_flutter…）在 `build/<plugin>/intermediates/**/release/`
    下也各生成一份合并清单，而它们的清单里本来就没有 INTERNET。本机一次真实构建里
    这样的候选有 8 个、其中 4 个没有 INTERNET ⇒ `head -1` 撞上谁全凭目录顺序，
    相当于这条门禁有一半概率**假红**（也有同样概率只是运气好）。
    """

    FINDS_MANIFEST = re.compile(
        r"find\s+(?P<dir>\S+)[^|\n]*-name\s+[\"']?AndroidManifest\.xml"
    )

    def test_manifest_lookups_are_scoped_to_the_app_module(self):
        offenders = []
        for fragment in sorted(JOBS.glob("*/job.yml")):
            for number, line in enumerate(
                fragment.read_text(encoding="utf-8").splitlines(), 1
            ):
                if line.lstrip().startswith("#"):
                    # 注释行里可能写着反面教材（本仓就有一条写着旧命令长什么样），
                    # 这条测试管的是会执行的那一行。
                    continue
                match = self.FINDS_MANIFEST.search(line)
                if not match:
                    continue
                normalized = match.group("dir").replace("\\", "/")
                if "build/app" not in normalized:
                    offenders.append(f"{fragment.relative_to(JOBS)}:{number}: {normalized}")
        self.assertEqual(
            offenders,
            [],
            "上面这些 find 在搜 AndroidManifest.xml 但没有限定 app 模块；"
            "library 模块的合并清单里没有 INTERNET，会把门禁变成掷硬币。",
        )


if __name__ == "__main__":
    unittest.main()
