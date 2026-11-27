# H-41：Core send 丢 quote、幂等键分裂、URL 编码损坏 UTF-8

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `01813de`。

审计原文：`AUDIT-60895e18.md:442-451`。

## 修复

### quote 字段

`crates/client-core/src/runtime/router.rs`：

- `msg.send` 的 body 字段从 `["text","idempotency_key"]` 扩为
  `["text","quote_of","idempotency_key"]`。
- 新增 `wire_aliases`：命令面契约参数仍是 `reply_to`（catalog/MCP/客户端的
  既有名字），路由层唯一一次翻译成服务端 `SendBody` 的 `quote_of`。若调用方
  已经显式给 `quote_of`，不覆盖。
- 不再让两个名字在 Executor 与 Core 各翻译一遍。

### 单一幂等键来源

- `ActionRequest.idempotency_key` 为唯一权威。`Session::send_now` 在路由前调用
  `router::with_request_idempotency(params, key)`，把独立键注入 wire params。
- params 里若已有同名键，只接受与请求键一致的旧调用；不一致返回
  `invalid_argument`，不发请求。没有请求键却带 params 键也拒绝，避免绕过
  `submit_write` 的必填校验。
- outbox 仍保存不含 params 键的原始参数；`pump` 用
  `record.idempotency_key` 重建请求时再注入，所以重启补发带的是同一个键。
- `crates/client-core/tests/runtime_contract.rs` 的 `send_request` 不再同时放
  params 键和独立键——旧测试正是靠两份相同的键掩盖了分裂。

### URL percent-encoding

`percent_encode` 改为按 UTF-8 字节编码：RFC 3986 unreserved
（`A-Z a-z 0-9 - . _ ~`）原样保留，其余一律 `%XX`（大写十六进制）。
旧实现 `other as char` 会把 0xE4 变成 'ä'，中文/emoji 路径与查询全部损坏。

## 证据

`cargo test -p wxlike-client-core`：lib 33 + integration 18 passed。

新增/调整：

- `runtime::router::tests::non_ascii_path_and_query_are_utf8_percent_encoded`：
  `"你好 😀"` → `%E4%BD%A0%E5%A5%BD%20%F0%9F%98%80`；`"会话/一"` →
  `%E4%BC%9A%E8%AF%9D%2F%E4%B8%80`。
- `msg_send_carries_reply_to_as_quote_of`：body 有 `quote_of`，没有
  `reply_to`。
- `request_idempotency_key_is_the_only_source`：注入、同值兼容、异值拒绝。
- `runtime_contract` 的 `killing_between_commit_and_ack_does_not_lose_the_message`
  在 params 不含键的情况下仍断言补发 body 带 `k-1`。

## 边界

- 这次只保证 Core 路由发出的 body 正确；服务端对 `quote_of` 的会话归属/撤回
  校验沿用既有 `SendBody` 逻辑，没有新增服务端用例。
- catalog 参数名仍是 `reply_to`（不是 `quote_of`），只有 wire body 叫
  `quote_of`；MCP 工具描述与桌面调用方无需改名。
- 复合命令（`conv.list` / `conv.ack`）没有幂等键语义，仍按原路径。
