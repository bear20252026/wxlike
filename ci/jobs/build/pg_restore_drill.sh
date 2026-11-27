#!/usr/bin/env bash
# G00-04 S2 PostgreSQL 备份/恢复演练。
#
# 前提：DATABASE_URL 指向已执行 M010-M024 的库；PATH 里有 pg_dump / psql。
# 流程：pg_dump plain SQL -> 过滤客户端版本特有的 SET -> 新库 psql 还原 ->
# 在新库重跑 M010-M024 断言。断言会写最小 fixtures 并自行清理。
#
# 为什么不用 custom + pg_restore：plain SQL 只过滤客户端版本特有的 SET，
# 保留全部 DDL/DML，跨小版本更稳。CI 用服务容器内的 PG 19.0 pg_dump，
# 避免 runner 自带客户端版本低于服务端时被直接拒绝。
#
# 用法：
#   DATABASE_URL=postgresql://postgres:postgres@localhost:5432/wxlike \
#   ADMIN_DATABASE_URL=postgresql://postgres:postgres@localhost:5432/postgres \
#   RESTORE_DATABASE_URL=postgresql://postgres:postgres@localhost:5432/wxlike_restore \
#   bash ci/jobs/build/pg_restore_drill.sh

set -euo pipefail

: "${DATABASE_URL:?DATABASE_URL is required}"
ADMIN_DATABASE_URL="${ADMIN_DATABASE_URL:-${DATABASE_URL%/*}/postgres}"
RESTORE_DATABASE_URL="${RESTORE_DATABASE_URL:-${DATABASE_URL%/*}/wxlike_restore}"
RESTORE_DB="${RESTORE_DB:-wxlike_restore}"
DUMP_FILE="${DUMP_FILE:-/tmp/wxlike-restore.sql}"

echo "restore drill: pg_dump -> filter client-only SET -> psql restore -> same assertions"

DUMP_CMD=(pg_dump)
if [[ -n "${PG_DUMP_CONTAINER:-}" ]]; then
  DUMP_CMD=(docker exec "$PG_DUMP_CONTAINER" pg_dump)
fi

"${DUMP_CMD[@]}" "$DATABASE_URL" \
  --no-owner --no-privileges --format=plain \
  | sed '/transaction_timeout/d' > "$DUMP_FILE"

psql -X "$ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -c "DROP DATABASE IF EXISTS \"${RESTORE_DB}\" WITH (FORCE);"
psql -X "$ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -c "CREATE DATABASE \"${RESTORE_DB}\";"

psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q -f "$DUMP_FILE"

# 恢复出来的库必须能过同一组 M010-M024 断言：这同时验证了
# 表、视图、函数、触发器、序列与索引都完整恢复，而不只是“行数对得上”。
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_compat.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_content.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_identity_retry.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_identity_session_meta.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_identity_session_meta_table.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_identity_session_meta_cleanup.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_g03.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_import.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_import_compat.sql

echo "restore drill: OK"
