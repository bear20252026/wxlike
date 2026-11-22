#!/usr/bin/env python3
r"""M013 迁移导入器：SQLite `wallets` / `ledger` -> PostgreSQL staging。

只做两件事：把旧库的行按 staging 列导出为 CSV，再调用 `migrate_wallets_batch`。
不在 Python 里做账务记账；记账口径全部在 M013 的 SQL 过程里，避免第二套实现。

用法（PowerShell / bash 均可）：

    python schema/proposals/G06/import_ledger_wallets.py `
      --sqlite D:\data\wxlike.db `
      --dsn "postgresql://postgres:postgres@127.0.0.1:5432/wxlike" `
      --batch-size 1000

默认严格模式：`wallets.balance` 与流水和不一致时整批中止。确认存量有历史漂移、
要走补差路径时再加 `--allow-adjustment`；脚本会在最后打印 `adjusted` 数量。

`--reset` 只清空两张 staging 表（不动 ledger_journals/entries）；因为 journal
的 business_key 幂等，重跑不会重复入账。
"""

from __future__ import annotations

import argparse
import csv
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

ALLOWED_KINDS = {
    "fund_in",
    "transfer_in",
    "transfer_out",
    "redpacket_out",
    "redpacket_in",
    "redpacket_refund",
}

NEGATIVE_KINDS = {"transfer_out", "redpacket_out"}


def fail(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def run_psql(psql: str, dsn: str, sql: str, stdin_path: Path | None = None) -> None:
    cmd = [psql, "-X", "-v", "ON_ERROR_STOP=1", "-d", dsn, "-c", sql]
    with (stdin_path.open("rb") if stdin_path else open(os.devnull, "rb")) as stdin:
        result = subprocess.run(cmd, stdin=stdin, text=False)
    if result.returncode != 0:
        fail(f"psql failed ({result.returncode}): {sql}")


def read_sqlite(sqlite_path: Path) -> tuple[list[tuple[str, int, int]], list[tuple]]:
    if not sqlite_path.is_file():
        fail(f"SQLite 文件不存在: {sqlite_path}")

    conn = sqlite3.connect(f"file:{sqlite_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    try:
        wallets = []
        for row in conn.execute("SELECT user_id, balance FROM wallets ORDER BY user_id"):
            user_id = str(row["user_id"])
            balance = int(row["balance"])
            if balance < 0:
                fail(f"wallets.balance < 0: user={user_id} balance={balance}")
            wallets.append((user_id, balance, 0))

        ledger = []
        seen_ids = set()
        for row in conn.execute(
            "SELECT id, user_id, kind, amount, counterparty, ref_id, memo, created_at "
            "FROM ledger ORDER BY id"
        ):
            ledger_id = int(row["id"])
            if ledger_id in seen_ids:
                fail(f"ledger.id 重复: {ledger_id}")
            seen_ids.add(ledger_id)
            kind = str(row["kind"])
            if kind not in ALLOWED_KINDS:
                fail(f"ledger.id={ledger_id} 未知 kind={kind!r}，先补映射再迁移")
            amount = int(row["amount"])
            if amount <= 0:
                fail(f"ledger.id={ledger_id} amount<=0: {amount}")
            ledger.append(
                (
                    ledger_id,
                    str(row["user_id"]),
                    kind,
                    amount,
                    str(row["counterparty"] or ""),
                    str(row["ref_id"] or ""),
                    str(row["memo"] or ""),
                    int(row["created_at"]),
                )
            )

        # 用 Python 算 staging 的 ledger_sum_minor，只作为导入报告/预检；
        # 严格判定仍以 SQL 过程内的 v_sum 为准。
        sums: dict[str, int] = {}
        for _, user_id, kind, amount, *_ in ledger:
            signed = -amount if kind in NEGATIVE_KINDS else amount
            sums[user_id] = sums.get(user_id, 0) + signed
        wallets = [
            (user_id, balance, sums.get(user_id, 0))
            for user_id, balance, _ in wallets
        ]
        return wallets, ledger
    finally:
        conn.close()


def write_csv(path: Path, header: list[str], rows: list[tuple]) -> None:
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(header)
        writer.writerows(rows)


def main() -> None:
    parser = argparse.ArgumentParser(description="M013 SQLite -> PG staging importer")
    parser.add_argument("--sqlite", required=True, type=Path, help="SQLite 数据库路径")
    parser.add_argument(
        "--dsn",
        default=os.environ.get("WXLIKE_PG_URL", ""),
        help="PostgreSQL DSN；缺省读 WXLIKE_PG_URL",
    )
    parser.add_argument("--batch-size", type=int, default=1000)
    parser.add_argument(
        "--allow-adjustment",
        action="store_true",
        help="允许 wallets.balance 与流水和不一致时补期初差额",
    )
    parser.add_argument(
        "--reset",
        action="store_true",
        help="先 TRUNCATE 两张 staging 表（不动已入账 journal）",
    )
    parser.add_argument("--dry-run", action="store_true", help="只导出并打印计数，不连 PG")
    args = parser.parse_args()

    if args.batch_size < 1 or args.batch_size > 10000:
        fail("--batch-size 必须在 1..10000")
    if not args.dry_run and not args.dsn:
        fail("缺少 --dsn 或 WXLIKE_PG_URL")

    psql = shutil.which("psql")
    if not args.dry_run and not psql:
        fail("找不到 psql；把 PostgreSQL bin 目录放进 PATH")

    wallets, ledger = read_sqlite(args.sqlite)
    print(f"sqlite: wallets={len(wallets)} ledger={len(ledger)}")
    if not ledger:
        print("warning: ledger 为空，只会迁移钱包开户与期初差额（如允许调整）")

    with tempfile.TemporaryDirectory(prefix="wxlike-m013-") as temp_dir:
        temp = Path(temp_dir)
        wallets_csv = temp / "migration_wallets.csv"
        ledger_csv = temp / "migration_ledger.csv"
        write_csv(
            wallets_csv,
            ["user_id", "balance_minor", "ledger_sum_minor"],
            wallets,
        )
        write_csv(
            ledger_csv,
            [
                "id",
                "user_id",
                "kind",
                "amount_minor",
                "counterparty",
                "ref_id",
                "memo",
                "created_at",
            ],
            ledger,
        )

        if args.dry_run:
            print(f"dry-run: {wallets_csv} / {ledger_csv} 已生成，未连 PG")
            return

        if args.reset:
            run_psql(psql, args.dsn, "TRUNCATE migration_ledger, migration_wallets")

        run_psql(
            psql,
            args.dsn,
            "COPY migration_wallets (user_id, balance_minor, ledger_sum_minor) "
            "FROM STDIN WITH (FORMAT csv, HEADER true)",
            wallets_csv,
        )
        run_psql(
            psql,
            args.dsn,
            "COPY migration_ledger (id, user_id, kind, amount_minor, counterparty, ref_id, memo, created_at) "
            "FROM STDIN WITH (FORMAT csv, HEADER true)",
            ledger_csv,
        )

        allow = "true" if args.allow_adjustment else "false"
        print(f"CALL migrate_wallets_batch({args.batch_size}, {allow})")
        run_psql(
            psql,
            args.dsn,
            f"CALL migrate_wallets_batch({args.batch_size}, {allow})",
        )


if __name__ == "__main__":
    main()
