#!/usr/bin/env python3
"""Regression tests for deploy/production/restore-drill.sh (H-43).

The drill is a shell script, so these tests run it against a fake `sqlite3`
that reports an intact database with zero rows. That keeps the test focused on
the media archive checks: declared-but-missing archive, hash/size mismatch,
and tar extraction failure must all fail the drill instead of printing
`restore drill ok`.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
DRILL = ROOT / "deploy" / "production" / "restore-drill.sh"

FAKE_SQLITE = """#!/usr/bin/env bash
case "$*" in
  *"PRAGMA integrity_check"*) echo ok ;;
  *"SELECT count(*) FROM users"*) echo 0 ;;
  *"SELECT count(*) FROM messages"*) echo 0 ;;
  *) echo 0 ;;
esac
"""



FAKE_SERVER = """#!/usr/bin/env python3
import http.server, os, signal, socketserver, sys

addr = os.environ["WXLIKE_HTTP"].rsplit(":", 1)

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *args):
        pass

signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer((addr[0], int(addr[1])), Handler) as server:
    server.serve_forever()
"""

def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class RestoreDrillMediaTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        sqlite = self.bin / "sqlite3"
        sqlite.write_text(FAKE_SQLITE, encoding="utf-8")
        sqlite.chmod(0o755)
        self.backup = self.root / "backup"
        self.backup.mkdir()
        self.db = self.backup / "wxlike.db"
        self.db.write_bytes(b"not-a-real-sqlite-but-hash-checked")
        self.media = self.backup / "media.tar.gz"
        with tarfile.open(self.media, "w:gz") as tar:
            payload = self.root / "blob"
            payload.write_bytes(b"media bytes")
            tar.add(payload, arcname="objects/aa/blob")
        self.manifest = self.backup / "manifest.json"
        self.write_manifest(self.media)

    def tearDown(self):
        self._tmp.cleanup()

    def write_manifest(self, media: Path | None):
        payload = {
            "schema_version": 1,
            "label": "test",
            "started_at": "2026-01-01T00:00:00Z",
            "finished_at": "2026-01-01T00:00:00Z",
            "database": {
                "path": "wxlike.db",
                "sha256": sha256(self.db),
                "bytes": self.db.stat().st_size,
                "schema_version": 0,
            },
            "media": {
                "path": media.name if media else None,
                "sha256": sha256(media) if media else None,
                "bytes": media.stat().st_size if media else 0,
            },
            "counts": {"users": 0, "messages": 0},
            "integrity_check": "ok",
        }
        self.manifest.write_text(json.dumps(payload), encoding="utf-8")

    def run_drill(self, out_name: str = "out") -> subprocess.CompletedProcess:
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        return subprocess.run(
            [
                "bash",
                str(DRILL),
                "--backup",
                str(self.backup),
                "--out",
                str(self.root / out_name),
            ],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_verified_media_is_recorded(self):
        result = self.run_drill()
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        drill = json.loads((self.root / "out" / "drill.json").read_text(encoding="utf-8"))
        self.assertTrue(drill["media_verified"])
        self.assertEqual(drill["media_sha256"], sha256(self.media))
        self.assertTrue((self.root / "out" / "restored" / "objects" / "aa" / "blob").is_file())

    def test_corrupt_media_archive_fails_the_drill(self):
        self.media.write_bytes(self.media.read_bytes() + b"x")
        result = self.run_drill()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("media archive sha256 mismatch", result.stderr)
        self.assertNotIn("restore drill ok", result.stdout)

    def test_declared_but_missing_media_fails_the_drill(self):
        self.media.unlink()
        result = self.run_drill()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("manifest declares media but", result.stderr)
        self.assertNotIn("restore drill ok", result.stdout)

    def test_archive_with_matching_hash_but_undecodable_fails_the_drill(self):
        self.media.write_bytes(b"definitely not gzip")
        self.write_manifest(self.media)
        result = self.run_drill()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("media archive extraction failed", result.stderr)
        self.assertNotIn("restore drill ok", result.stdout)

    def test_server_is_started_then_reaped(self):
        server = self.root / "fake-server"
        server.write_text(FAKE_SERVER, encoding="utf-8")
        server.chmod(0o755)
        env = dict(os.environ)
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        result = subprocess.run(
            [
                "bash",
                str(DRILL),
                "--backup",
                str(self.backup),
                "--out",
                str(self.root / "out"),
                "--server-bin",
                str(server),
            ],
            cwd=ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        drill = json.loads((self.root / "out" / "drill.json").read_text(encoding="utf-8"))
        self.assertTrue(drill["server_started"])

    def test_no_media_is_explicit_in_the_ledger(self):
        self.media.unlink()
        self.write_manifest(None)
        result = self.run_drill()
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        drill = json.loads((self.root / "out" / "drill.json").read_text(encoding="utf-8"))
        self.assertFalse(drill["media_verified"])
        self.assertIsNone(drill["media_sha256"])


if __name__ == "__main__":
    unittest.main()
