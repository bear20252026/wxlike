# H-20：stale running video job 不可 claim

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `3cfbae1`。

审计原文：`AUDIT-60895e18.md:269-274`。`due_jobs` 会把 `running` 超过
`STALE_RUNNING_MS` 的作业选出来，但 `claim_job` 只接受 `pending`；`run_job` 领不到就
报错，`run_worker_round` 用 `?` 中止整轮。worker 崩溃后的 stale running 永卡，并阻断
后续任务。

## 修复

`crates/video/src/jobs.rs`：

- `claim_job_at(job_id, now_ms)` 的 CAS 条件改为：

  ```sql
  UPDATE video_jobs SET state = 'running', updated_at = ?2
   WHERE job_id = ?1
     AND (state = 'pending'
          OR (state = 'running' AND updated_at <= ?3))
  ```

  `?3 = now_ms - STALE_RUNNING_MS`。同一个写锁 + affected rows 保证两个 worker 只有
  一个能重新领到 stale running；正常 `running` 且未过期仍然领不到。
- `claim_job` 用当前时钟调用 `claim_job_at`；测试可直接注入时钟。
- `run_job` 保留原签名（claim + 执行）；新增私有 `run_claimed_job` 承载“已领到”
  之后的读源/转码/写回。
- `run_worker_round` 先 `claim_job`，CAS 失败直接 `continue`，不再把“别人先领走”
  当错误中止整轮；已领到的作业再走 `run_claimed_job`。

## 证据

`cargo test -p wxlike-video --test jobs_queue`：17 passed。

- `due_respects_the_clock_and_reclaims_stale_running`：原有 due/claim CAS 断言仍通过。
- 新增 `stale_running_job_is_reclaimed_and_run`：
  - 用 `claim_job_at(job, now - STALE_RUNNING_MS - 1000)` 制造一条过期 running；
  - `due_jobs(now)` 能选中它；
  - `run_job` 不再报 `not claimable`，实际执行一轮（`attempts == 1`，状态离开
    `running`；没有 ffmpeg 时进入可重试的 `pending`）。
- `missing_staged_source_is_a_retryable_failure_not_a_panic` 继续验证近期 running
  的第二次 `claim_job` 仍为 false。

## 边界

当前没有 worker 心跳，只有 `updated_at` 的领取时间。一个合法但超过 30 分钟的转码
仍可能被另一个 worker 认为 stale 并重复领取。下一步若视频作业允许长于
`STALE_RUNNING_MS`，需要增加周期性 heartbeat，或把阈值与最大转码时长绑定。该修复
只保证 stale 可回收、队列不被单条坏作业阻断；真实 ffmpeg 长任务并发回收仍需要
带 ffmpeg 的集成测试。
