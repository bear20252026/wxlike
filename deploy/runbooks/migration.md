# 数据库迁移与回滚

## 现状

服务端使用 SQLite，迁移在 `start_stack` 打开数据库时执行（`crates/platform/src/db.rs`）。
迁移是**向前执行**、幂等的 SQL；没有自动 down 迁移。PostgreSQL 割接仍是
`G00-04-PG` 缺口。

## 发布前

1. 跑一次 `deploy/production/backup.sh`，确认 `manifest.json` 的
   `integrity_check=ok`；
2. 在备份副本上启动新二进制，确认迁移完成且 `/health` 200；
3. 记录旧版本镜像 digest 与 schema `user_version`。

## 迁移失败的行为

- 数据库不可读/损坏：进程非零退出，**不会**带着半迁移的库继续服务。
  该行为由 `tests/fault/run.py --suite migration` 实测（损坏文件 → exit 1）。
- 迁移执行到一半断电：SQLite 事务保证单条迁移原子；下一次启动重新执行未完成
  的迁移。不要手工改 `user_version`。

## 回滚

1. 停止服务（SIGTERM，等待排空）；
2. 优先回滚**镜像**到旧 digest；若旧版本无法读取新 schema，再恢复发布前备份；
3. 恢复步骤见 `backup-restore.md`；恢复会丢失备份点之后的写入；
4. 启动后跑 `POST /api/admin/reconcile`，要求 `ok=true`；
5. 记录实际丢失窗口（RPO）与恢复耗时（RTO）。

## 不允许

- 用 `git checkout` 回退迁移文件后直接启动旧库（已执行的迁移不会自动撤销）；
- 手工 `UPDATE` 余额/账本“修数据”（对账会失败，且破坏审计）；
- 在没有备份的情况下执行破坏性迁移。

PG 割接完成后，本手册必须改为 `sqlx migrate`/`pg_restore` 流程并重新演练；
在此之前不要声称支持 PG 回滚。
