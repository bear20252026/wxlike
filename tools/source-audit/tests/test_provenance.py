"""来源登记门禁的用例：成功 / 空结果 / 参数错误 / 未授权 / 数据缺失。

每条用例都对应一个真实会犯的错——不是"覆盖率"，是"上次谁在哪踩过"。
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from fixture import FakeRepo  # noqa: E402
from sourceaudit import provenance  # noqa: E402
from sourceaudit.util import DataError, UsageError, repo_root, resolve_inside  # noqa: E402


class ProvenanceTest(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = FakeRepo()

    def tearDown(self) -> None:
        self.repo.cleanup()

    def codes(self) -> list[str]:
        return [f.code for f in provenance.check(self.repo.root).findings]

    def test_clean_registry_passes(self) -> None:
        report = provenance.check(self.repo.root)
        self.assertEqual(report.blocking, [], report.render_text())
        self.assertIn("PROV-SHA", self.codes())

    def test_tampered_file_is_caught(self) -> None:
        self.repo.vendored.write_text("// 有人悄悄改了 vendored 文件\n", encoding="utf-8")
        codes = self.codes()
        self.assertIn("PROV-SHA-MISMATCH", codes)

    def test_missing_file_is_caught(self) -> None:
        self.repo.rm("ui/src/vendor/x.ts")
        self.assertIn("PROV-MISSING", self.codes())

    def test_path_traversal_is_refused(self) -> None:
        """未授权面：登记表是数据，不能变成读仓库外任意文件的手段。"""
        reg = self.repo.load_json("third_party/reviews/registry.json")
        reg["files"][0]["path"] = "../../etc/passwd"
        self.repo.write_json("third_party/reviews/registry.json", reg)
        self.assertIn("PROV-PATH", self.codes())
        with self.assertRaises(UsageError):
            resolve_inside(self.repo.root, "../../etc/passwd")

    def test_unknown_license_blocks_vendoring(self) -> None:
        reg = self.repo.load_json("third_party/reviews/registry.json")
        reg["files"][0]["license_spdx"] = "NOASSERTION"
        self.repo.write_json("third_party/reviews/registry.json", reg)
        report = provenance.check(self.repo.root)
        self.assertIn("PROV-LICENSE", [f.code for f in report.blocking])

    def test_short_commit_is_rejected(self) -> None:
        reg = self.repo.load_json("third_party/reviews/registry.json")
        reg["upstreams"][0]["commit"] = "abc1234"
        self.repo.write_json("third_party/reviews/registry.json", reg)
        self.assertIn("PROV-UP-SHA", self.codes())

    def test_missing_patchlog_is_rejected(self) -> None:
        self.repo.rm("third_party/reviews/patches/UP-DEMO-x.md")
        self.assertIn("PROV-PATCHLOG", self.codes())

    def test_notice_must_name_the_file(self) -> None:
        (self.repo.root / "THIRD-PARTY-NOTICES.md").write_text("# empty\n", encoding="utf-8")
        self.assertIn("PROV-NOTICE", self.codes())

    def test_excluded_entry_needs_known_rule(self) -> None:
        reg = self.repo.load_json("third_party/reviews/registry.json")
        reg["files"].append(
            {
                "path": "upstream:/ee",
                "upstream": "UP-DEMO",
                "upstream_path": "ee/",
                "decision": "excluded",
                "rule": "EXCL-MADE-UP",
                "reason": "看着不像开源",
            }
        )
        self.repo.write_json("third_party/reviews/registry.json", reg)
        self.assertIn("PROV-EXCL-RULE", self.codes())

    def test_empty_registry_is_not_silently_fine(self) -> None:
        """空结果不是通过：空表无法区分"没引入"和"忘了登记"。"""
        reg = self.repo.load_json("third_party/reviews/registry.json")
        reg["files"] = []
        self.repo.write_json("third_party/reviews/registry.json", reg)
        self.assertIn("PROV-EMPTY", self.codes())

    def test_broken_json_is_data_error_not_pass(self) -> None:
        (self.repo.root / "third_party/reviews/registry.json").write_text("{ not json", encoding="utf-8")
        with self.assertRaises(DataError):
            provenance.check(self.repo.root)

    def test_missing_registry_is_data_error(self) -> None:
        self.repo.rm("third_party/reviews/registry.json")
        with self.assertRaises(DataError):
            provenance.check(self.repo.root)

    def test_permalink_is_full_sha_not_branch(self) -> None:
        link = provenance.permalink({"url": "https://github.com/a/b", "commit": "c" * 40}, "src/x.ts")
        self.assertEqual(link, "https://github.com/a/b/blob/" + "c" * 40 + "/src/x.ts")


class RepoRootTest(unittest.TestCase):
    def test_repo_root_is_found_from_deep_path(self) -> None:
        root = repo_root(Path(__file__).resolve())
        self.assertTrue((root / "Cargo.toml").is_file())
        self.assertTrue((root / "crates").is_dir())

    def test_repo_root_refuses_random_dir(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(DataError):
                repo_root(Path(directory))


if __name__ == "__main__":
    unittest.main()
