# 事故处理

## 通用顺序

1. **确认影响面**：`/health`、`/metrics`、Prometheus `up`、最近一次发布 digest；
2. **止血优先**：回滚（见 `rollback.md`）或摘流，不先做根因分析；
3. **保留证据**：`docker logs --since 30m > incident-<ts>.log`，脱敏后再外发；
4. **通知**：记录开始时间、影响用户、当前版本、临时措施；
5. **恢复验证**：`/health`、对账 `ok=true`、抽样消息/转账；
6. **复盘**：时间线、根因、为什么监控没提前发现、缺口归属。

## Instance down

`up{job="wxlike"} == 0` 持续 2 分钟。

```bash
docker ps --filter name=wxlike
docker logs --tail 200 <container>
docker inspect <container> --format '{{.State.OOMKilled}} {{.State.ExitCode}}'
```

- OOM → 查 `wxlike_offline_parked` 与流量；临时提高内存或限流；
- 退出码 0 → 被信号停止（部署/人为），确认是否有人在发布；
- 启动即退出 → 看是否缺 `WXLIKE_LIVE_INGEST_SECRET`、数据目录权限、DB 损坏。

## Scrape missing / metrics missing

`absent(up{job="wxlike"})` 或 `absent(wxlike_hub_info)`。

1. 从监控机 `curl http://<host>:8099/metrics`；
2. 网络/安全组是否只允许监控网段；
3. 200 但无 `wxlike_hub_info` → recorder 安装失败，看启动日志 `[metrics]`；
4. 200 且有指标 → Prometheus 配置/label 不匹配。

## Health failed

`probe_success{job="wxlike-blackbox"} == 0`。

`/health` 只证明进程在监听，**不证明数据库可用**（就绪探针缺口）。先看日志是否有
db/mailbox 错误，再做一次只读请求确认。

## Latency

`wxlike_delivery_seconds` p99 >500ms 持续 10 分钟。

- 看 `wxlike_offline_parked` 是否同步上涨（积压）；
- 看 `wxlike_rate_limit_sleep_total` 是否上涨（发送门限在拖延）；
- 看宿主机 CPU/IO；SQLite 写入放大时优先减少非必要后台任务；
- 不通过调大超时掩盖，先确认是否发布引入。

## Backlog

`wxlike_offline_parked > 10000`。离线用户积压会吃内存；确认是否单 hub 故障导致
大量重连，必要时扩容 hub 或触发客户端重新同步。不要手工清空信箱表。

## Rate limit

看 `rate_limiter_name` label 区分门限。若单用户异常，先封禁/限速该用户；若全局
增长，检查是否被刷接口或客户端重试风暴（idempotency key 是否正确使用）。

## No traffic

连接数 >0 但 `wxlike_hub_messages_total` 15 分钟无增长：客户端可能连着但发不出
消息。检查发送门限、MQ 后端（`WXLIKE_MQ=redis` 时 Redis 可用性）与 hub 日志。
