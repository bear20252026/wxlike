-- M016：PG 兼容层的会话侧车列（auth_level / features / serial / issued_at）。
--
-- 背景：SQLite 把访问会话（`sessions`）与令牌载荷侧车
-- （`identity_session_token`）分成两张表；M011 的 PG 兼容层把
-- `identity_session_token` 当成访问会话表，只保留了 token/user/sid/设备列，
-- 遗漏了侧车四列。刷新轮换要读 `features` 判断临时认证（`L` 位），
-- 没有该列时 `SELECT features` 在 PostgreSQL 上直接报 undefined column。
--
-- 本迁移只补列，不改主键、不改既有行；M011 已写入的访问会话拿到
-- `auth='auth'` / `features=''` / `serial=0` / `issued_at=0`，可被现有读路径
-- 解释为普通永久会话。expand-only、ADD COLUMN IF NOT EXISTS 可重入。
--
-- 回滚：四列均为新增，DROP COLUMN 即可；已有访问会话仍保留。

BEGIN;

ALTER TABLE identity_session_token
    ADD COLUMN IF NOT EXISTS auth_level TEXT NOT NULL DEFAULT 'auth',
    ADD COLUMN IF NOT EXISTS features   TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS serial     BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS issued_at  BIGINT NOT NULL DEFAULT 0;

COMMIT;
