"""SBOM 用例：可复现、四类来源、漂移检测、重复生成的幂等。"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from fixture import FakeRepo  # noqa: E402
from sourceaudit import sbom  # noqa: E402
from sourceaudit.util import DataError  # noqa: E402


class SbomTest(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FakeRepo()

    def tearDown(self) -> None:
        self.repo.cleanup()

    def test_components_cover_all_four_lock_sources(self) -> None:
        self._add_npm()
        self._add_dart()
        self._add_docker()
        sources = {c.source.split(":")[0] for c in sbom.collect(self.repo.root)}
        self.assertEqual(sources, {"cargo-lock", "npm-lock", "pubspec-lock", "dockerfile", "docker-compose"})

    def test_desktop_lock_versions_are_not_hidden_by_root(self) -> None:
        lock = self.repo.root / "ui/src-tauri/Cargo.lock"
        lock.parent.mkdir(parents=True, exist_ok=True)
        lock.write_text('[[package]]\nname = "desktop-only"\nversion = "2.0.0"\n', encoding="utf-8")
        comps = sbom.collect(self.repo.root)
        self.assertTrue(any(c.name == "desktop-only" and c.source.endswith("ui/src-tauri/Cargo.lock") for c in comps))
        sbom.write(self.repo.root, self.repo.root / "third_party/reviews/sbom/wxlike.cdx.json")
        lock.write_text(lock.read_text().replace("2.0.0", "2.0.1"), encoding="utf-8")
        self.assertTrue(sbom.check(self.repo.root).blocking, "桌面独立锁变化必须造成 SBOM 漂移")

    def test_current_frontend_lock_locations_are_scanned(self) -> None:
        paths = ("apps/desktop-web/package-lock.json", "design/ui-v2/prototype/package-lock.json")
        for index, path in enumerate(paths):
            self.repo.write_json(path, {"packages": {"node_modules/frontend-only": {"version": f"{index}.0.0"}}})
        found = {c.source for c in sbom.collect(self.repo.root) if c.name == "frontend-only"}
        self.assertEqual(found, {f"npm-lock:{path}" for path in paths})

    def test_generation_is_reproducible(self) -> None:
        first = sbom.build(self.repo.root)
        second = sbom.build(self.repo.root)
        self.assertEqual(
            json.dumps(first, sort_keys=True),
            json.dumps(second, sort_keys=True),
            "两次生成不一致：serialNumber 或组件顺序里混进了非确定内容",
        )
        self.assertTrue(first["serialNumber"].startswith("urn:uuid:"))

    def test_second_write_is_noop(self) -> None:
        out = self.repo.root / "third_party/reviews/sbom/wxlike.cdx.json"
        changed_first, count = sbom.write(self.repo.root, out)
        changed_second, _ = sbom.write(self.repo.root, out)
        self.assertTrue(changed_first)
        self.assertFalse(changed_second, "同一锁文件重复生成应当零改动")
        self.assertGreater(count, 0)

    def test_drift_is_detected(self) -> None:
        out = self.repo.root / "third_party/reviews/sbom/wxlike.cdx.json"
        sbom.write(self.repo.root, out)
        lock = self.repo.root / "Cargo.lock"
        lock.write_text(
            lock.read_text(encoding="utf-8")
            + '\n[[package]]\nname = "hijacked"\nversion = "6.6.6"\n',
            encoding="utf-8",
        )
        report = sbom.check(self.repo.root)
        self.assertIn("SBOM-DRIFT-ADD", [f.code for f in report.blocking])

    def test_missing_sbom_is_a_blocker(self) -> None:
        report = sbom.check(self.repo.root)
        self.assertIn("SBOM-MISSING", [f.code for f in report.blocking])

    def test_no_lockfiles_is_data_error(self) -> None:
        self.repo.rm("Cargo.lock")
        with self.assertRaises(DataError):
            sbom.build(self.repo.root)

    def test_license_is_never_guessed(self) -> None:
        bom = sbom.build(self.repo.root)
        for comp in bom["components"]:
            self.assertEqual(comp["licenses"], [{"license": {"name": "NOASSERTION"}}])

    # ---- 夹具 -------------------------------------------------------------
    def _add_npm(self) -> None:
        self.repo.write_json(
            "ui/package-lock.json",
            {
                "lockfileVersion": 3,
                "packages": {
                    "": {"name": "ui"},
                    "node_modules/react": {"version": "18.3.1"},
                    "node_modules/@vitejs/plugin-rsc": {"version": "0.4.0"},
                },
            },
        )

    def _add_dart(self) -> None:
        path = self.repo.root / "apps/mobile/pubspec.lock"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            "packages:\n"
            "  args:\n"
            "    dependency: transitive\n"
            '    version: "2.5.0"\n'
            "  root:\n"
            "    dependency: root\n"
            '    version: "1.0.0"\n',
            encoding="utf-8",
        )

    def _add_docker(self) -> None:
        (self.repo.root / "Dockerfile").write_text(
            "FROM --platform=linux/amd64 rust:1-bookworm AS build\n"
            "FROM debian:bookworm-slim\n",
            encoding="utf-8",
        )
        (self.repo.root / "docker-compose.yml").write_text(
            "services:\n  cache:\n    image: redis:7-alpine\n"
            "  turn:\n    image: coturn/coturn@sha256:" + "f" * 64 + "\n",
            encoding="utf-8",
        )


class ImageRefTest(unittest.TestCase):
    def test_digest_pin_is_preserved(self) -> None:
        comp = sbom._image_component("coturn/coturn@sha256:" + "a" * 64, "docker-compose")
        assert comp is not None
        self.assertIn("digest=sha256:" + "a" * 64, comp.purl)

    def test_scratch_is_skipped(self) -> None:
        self.assertIsNone(sbom._image_component("scratch", "dockerfile"))


if __name__ == "__main__":
    unittest.main()
