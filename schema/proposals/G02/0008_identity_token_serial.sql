-- M022：全局令牌序列号。
--
-- 背景：identity 的 `rotate_serial` 把所有已签发访问令牌一次性作废（等价
-- 上游换掉 serial_num），但 M014 的 PG 身份兼容层没有 identity_token_serial。
-- 缺表时 PG 上的全局轮换只能退化成“没有序列号”或直接报错。
--
-- 形状对齐 SQLite：单行表，id 恒为 1，避免“取最新一行”这种靠 ORDER BY
-- 兜底的读法。serial 单调递增，rotated_at 是毫秒时间戳。
--
-- expand-only、CREATE TABLE IF NOT EXISTS 可重入。
-- 回滚：DROP 本表；已签发令牌不受影响，但下一次轮换会重新从 0 起算。

BEGIN;

CREATE TABLE IF NOT EXISTS identity_token_serial (
    id         BIGINT PRIMARY KEY CHECK (id = 1),
    serial     BIGINT NOT NULL,
    rotated_at BIGINT NOT NULL
);

COMMIT;
