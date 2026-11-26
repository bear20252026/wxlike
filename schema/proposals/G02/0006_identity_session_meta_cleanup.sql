-- M018：访问会话 token 被删除时，清掉同 sid 的侧车行。
--
-- 背景：M017 把 `identity_session_meta` 拆成独立表，platform repo 的
-- `delete_by_token` / `delete_by_user` / `delete_expired_batch` 只删
-- `identity_session_token`。如果侧车不跟着删，就会留下永久孤儿行。
--
-- 触发器只在该 sid 已没有任何访问 token 时删除侧车，避免"同 sid 多 token"
-- 时提前清掉共享侧车。identity 自己的 logout 仍可显式删两张表；触发器是
-- 兜底，不是替代。
--
-- expand-only、CREATE OR REPLACE / DROP TRIGGER IF EXISTS 可重入。
-- 回滚：DROP TRIGGER + DROP FUNCTION；侧车表和数据不受影响。

BEGIN;

CREATE OR REPLACE FUNCTION identity_session_meta_cleanup()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.sid IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM identity_session_token WHERE sid = OLD.sid
    ) THEN
        DELETE FROM identity_session_meta WHERE sid = OLD.sid;
    END IF;
    RETURN OLD;
END;
$$;

DROP TRIGGER IF EXISTS identity_session_meta_cleanup ON identity_session_token;
CREATE TRIGGER identity_session_meta_cleanup
AFTER DELETE ON identity_session_token
FOR EACH ROW
EXECUTE FUNCTION identity_session_meta_cleanup();

COMMIT;
