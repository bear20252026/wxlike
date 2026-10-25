-- G02-02 关系域迁移草稿
--
-- 执行者：G00。幂等；对既有 friends 表只补 `starred` 列，不改既有语义。
-- 回滚：见同目录 README.md「回滚兼容说明」。

-- ---------------------------------------------------------------------------
-- 既有表（规范形状；平台已有同名表时本段为空操作）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS friends (
    user_id    TEXT NOT NULL,
    friend_id  TEXT NOT NULL,
    remark     TEXT NOT NULL DEFAULT '',
    source     TEXT NOT NULL DEFAULT '',
    starred    INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, friend_id)
);
CREATE TABLE IF NOT EXISTS friend_requests (
    id         TEXT PRIMARY KEY,
    from_uid   TEXT NOT NULL,
    to_uid     TEXT NOT NULL,
    message    TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL DEFAULT 'pending',
    created_at INTEGER NOT NULL,
    UNIQUE(from_uid, to_uid)
);
CREATE TABLE IF NOT EXISTS blocks (
    user_id    TEXT NOT NULL,
    blocked_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, blocked_id)
);
CREATE TABLE IF NOT EXISTS groups (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    owner_uid  TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS group_members (
    group_id   TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    role       TEXT NOT NULL DEFAULT 'member',
    my_remark  TEXT NOT NULL DEFAULT '',
    joined_at  INTEGER NOT NULL,
    PRIMARY KEY (group_id, user_id)
);

-- 老库执行前先看 PRAGMA table_info(friends)，缺列才 ALTER。
-- ALTER TABLE friends ADD COLUMN starred INTEGER NOT NULL DEFAULT 0;

-- ---------------------------------------------------------------------------
-- G02 新增
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS friend_tag (
    user_id    TEXT NOT NULL,
    friend_id  TEXT NOT NULL,
    tag        TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, friend_id, tag)
);

CREATE TABLE IF NOT EXISTS group_meta (
    group_id     TEXT PRIMARY KEY,
    visibility   TEXT NOT NULL DEFAULT 'public',
    dissolved_at INTEGER,
    dissolved_by TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS relations_audit (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    actor   TEXT NOT NULL,
    subject TEXT NOT NULL,
    kind    TEXT NOT NULL,
    at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ra_actor ON relations_audit(actor, seq);
