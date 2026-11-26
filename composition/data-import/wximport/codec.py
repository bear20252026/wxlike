"""Byte-stable CSV encoding and multiset digests for row imports.

``COPY ... WITH (FORMAT csv, NULL '\\N')`` distinguishes NULL (unquoted
``\\N``) from an empty string (quoted ``""``). Python's ``csv.writer`` collapses
both to an empty field, so this module writes the fields directly.
"""

from __future__ import annotations

import hashlib
import json
import math
from decimal import Decimal

from .model import Kind

NULL_TOKEN = r"\N"


def _decimal(value: object) -> Decimal:
    """Decimal from SQLite, avoiding the binary float noise in ``Decimal(1.23)``."""

    if isinstance(value, Decimal):
        return value
    return Decimal(str(value))


def _quote(text: str) -> str:
    return '"' + text.replace('"', '""') + '"'


def encode_field(value: object, kind: Kind) -> str:
    if value is None:
        return NULL_TOKEN
    if kind == "bytes":
        if not isinstance(value, (bytes, bytearray)):
            raise ValueError(f"bytes column got {type(value).__name__}")
        return _quote("\\x" + bytes(value).hex())
    if kind == "bool":
        return _quote("t" if bool(value) else "f")
    if kind == "int":
        return _quote(str(int(value)))
    if kind == "float":
        number = float(value)
        if math.isnan(number):
            return _quote("NaN")
        if math.isinf(number):
            return _quote("Infinity" if number > 0 else "-Infinity")
        return _quote(repr(number))
    if kind == "decimal":
        return _quote(format(_decimal(value).normalize(), "f"))
    if kind == "text":
        return _quote(str(value))
    raise ValueError(f"unknown value family {kind!r}")


def encode_row(values: list[object], kinds: list[Kind]) -> str:
    if len(values) != len(kinds):
        raise ValueError("row length does not match column count")
    return ",".join(encode_field(value, kind) for value, kind in zip(values, kinds)) + "\n"


def coerce_source(value: object, kind: Kind) -> object:
    """Normalize a SQLite value to the family the target expects."""

    if value is None:
        return None
    if kind == "bytes":
        return bytes(value) if isinstance(value, (bytes, bytearray)) else str(value).encode()
    if kind == "bool":
        return bool(int(value))
    if kind == "int":
        return int(value)
    if kind == "float":
        return float(value)
    if kind == "decimal":
        return _decimal(value)
    if kind == "text":
        return str(value)
    raise ValueError(f"unknown value family {kind!r}")


def canonical_value(value: object, kind: Kind) -> object:
    if value is None:
        return None
    if kind == "bytes":
        return "b:" + bytes(value).hex()
    if kind == "bool":
        return "o:" + ("1" if bool(value) else "0")
    if kind == "int":
        return "i:" + str(int(value))
    if kind == "float":
        return "f:" + repr(float(value))
    if kind == "decimal":
        return "d:" + format(_decimal(value).normalize(), "f")
    if kind == "text":
        return "t:" + str(value)
    raise ValueError(f"unknown value family {kind!r}")


def row_hash(values: list[object], kinds: list[Kind]) -> str:
    payload = [canonical_value(value, kind) for value, kind in zip(values, kinds)]
    encoded = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


class RowDigest:
    """Order-independent digest over every row in a table.

    Sorting per-row hashes means the source and the target may stream in any
    order, including a table with no primary key, and still agree on the
    multiset of rows.
    """

    def __init__(self) -> None:
        self.rows = 0
        self._hashes: list[str] = []

    def add(self, values: list[object], kinds: list[Kind]) -> None:
        self.rows += 1
        self._hashes.append(row_hash(values, kinds))

    def finish(self) -> str:
        if not self._hashes:
            return hashlib.sha256(b"").hexdigest()
        joined = "\n".join(sorted(self._hashes)).encode("utf-8")
        return hashlib.sha256(joined).hexdigest()
