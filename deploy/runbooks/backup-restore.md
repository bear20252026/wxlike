# 备份与恢复

## 目标

| 指标 | 定义 | 当前状态 |
|---|---|---|
| RPO | 可接受丢失的时间窗 | 由备份频率决定；建议每小时 `backup.sh` |
| RTO | 从开始恢复到 `/health` 通过 | 每次 `restore-drill.sh` 实测并写入 `drill.json` |
| 保留 | 备份保留期 | 本地 14 天 + 异地 30 天（按合规要求调整） |

**没有实测的 RTO 不算数**。发布前跑一次演练，把 `drill.json` 附在发布证据里。

## 备份

```bash
deploy/production/backup.sh \
  --db /srv/wxlike/wxlike.db \
  --media /srv/wxlike/media \
  --out /var/backups/wxlike \
  --label hourly
```

脚本做：

1. `sqlite3 .backup`（在线一致快照，不是 `cp`）；
2. `PRAGMA integrity_check` 必须为 `ok`；
3. 记录 schema user_version、用户数、消息数；
4. 打包 `media/` 并计算 SHA256；
5. 写 `manifest.json`（含耗时和校验和）。

cron 示例：

```cron
17 * * * * /opt/wxlike/deploy/production/backup.sh --db /srv/wxlike/wxlike.db --media /srv/wxlike/media --out /var/backups/wxlike --label hourly >> /var/log/wxlike-backup.log 2>&1
```

## 恢复演练（不改生产）

```bash
deploy/production/restore-drill.sh \
  --backup /var/backups/wxlike/hourly-20261024T160000Z \
  --out /tmp/wxlike-restore-drill \
  --server-bin /opt/wxlike/bin/wxlike-server
```

校验：数据库哈希与 manifest 一致、`integrity_check=ok`、用户/消息计数一致、
真实二进制能在此库上启动并通过 `/health`。输出 `drill.json`：

```json
{"rpo_seconds": 3600, "rto_ms": 4200, "server_started": true, "integrity_check": "ok"}
```

`rpo_seconds` 是“恢复开始时这份备份有多旧”，不是承诺的 RPO 上限。

## 生产恢复

1. 停止写入：`systemctl stop wxlike` / `docker stop`，确认进程退出码 0；
2. 保留现场：把当前 db 和 media 移到 `*.corrupt-<ts>`，不要覆盖；
3. 解包备份到数据目录，保持属主 `10001:10001`；
4. 启动，等待 `/health` 200；
5. 跑一次对账：`POST /api/admin/reconcile`（`x-admin-token`），要求 `ok=true`；
6. 抽样登录、历史、转账各一条；
7. 记录实际 RTO 和丢失窗口，更新事故记录。

## 注意

- 恢复后旧会话 token 仍然有效（会话在数据库里）；若怀疑泄露，用
  `POST /api/sessions/revoke-others` 或提高 session 版本（见认证代码）。
- SQLite 恢复必须是整库替换，不能把旧库的 `-wal` 文件留下。
- PG 割接（G00-04）完成后，此手册需要改为 `pg_dump/pg_restore` 并重新演练；
  在完成前不要声称支持 PG 备份。
