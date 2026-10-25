"""OSV 接线回归：真实请求契约，网络只在此做故障/分页注入。"""

from __future__ import annotations

import io
import json
import sys
import unittest
import urllib.error
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from sourceaudit import advisories
from sourceaudit.model import Report, Severity
from sourceaudit.util import DataError


def response(data):
    return io.BytesIO(json.dumps(data).encode())


class OsvTransportTest(unittest.TestCase):
    def test_ecosystems_are_preserved_and_images_are_excluded(self):
        requests = []

        def send(req, **_):
            queries = json.loads(req.data)["queries"]
            requests.extend(queries)
            return response({"results": [{} for _ in queries]})

        inventory = {"same-name": {"1.0": {"rust-root", "web", "mobile"}}, "debian": {"bookworm": {"infra"}}}
        with patch.object(advisories.urllib.request, "urlopen", side_effect=send):
            scanned = advisories._fetch_osv(inventory, 1)
        self.assertEqual({q["package"]["ecosystem"] for q in requests}, {"crates.io", "npm", "Pub"})
        self.assertEqual(len(scanned["index"]), 3)
        self.assertTrue(all(q["package"]["name"] == "same-name" for q in requests))

    def test_only_pending_queries_are_paginated(self):
        requests = []

        def send(req, **_):
            queries = json.loads(req.data)["queries"]
            requests.append(queries)
            if len(requests) == 1:
                return response({"results": [{"vulns": [{"id": "A"}], "next_page_token": "p2"}, {}]})
            return response({"results": [{"vulns": [{"id": "B"}]}]})

        with patch.object(advisories.urllib.request, "urlopen", side_effect=send):
            scanned = advisories._fetch_osv({"a": {"1": {"web"}}, "b": {"1": {"web"}}}, 1)
        self.assertEqual(len(requests[1]), 1)
        self.assertEqual(requests[1][0]["page_token"], "p2")
        self.assertEqual([v["id"] for v in scanned["osv"]["results"][0]["vulns"]], ["A", "B"])
        self.assertEqual(scanned["osv"]["results"][1]["vulns"], [])

    def test_missing_response_row_is_not_a_clean_scan(self):
        with patch.object(advisories.urllib.request, "urlopen", return_value=response({"results": []})):
            with self.assertRaises(DataError):
                advisories._fetch_osv({"a": {"1": {"web"}}}, 1)

    def test_repeating_page_token_fails_instead_of_losing_results(self):
        def send(*_, **__):
            return response({"results": [{"next_page_token": "loop"}]})

        with patch.object(advisories.urllib.request, "urlopen", side_effect=send):
            with self.assertRaises(DataError):
                advisories._fetch_osv({"a": {"1": {"web"}}}, 1)

    def test_detail_network_failure_is_not_an_unverified_success(self):
        with patch.object(advisories.urllib.request, "urlopen", side_effect=urllib.error.URLError("timeout")):
            with self.assertRaises(DataError):
                advisories._osv_detail("GHSA-test", 1)

    def test_wrong_detail_id_is_rejected(self):
        with patch.object(advisories.urllib.request, "urlopen", return_value=response({"id": "other"})):
            with self.assertRaises(DataError):
                advisories._osv_detail("GHSA-test", 1)


class OsvClassificationTest(unittest.TestCase):
    def test_same_advisory_preserves_every_version_and_ecosystem(self):
        scanned = {"index": [("x", "1", "crates.io"), ("x", "2", "crates.io"), ("x", "2", "npm")],
                   "osv": {"results": [{"vulns": [{"id": "GHSA-test"}]} for _ in range(3)]}}
        detail = {"id": "GHSA-test", "database_specific": {"severity": "HIGH"}}
        with patch.object(advisories, "_osv_detail", return_value=detail) as fetch:
            records = advisories._from_osv(scanned, [])
        self.assertEqual(len(records), 2)
        cargo = next(r for r in records if r["ecosystem"] == "crates.io")
        self.assertEqual(cargo["affected"], ["==1", "==2"])
        self.assertEqual(fetch.call_count, 1)

    def test_cargo_version_cannot_be_confused_with_same_named_npm_package(self):
        row = {"id": "TEST", "component": "x", "ecosystem": "npm", "severity": "high", "affected": ["<2"]}
        hits = advisories._judge(row, {"x": {"1": {"rust-root"}, "3": {"web"}}}, {})
        self.assertEqual([(h.version, h.verdict) for h in hits], [("3", advisories.FIXED)])

    def test_unsound_is_not_downgraded_to_unmaintained(self):
        scanned = {"index": [("x", "1", "crates.io")], "osv": {"results": [{"vulns": [{"id": "TEST"}]}]}}
        detail = {"id": "TEST", "database_specific": {"severity": "HIGH", "informational": "unsound"}}
        with patch.object(advisories, "_osv_detail", return_value=detail):
            record = advisories._from_osv(scanned, [])[0]
        self.assertEqual((record["kind"], record["severity"]), ("vulnerability", "high"))

    def test_unknown_cvss_requires_review_when_policy_blocks_unverified(self):
        level = advisories._osv_severity({"severity": [{"score": "CVSS:3.1/AV:N/C:H/I:H/A:H"}]})
        self.assertIsNone(level, "未使用可信评分器，不能从向量猜严重等级")
        report = Report("test")
        hit = advisories.Hit("TEST", "x", "1", Severity.HIGH, advisories.UNVERIFIED, "needs review")
        advisories._report_hits(report, [hit], {"gate": {"unverified": "block"}})
        self.assertTrue(report.blocking)


if __name__ == "__main__":
    unittest.main()
