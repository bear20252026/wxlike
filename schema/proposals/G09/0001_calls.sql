-- G09-02 通话记录（提案；编号与落库由 G00/集成方分配）。
-- 一行一次呼叫；participants 细节在 CallRecord 里已足够支撑"最近通话"。
-- 个人数据最小化：只存 user/device id、时间与结束原因，不存 SDP/媒体内容。
CREATE TABLE IF NOT EXISTS calls (
    call_id        TEXT PRIMARY KEY,
    conv_id        TEXT NOT NULL,
    caller         TEXT NOT NULL,
    callee         TEXT NOT NULL,
    caller_device  TEXT NOT NULL,
    answer_device  TEXT,
    video          INTEGER NOT NULL DEFAULT 0,
    generation     INTEGER NOT NULL DEFAULT 1,
    started_at_ms  INTEGER NOT NULL,
    answered_at_ms INTEGER,
    ended_at_ms    INTEGER NOT NULL,
    reason         TEXT NOT NULL CHECK (reason IN (
        'rejected', 'cancelled', 'hung_up', 'busy', 'timeout', 'failed',
        'authorization_revoked'
    ))
);

CREATE INDEX IF NOT EXISTS idx_calls_conv
    ON calls(conv_id, ended_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_calls_caller
    ON calls(caller, ended_at_ms DESC);
CREATE INDEX IF NOT EXISTS idx_calls_callee
    ON calls(callee, ended_at_ms DESC);

-- 回滚兼容说明（additive only）：
--   * 不修改既有表/列，旧版本二进制不读取 calls，可先回滚应用再删表。
--   * 回滚 SQL（按反序）：
--       DROP INDEX IF EXISTS idx_calls_callee;
--       DROP INDEX IF EXISTS idx_calls_caller;
--       DROP INDEX IF EXISTS idx_calls_conv;
--       DROP TABLE IF EXISTS calls;
--   * 数据丢失范围：仅通话历史（"最近通话"）。媒体与消息不受影响。
