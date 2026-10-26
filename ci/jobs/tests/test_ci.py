#!/usr/bin/env python3
"""Unit tests for the CI control plane (no network, no Docker)."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
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


if __name__ == "__main__":
    unittest.main()
