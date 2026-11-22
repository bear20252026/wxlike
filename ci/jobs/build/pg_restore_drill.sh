#!/usr/bin/env bash
# G00-04 S2 PostgreSQL 备份/恢复演练。
#
# 前提：DATABASE_URL 指向已执行 M010-M013 的库；PATH 里有 pg_dump / pg_restore / psql。
# 流程：pg_dump 自定义格式 -> 在新库 pg_restore -> 在新库重跑 M010/M011-M013 断言。
# 断言会写最小 fixtures 并自行清理；pg_restore 必须保留视图、函数与触发器。
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
DUMP_FILE="${DUMP_FILE:-/tmp/wxlike-restore.dump}"

echo "restore drill: pg_dump -> pg_restore -> same assertions"

pg_dump "$DATABASE_URL" \
  --no-owner --no-privileges --format=custom --file="$DUMP_FILE"

psql -X "$ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -c "DROP DATABASE IF EXISTS \"${RESTORE_DB}\" WITH (FORCE);"
psql -X "$ADMIN_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -c "CREATE DATABASE \"${RESTORE_DB}\";"

pg_restore --no-owner --no-privileges --exit-on-error \
  --dbname="$RESTORE_DATABASE_URL" "$DUMP_FILE"

# 恢复出来的库必须能过同一组 M010/M011-M013 断言：这同时验证了
# 表、视图、函数、触发器、序列与索引都完整恢复，而不只是“行数对得上”。
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts.sql
psql -X "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -f ci/jobs/build/pg_migration_asserts_compat.sql

echo "restore drill: OK"

