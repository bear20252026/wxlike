from __future__ import annotations

import csv
import io
import sqlite3
import tempfile
import unittest
from pathlib import Path

from wximport.codec import RowDigest, coerce_source, encode_field, encode_row, row_hash
from wximport.exporter import export_table
from wximport.plan import build_plan
from wximport.sqlite_source import open_readonly


class CodecTest(unittest.TestCase):
    def test_null_empty_and_bytes_are_distinct(self) -> None:
        row = encode_row(
            [None, "", b"\x00\xff"],
            ["text", "text", "bytes"],
        )
        self.assertEqual(row, '\\N,"","\\x00ff"\n')

    def test_quotes_and_newlines_round_trip(self) -> None:
        row = encode_row(['a"b\nc'], ["text"])
        parsed = list(csv.reader(io.StringIO(row)))
        self.assertEqual(parsed, [['a"b\nc']])

    def test_float_and_boolean_are_canonical(self) -> None:
        self.assertEqual(encode_field(1.5, "float"), '"1.5"')
        self.assertEqual(encode_field(True, "bool"), '"t"')
        self.assertEqual(encode_field(False, "bool"), '"f"')
        self.assertEqual(encode_field(7, "int"), '"7"')

    def test_decimal_from_sqlite_float_has_no_binary_noise(self) -> None:
        source = coerce_source(1.23, "decimal")
        self.assertEqual(format(source, "f"), "1.23")
        self.assertEqual(encode_field(1.23, "decimal"), '"1.23"')
        self.assertEqual(row_hash([source], ["decimal"]), row_hash([1.23], ["decimal"]))

    def test_digest_is_order_independent(self) -> None:
        left = RowDigest()
        right = RowDigest()
        rows = [(["a"], ["text"]), (["b"], ["text"]), (["c"], ["text"])]
        for values, kinds in rows:
            left.add(values, kinds)
        for values, kinds in reversed(rows):
            right.add(values, kinds)
        self.assertEqual(left.finish(), right.finish())
        self.assertEqual(left.rows, 3)

    def test_digest_changes_with_a_value(self) -> None:
        self.assertNotEqual(
            row_hash(["a"], ["text"]),
            row_hash(["A"], ["text"]),
        )


class ExporterTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "source.db"
        writer = sqlite3.connect(self.path)
        writer.executescript(
            """
            CREATE TABLE items (
                id TEXT PRIMARY KEY,
                note TEXT,
                payload BLOB,
                n INTEGER NOT NULL
            );
            """
        )
        writer.execute("INSERT INTO items VALUES ('a', NULL, x'00ff', 1)")
        writer.execute("INSERT INTO items VALUES ('b', '', x'', 2)")
        writer.commit()
        writer.close()
        self.conn = open_readonly(self.path)

    def tearDown(self) -> None:
        self.conn.close()
        self.temp.cleanup()

    def test_export_keeps_null_and_empty_apart(self) -> None:
        plan = build_plan(self.path)
        csv_path = Path(self.temp.name) / "items.csv"
        result = export_table(self.conn, plan.tables[0], csv_path)
        self.assertEqual(result.rows, 2)
        lines = csv_path.read_text(encoding="utf-8").splitlines()
        self.assertEqual(lines[0], '"a",\\N,"\\x00ff","1"')
        self.assertEqual(lines[1], '"b","","\\x","2"')

    def test_export_digest_is_repeatable(self) -> None:
        plan = build_plan(self.path)
        first = export_table(self.conn, plan.tables[0], Path(self.temp.name) / "one.csv")
        second = export_table(self.conn, plan.tables[0], Path(self.temp.name) / "two.csv")
        self.assertEqual(first.digest, second.digest)
        self.assertEqual(first.rows, second.rows)


if __name__ == "__main__":
    unittest.main()
