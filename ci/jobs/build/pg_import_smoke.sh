#!/usr/bin/env bash
# G00-04 S2 / M013 导入器全链路 smoke：
# 合成 SQLite -> import_ledger_wallets.py -> PostgreSQL staging -> migrate_wallets_batch -> 聚合断言。
#
# 这是 CI 上的“COPY + CALL”实跑；本机不需要安装 PostgreSQL 客户端。
# 前提：DATABASE_URL 指向已执行 M010-M013 的库，且 PATH 里有 psql。

set -euo pipefail

: "${DATABASE_URL:?DATABASE_URL is required}"
SQLITE_FILE="$(mktemp /tmp/wxlike-m013-import-XXXXXX.db)"
trap 'rm -f "$SQLITE_FILE"' EXIT

python3 - "$SQLITE_FILE" <<'PY'
import sqlite3
import sys

path = sys.argv[1]
conn = sqlite3.connect(path)
conn.executescript(
    """
    CREATE TABLE wallets (
        user_id TEXT PRIMARY KEY,
        balance INTEGER NOT NULL
    );
    CREATE TABLE ledger (
        id INTEGER PRIMARY KEY,
        user_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        amount INTEGER NOT NULL,
        counterparty TEXT,
        ref_id TEXT,
        memo TEXT,
        created_at INTEGER NOT NULL
    );
    """
)
conn.execute("INSERT INTO wallets(user_id, balance) VALUES ('ci-import-u1', 100)")
conn.execute(
    "INSERT INTO ledger(id, user_id, kind, amount, counterparty, ref_id, memo, created_at) "
    "VALUES (1001, 'ci-import-u1', 'fund_in', 100, '', 'ci-import-r1', 'ci import', 10)"
)
conn.commit()
conn.close()
PY

python3 schema/proposals/G06/import_ledger_wallets.py \
    --sqlite "$SQLITE_FILE" \
    --dsn "$DATABASE_URL" \
    --batch-size 10 \
    --reset

psql -X "$DATABASE_URL" -v ON_ERROR_STOP=1 -q <<'SQL'
DO $$
BEGIN
    IF (SELECT balance FROM wallet_balances WHERE user_id = 'ci-import-u1') IS DISTINCT FROM 100 THEN
        RAISE EXCEPTION 'M013 import: wallet_balances 不是 100';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM migration_wallets
        WHERE user_id = 'ci-import-u1' AND status = 'migrated'
    ) THEN
        RAISE EXCEPTION 'M013 import: staging 钱包未标记 migrated';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM ledger_journals WHERE business_key = 'mig:ledger:1001'
    ) THEN
        RAISE EXCEPTION 'M013 import: 缺 journal mig:ledger:1001';
    END IF;
END $$;
SQL

echo "M013 import smoke: OK"

