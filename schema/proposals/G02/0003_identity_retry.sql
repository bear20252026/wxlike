-- M015：身份域“凭据重试预算”表（identity_retry）。
--
-- 背景：`identity_challenge` 的 max_attempts 只约束单枚验证码；重发会换一行
-- 新挑战，跨重发的累计试错必须落在独立的预算桶上。SQLite 侧一直有这张表，
-- 但 M014 的 PG 兼容层漏了它，导致 `throttle::check_budget` 等函数只能在
-- SQLite 上跑。本迁移只补表与过期索引，expand-only。
--
-- 幂等：CREATE TABLE / INDEX IF NOT EXISTS；可重复执行。
-- 回滚：DROP TABLE identity_retry（会丢失尚未到期的重试计数，不影响会话）。

BEGIN;

CREATE TABLE IF NOT EXISTS identity_retry (
    bucket      TEXT PRIMARY KEY,
    attempts    BIGINT NOT NULL,
    created_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL,
    expires_at  BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_identity_retry_expiry
    ON identity_retry(expires_at);

COMMIT;
