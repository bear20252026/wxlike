# H-37：相同内容提交泄露他人完整 job

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `e94a2d8`。

审计原文：`AUDIT-60895e18.md:409-415`。`job_id == video_id == SHA256(content)`；
B 提交与 A 相同的字节会命中 A 的作业，`submit_job` 返回完整 `Job`，带出 A 的
owner、callback URL、callback_error、last_error 等字段。单独 GET 有 ownership
检查，submit 这条路径没有。

## 修复

`crates/video/src/jobs.rs`：

- `Job::public_view_for(caller)`：只保留 `job_id/state/attempts/timestamps/
  source_size/result` 等公共字段；`owner` 改成当前调用者，`callback_url/
  callback_error/callback_attempts/callback_next_retry_at/last_error` 全部清空，
  `callback_state=None`。
- `submit_reply(job_id, caller, newly_queued)` 读出作业后按 owner 分流：
  - 当前 owner：原样返回；
  - 非 owner：返回 `public_view_for(caller)`。
- `record_ready_job` 的 `ON CONFLICT` 不再把已有行的 callback 状态/重试时间
  重置成后来者的值：`callback_state` 只允许 `delivered` 保持，否则保留原值；
  `callback_next_retry_at` 在 `pending` 时保留原值。B 重提同一份已就绪内容
  不会压掉 A 还没发的 terminal callback。
- 已有行的 `owner`、`callback_url` 本来就不在 `DO UPDATE SET` 里，继续保留。

## 证据

`cargo test -p wxlike-video --test jobs_queue`：18 passed。

新增 `submit_same_bytes_does_not_expose_the_other_owners_job`：

1. alice 带 callback URL 提交字节，得到 pending 作业；
2. `run_job` 让假字节产生 `last_error`；
3. bob 提交同一份字节，`newly_queued=false`；
4. 断言 bob 的响应里 owner=bob、`callback_url/callback_error/last_error`
   全为 None、`callback_state=None`；
5. 回读数据库，alice 的 owner 和 callback URL 未被改写。

## 边界

作业行仍然只有一份（内容寻址设计）；非 owner 的 submit 响应是只读公共视图，
bob 之后用 `GET /api/video/jobs/{id}` 仍会 404。若产品需要 bob 也能轮询同一份
内容的进度，需要引入 per-caller 的提交记录或允许只读公共 GET，不能靠把 A 的
owner 改给 B。回调地址仍可能通过其他管理面/日志泄露，需配合日志脱敏。
