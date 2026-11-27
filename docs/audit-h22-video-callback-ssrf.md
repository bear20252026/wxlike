# H-22：视频回调 SSRF TOCTOU 与终态回调被压制

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `a2817fc`。

审计原文：`AUDIT-60895e18.md:283-291`。两处问题：

1. `CallbackPolicy::resolve` 只校验 DNS 结果、不返回地址；`deliver_callback_signed`
   里 ureq 再解析一次，存在 DNS rebinding 窗口。
2. `due_callbacks` 只按 `callback_state='pending'` 选，retryable transcode failure
   仍是 `pending` 时就会发 preterminal 回调；2xx 标 `delivered` 后压制最终成功通知。

## 修复

`crates/video/src/policy.rs`：

- `resolve(url)` 返回 `Vec<IpAddr>`：解析后把实际地址交给调用方，校验和连接用
  同一组 IP。公网模式仍对**每一个**地址 `ensure_public`；`allow_private` 用于
  本机/测试。
- 主机字面量直接返回该 IP，不再查 DNS。

`crates/video/src/jobs.rs`：

- 新增 `PinnedResolver`：ureq 的 `resolve` 只接受 URI host 与校验时一致的请求，
  返回预先校验过的 IP:port；host 不一致直接 `HostNotFound`。
- `deliver_callback_signed_to(url, body, token, pinned)` 用
  `ureq::Agent::with_parts(config, DefaultConnector, PinnedResolver)` 建 agent；
  `deliver_callback_signed` / `deliver_callback` 保留原签名，传空 pin 走系统 DNS
  （测试/内部调用）。
- worker 发回调时先 `resolve`，再把返回 IP 传给 `deliver_callback_signed_to`。
- agent 显式 `.proxy(None)`：环境里的 `HTTP(S)_PROXY` 会由代理自己解析目标，
  固定 IP 就失效了；回调必须直连（安全优先）。需要代理的部署要显式支持，
  不能吃环境变量。
- `DUE_CALLBACKS_SQL` 增加 `AND state IN ('succeeded','failed')`：只有终态作业
  才发终态回调；retryable failure（`pending`）不会再提前发。

## 证据

`cargo test -p wxlike-video --test jobs_queue`：20 passed，新增：

- `callback_connector_uses_only_the_pinned_address`：sink 监听
  `127.0.0.1:<port>`，URL host 故意写不可解析的 `pinned.invalid`，只传
  `[127.0.0.1]` 作为 pin。请求成功，证明 connector 没有重新查 DNS。
- `retryable_failure_does_not_fire_a_preterminal_callback`：retryable 失败后
  `state=pending`，`due_callbacks` 不含该作业；permanent 失败进入 `failed`
  后才 due。
- `submit_same_bytes_does_not_expose_the_other_owners_job` 同时覆盖 H-37 的
  callback 字段隔离。

`cargo test -p wxlike-video --test jobs_pipeline -- --ignored`：3 passed，
真实转码 + 回调（worker 的 pinned 路径）。

## 边界

- 回调 DNS 仍由 `CallbackPolicy::resolve` 的 `tokio::lookup_host` 解析；解析
  结果被固定，但解析本身仍受本地 DNS 污染影响，白名单+公网校验是防线。
- 回调目标如果是 HTTPS，TLS SNI 仍用 URL host；`PinnedResolver` 只改连接
  IP，不改 SNI，证书校验按 host 正常执行。
- 代理部署暂不支持：`.proxy(None)` 是有意的，避免代理重新解析目标。需要
  代理时应在 `CallbackPolicy` 里显式配置受信任代理并让代理只连固定 IP，
  不能直接恢复环境变量。
- 终态判定依赖 `JobState`：`Succeeded`/`Failed` 才发；`Pending` 的 retryable
  失败不发。`requeue_job` 会把 failed 重新排 pending，回调也会重新 arm。
