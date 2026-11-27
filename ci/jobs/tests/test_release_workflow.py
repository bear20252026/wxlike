#!/usr/bin/env python3
"""H-44 regression tests for release input handling and workflow gates."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
MODULE_PATH = ROOT / "ci" / "jobs" / "release" / "release_meta.py"


def load_module():
    spec = importlib.util.spec_from_file_location("release_meta", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class VersionValidationTest(unittest.TestCase):
    def setUp(self):
        self.meta = load_module()

    def test_accepts_strict_semver(self):
        for version in (
            "0.2.0",
            "1.0.0-rc.1",
            "1.0.0-alpha.1+build.20261127",
            "10.20.30",
        ):
            self.assertEqual(self.meta.validate_version(version), version)

    def test_rejects_shell_and_output_injection(self):
        for value in (
            "0.1.0\npublish=true",
            "0.1.0\rversion=9.9.9",
            "0.1.0; touch /tmp/pwned",
            "0.1.0$(id)",
            "v0.1.0",
            "01.2.3",
            "1.2",
            "1.2.3-01",
            "1.2.3;",
            "1.2.3\n",
            "１.２.３",
        ):
            with self.assertRaises(self.meta.VersionError, msg=value):
                self.meta.validate_version(value)

    def test_tag_is_the_fallback_and_must_have_the_v_prefix(self):
        self.assertEqual(self.meta.resolve_version("", "v0.2.0"), "0.2.0")
        with self.assertRaises(self.meta.VersionError):
            self.meta.resolve_version("", "release-0.2.0")

    def test_output_file_contains_only_validated_fields(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "github-output"
            self.meta.write_outputs(output, "0.2.0", True, "ghcr.io/acme/wxlike-server")
            self.assertEqual(
                output.read_text(encoding="utf-8").splitlines(),
                [
                    "version=0.2.0",
                    "publish=true",
                    "image=ghcr.io/acme/wxlike-server",
                ],
            )


class ReleaseWorkflowGateTest(unittest.TestCase):
    def setUp(self):
        self.release = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )

    def test_release_waits_for_main_ci_and_security(self):
        for job in ("ci-gate:", "security-gate:"):
            self.assertIn(job, self.release)
        self.assertIn("uses: ./.github/workflows/ci.yml", self.release)
        self.assertIn("uses: ./.github/workflows/security.yml", self.release)
        self.assertIn("needs.ci-gate.result == 'success'", self.release)
        self.assertIn("needs.security-gate.result == 'success'", self.release)

    def test_both_called_workflows_allow_workflow_call(self):
        for name in ("ci.yml", "security.yml"):
            text = (ROOT / ".github" / "workflows" / name).read_text(encoding="utf-8")
            self.assertIn("workflow_call", text, name)

    def test_raw_version_is_not_interpolated_into_run_blocks(self):
        # `run:` bodies may only see the validated step output via env.
        for line in self.release.splitlines():
            if line.strip().startswith("run:"):
                self.assertNotIn("inputs.version", line)
                self.assertNotIn("steps.meta.outputs.version", line)


if __name__ == "__main__":
    unittest.main()
