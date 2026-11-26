from __future__ import annotations

import sqlite3
import tempfile
import unittest
from pathlib import Path

from wximport.sqlite_source import (
    data_version,
    file_sha256,
    list_tables,
    open_readonly,
    source_table,
)


SCHEMA = """
CREATE TABLE users (
    id TEXT PRIMARY KEY,
    n INTEGER NOT NULL,
    note TEXT,
    payload BLOB,
    g INTEGER GENERATED ALWAYS AS (n + 1) VIRTUAL
);
CREATE TABLE child (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id)
);
CREATE TABLE auto (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    body TEXT NOT NULL
);
"""


class SqliteSourceTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "source.db"
        writer = sqlite3.connect(self.path)
        writer.executescript(SCHEMA)
        writer.execute("INSERT INTO users(id, n, note, payload) VALUES ('u1', 7, '', x'00ff')")
        writer.execute("INSERT INTO child(id, user_id) VALUES ('c1', 'u1')")
        writer.execute("INSERT INTO auto(body) VALUES ('x')")
        writer.commit()
        writer.close()
        self.conn = open_readonly(self.path)

    def tearDown(self) -> None:
        self.conn.close()
        self.temp.cleanup()

    def test_lists_physical_tables_but_not_sqlite_sequence(self) -> None:
        self.assertEqual(list_tables(self.conn), ["auto", "child", "users"])

    def test_generated_column_is_not_insertable(self) -> None:
        users = source_table(self.conn, "users")
        self.assertEqual([column.name for column in users.insertable_columns], ["id", "n", "note", "payload"])
        self.assertEqual(users.primary_key, ("id",))
        kinds = {column.name: column.kind for column in users.columns}
        self.assertEqual(kinds["n"], "int")
        self.assertEqual(kinds["payload"], "bytes")

    def test_foreign_key_resolves_to_parent_primary_key(self) -> None:
        child = source_table(self.conn, "child")
        self.assertEqual(child.primary_key, ("id",))
        self.assertEqual(
            [(fk.column, fk.ref_table, fk.ref_column) for fk in child.foreign_keys],
            [("user_id", "users", "id")],
        )

    def test_file_digest_is_stable(self) -> None:
        self.assertEqual(file_sha256(self.path), file_sha256(self.path))
        self.assertEqual(len(file_sha256(self.path)), 64)

    def test_data_version_moves_only_after_another_writer_commits(self) -> None:
        before = data_version(self.conn)
        writer = sqlite3.connect(self.path)
        writer.execute("INSERT INTO auto(body) VALUES ('y')")
        writer.commit()
        writer.close()
        self.assertGreater(data_version(self.conn), before)


if __name__ == "__main__":
    unittest.main()
