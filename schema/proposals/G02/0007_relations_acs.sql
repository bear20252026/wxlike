-- M021：关系域 ACL / 隐私默认位 / 群成员细粒度位。
--
-- 背景：SQLite 侧 `crates/features/relations` 的 SCHEMA 已建 friend_acs /
-- friend_policy / group_member_acs，但 M014 的 PG 平台兼容层没有这三张表。
-- 缺表时关系域的 want/given 与默认隐私位在 PG 上无处落盘。
--
-- 语义借 Tinode AccessMode：
--   mode_want / mode_given 空串 = UNSET（走类目默认），'N' = 明确拒绝；
--   friend_policy 是我对“已有关系者/陌生人”的默认位。
--
-- expand-only、CREATE TABLE IF NOT EXISTS 可重入。
-- 回滚：DROP 本文件新建的 3 张表；不触碰既有 friends/groups 表。

BEGIN;

CREATE TABLE IF NOT EXISTS friend_acs (
    user_id    TEXT NOT NULL,
    peer_id    TEXT NOT NULL,
    mode_want  TEXT NOT NULL DEFAULT '',
    mode_given TEXT NOT NULL DEFAULT '',
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (user_id, peer_id)
);

CREATE TABLE IF NOT EXISTS friend_policy (
    user_id     TEXT PRIMARY KEY,
    access_auth TEXT NOT NULL DEFAULT '',
    access_anon TEXT NOT NULL DEFAULT '',
    updated_at  BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS group_member_acs (
    group_id   TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    mode_want  TEXT NOT NULL DEFAULT '',
    mode_given TEXT NOT NULL DEFAULT '',
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (group_id, user_id)
);

COMMIT;
