# 生产部署

单机 Compose 形态见 `containers/server/compose.prod.yml`；镜像必须是已签名 digest，
不要用 `latest`。完整发布流程见 `deploy/runbooks/release.md`。

## 上线前检查

- [ ] `deploy/production/wxlike.env` 为 0600，所有 `CHANGE_ME` 已替换；
- [ ] 镜像 digest 与 `release-manifest.json` 一致，`cosign verify` 通过；
- [ ] 监控已抓取 `/metrics`，`prometheus-alerts.yml` 已加载；
- [ ] 日志采集已接入，`check-log-redaction.py` 对样本日志通过；
- [ ] 最近一次 `restore-drill.sh` 在 7 天内，`drill.json` 记录了 RPO/RTO；
- [ ] 回滚镜像与当前版本都在 registry 中保留；
- [ ] 已知缺口清单（`ci/jobs/policy/known-gaps.json`）没有新增 release-blocking 项。

## 目录

| 文件 | 用途 |
|---|---|
| `wxlike.env.example` | 生产环境变量模板（无真实值） |
| `prometheus-alerts.yml` | 告警规则 |
| `grafana-dashboard.json` | 看板 |
| `observability.md` | 指标/日志/脱敏/缺口 |
| `backup.sh` / `restore-drill.sh` | 备份与恢复演练（实测 RPO/RTO） |
| `../runbooks/` | 事故、发布、回滚、备份恢复手册 |

## 未闭环的硬限制

- 数据库仍是 SQLite（G00-04 未合入 PG），单机写入；多节点共享同一 SQLite 文件
  不受支持。
- `/health` 只是存活探针；没有就绪探针（`READINESS-ENDPOINT`）。
- 日志没有统一 request id（`OBS-TRACE-ID`）。
- 移动端没有真机证据（`MOBILE-DEVICE-EVIDENCE`），发布门禁会失败。
