"""Verify the target side by streaming rows back and rebuilding the digest."""

from __future__ import annotations

from dataclasses import dataclass
from decimal import Decimal

from .codec import NULL_TOKEN, RowDigest
from .loader import ident
from .model import Kind, TablePlan


@dataclass(frozen=True)
class VerifyResult:
    source: str
    target: str
    source_rows: int
    target_rows: int
    expected_digest: str
    actual_digest: str
    reason: str = ""

    @property
    def ok(self) -> bool:
        return self.source_rows == self.target_rows and self.expected_digest == self.actual_digest


def copy_out_sql(plan: TablePlan) -> str:
    columns = ", ".join(ident(column.name) for column in plan.columns)
    return (
        f"COPY (SELECT {columns} FROM {ident(plan.target)}) TO STDOUT "
        "WITH (FORMAT csv, HEADER false, NULL '\\N')"
    )


def decode_target(raw: str, kind: Kind) -> object:
    if raw == NULL_TOKEN:
        return None
    if kind == "bytes":
        if not raw.startswith("\\x"):
            raise ValueError(f"bytea value is not hex: {raw[:16]!r}")
        return bytes.fromhex(raw[2:])
    if kind == "bool":
        lower = raw.lower()
        if lower in {"t", "true", "1"}:
            return True
        if lower in {"f", "false", "0"}:
            return False
        raise ValueError(f"boolean value is not recognized: {raw!r}")
    if kind == "int":
        # An INTEGER source may land in NUMERIC(20,2); the target prints 12.00.
        lower = raw.lower()
        if lower in {"t", "true", "f", "false"}:
            return 1 if lower in {"t", "true"} else 0
        value = Decimal(raw)
        if value != value.to_integral_value():
            raise ValueError(f"integer target returned a fractional value: {raw!r}")
        return int(value)
    if kind == "float":
        return float(raw)
    if kind == "decimal":
        return Decimal(raw)
    if kind == "text":
        return raw
    raise ValueError(f"unknown value family {kind!r}")


def verify_table(psql, plan: TablePlan, expected_rows: int, expected_digest: str) -> VerifyResult:
    kinds = [column.kind for column in plan.columns]
    digest = RowDigest()
    for raw_row in psql.iter_copy_rows(copy_out_sql(plan)):
        if len(raw_row) != len(kinds):
            raise ValueError(
                f"{plan.target}: target row has {len(raw_row)} fields, plan has {len(kinds)}"
            )
        values = [decode_target(raw, kind) for raw, kind in zip(raw_row, kinds)]
        digest.add(values, kinds)
    actual = digest.finish()
    reason = ""
    if digest.rows != expected_rows:
        reason = f"row count source={expected_rows} target={digest.rows}"
    elif actual != expected_digest:
        reason = "row digest differs"
    return VerifyResult(
        source=plan.source,
        target=plan.target,
        source_rows=expected_rows,
        target_rows=digest.rows,
        expected_digest=expected_digest,
        actual_digest=actual,
        reason=reason,
    )
