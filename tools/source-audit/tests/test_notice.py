"""NOTICE 覆盖用例：漏登记 / 合格豁免 / 过期豁免 / 僵尸豁免。"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from fixture import FakeRepo  # noqa: E402
from sourceaudit import notice  # noqa: E402
from sourceaudit.util import DataError  # noqa: E402


class NoticeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FakeRepo()

    def tearDown(self) -> None:
        self.repo.cleanup()

    def codes(self) -> list[str]:
        return [f.code for f in notice.check(self.repo.root).findings]

    def test_covered_dependency_passes(self) -> None:
        report = notice.check(self.repo.root)
        self.assertEqual(report.blocking, [], report.render_text())

    def test_unlisted_dependency_blocks(self) -> None:
        manifest = self.repo.root / "crates/demo/Cargo.toml"
        manifest.write_text(
            manifest.read_text(encoding="utf-8").replace('serde = "1"', 'serde = "1"\nanyhow = "1"'),
            encoding="utf-8",
        )
        report = notice.check(self.repo.root)
        self.assertIn("NOTICE-MISSING", [f.code for f in report.blocking])

    def test_target_specific_dependency_counts(self) -> None:
        """`[target.'cfg(unix)'.dependencies]` 也是直接依赖——漏解析会漏登记。"""
        manifest = self.repo.root / "crates/demo/Cargo.toml"
        manifest.write_text(
            manifest.read_text(encoding="utf-8")
            + "\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n",
            encoding="utf-8",
        )
        self.assertIn("NOTICE-MISSING", [f.code for f in notice.check(self.repo.root).blocking])

    def test_waiver_covers_gap(self) -> None:
        self.repo.write_json(
            "third_party/reviews/notice-waivers.json",
            {
                "schema": "wxlike.notice-waivers.v1",
                "waivers": [
                    {"crate": "serde", "owner": "G00", "issue": "#1", "expires": "2099-12-31"}
                ],
            },
        )
        (self.repo.root / "THIRD-PARTY-NOTICES.md").write_text("# nothing here\n", encoding="utf-8")
        report = notice.check(self.repo.root)
        self.assertEqual(report.blocking, [], report.render_text())
        self.assertIn("NOTICE-WAIVER", [f.code for f in report.findings])

    def test_expired_waiver_blocks(self) -> None:
        self.repo.write_json(
            "third_party/reviews/notice-waivers.json",
            {
                "schema": "wxlike.notice-waivers.v1",
                "waivers": [
                    {"crate": "serde", "owner": "G00", "issue": "#1", "expires": "2020-01-01"}
                ],
            },
        )
        (self.repo.root / "THIRD-PARTY-NOTICES.md").write_text("# nothing here\n", encoding="utf-8")
        codes = [f.code for f in notice.check(self.repo.root).blocking]
        self.assertIn("EXC-EXPIRED", codes)

    def test_waiver_without_owner_blocks(self) -> None:
        self.repo.write_json(
            "third_party/reviews/notice-waivers.json",
            {"schema": "wxlike.notice-waivers.v1", "waivers": [{"crate": "serde", "expires": "2099-01-01"}]},
        )
        (self.repo.root / "THIRD-PARTY-NOTICES.md").write_text("# nothing here\n", encoding="utf-8")
        self.assertIn("NOTICE-WAIVER", [f.code for f in notice.check(self.repo.root).blocking])

    def test_stale_waiver_warns(self) -> None:
        self.repo.write_json(
            "third_party/reviews/notice-waivers.json",
            {
                "schema": "wxlike.notice-waivers.v1",
                "waivers": [
                    {"crate": "removed-crate", "owner": "G00", "issue": "#1", "expires": "2099-01-01"}
                ],
            },
        )
        codes = [f.code for f in notice.check(self.repo.root).findings]
        self.assertIn("NOTICE-STALE-WAIVER", codes)

    def test_bad_waiver_schema_is_data_error(self) -> None:
        self.repo.write_json("third_party/reviews/notice-waivers.json", {"schema": "wrong", "waivers": []})
        with self.assertRaises(DataError):
            notice.check(self.repo.root)


if __name__ == "__main__":
    unittest.main()
