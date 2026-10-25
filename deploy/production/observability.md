# 生产可观测性

## 抓取

```yaml
scrape_configs:
  - job_name: wxlike
    metrics_path: /metrics
    static_configs:
      - targets: ["wxlike.internal:8099"]
  - job_name: wxlike-blackbox
    metrics_path: /probe
    params: {module: [http_2xx]}
    static_configs:
      - targets: ["http://wxlike.internal:8099/health"]
    relabel_configs:
      - source_labels: [__address__]
        target_label: __param_target
```

`/metrics` 是 Prometheus 文本格式，无需认证，因此**不要直接暴露到公网**：只允许
监控网段访问，前面用反向代理或安全组限制。`/health` 是存活探针，不代表数据库/
消息队列可用；就绪探针仍是 `READINESS-ENDPOINT` 缺口。

## 指标与告警

指标定义见 `crates/net/src/metrics.rs`；告警在 `prometheus-alerts.yml`。

| 指标 | 用途 | 告警 |
|---|---|---|
| `wxlike_hub_connections` | 在线连接 | 突降结合 `WxlikeInstanceDown` 判断 |
| `wxlike_hub_messages_total` | 路由吞吐 | `WxlikeNoTraffic` |
| `wxlike_offline_parked` | 离线积压 | >10000 持续 10m |
| `wxlike_delivery_seconds` | 入站→路由完成 | p99 >500ms 持续 10m |
| `wxlike_rate_limit_reject_total` | 发送门限拒绝 | 有任何增长即提示 |
| `up{job="wxlike"}` | 抓取可达 | 2m 失败 critical |
| `absent(wxlike_hub_info)` | 指标缺失 | 5m 即告警 |

`absent()` 规则不是多余：实例还在回 200 但 recorder 安装失败时，普通的
`rate(...)>x` 永远不触发，只有缺失检测能发现“看起来健康、实际没有数据”。

## 日志

现状：`tracing_subscriber` 输出到 stdout，由容器日志驱动轮转（20m×5）。
日志**没有**统一 request id——这是 `OBS-TRACE-ID` 缺口，值班无法按一次请求
串起 HTTP → hub → 投递。在缺口关闭前，用业务 id 关联：

- 消息：`msg_id`（16 字节 hex）
- 转账：`transfer_id`
- 对账：`at_ms` + `reconcile_runs` 行
- 会话：`sid`

要求（缺口关闭的验收口径）：每个 HTTP 请求生成/接收 `x-request-id`，写入
结构化日志的 `request_id` 字段，并透传到 hub 路由日志；同一请求的所有行可
用 `request_id` 过滤。当前代码没有该字段，因此不能声称已具备。

## 脱敏

`deploy/production/check-log-redaction.py` 扫描采集到的日志，命中以下模式即
失败：`Authorization: Bearer ...`、`x-admin-token`、`password=`、
`WXLIKE_LIVE_INGEST_SECRET`/`WXLIKE_TURN_SECRET` 的值、keystore/证书 base64、
私钥块。CI 的 e2e 证据也走同一套脱敏（harness 把 token 替换为 `<redacted>`）。

运行：

```bash
python deploy/production/check-log-redaction.py /var/log/wxlike/*.log
python deploy/production/check-log-redaction.py --self-test
```

## 看板

`grafana-dashboard.json` 可直接导入：连接数、消息速率、p99、离线积压、
限流拒绝、实例状态。面板查询与告警同源，避免“看板绿、告警红”。

## 值班入口

- 事故处理：`deploy/runbooks/incident.md`
- 发布/回滚：`deploy/runbooks/release.md`、`deploy/runbooks/rollback.md`
- 备份/恢复：`deploy/runbooks/backup-restore.md`
