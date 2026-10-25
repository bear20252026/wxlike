-- G05 内容 / 社交 / 收藏 / 检索 迁移草稿（G05-01..06 全部六卡）
--
-- 执行者：G00（迁移编号与串行合入归 G00，本文件只是草稿）。
-- 幂等：全部 IF NOT EXISTS；对既有 `moments` / `favorites` 只做"补列"，
-- 不改既有列语义。crate 内 `Db::migrate_g05()`（crates/platform/src/db.rs）
-- 已经按同样逻辑执行：G00 可直接复用，也可把本文件写进正式迁移脚本。
-- 回滚：见同目录 README.md「回滚兼容说明」。

-- ---------------------------------------------------------------------------
-- G05-01 朋友圈草稿：发布前的文本/媒体/可见性可反复保存，
-- 发布成功后客户端删除；表本身不含"已发布"状态。
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS moment_drafts (
    id             TEXT PRIMARY KEY,
    author         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    text           TEXT NOT NULL DEFAULT '',
    media          TEXT NOT NULL DEFAULT '[]',
    visibility     TEXT NOT NULL DEFAULT 'friends',
    audience_group TEXT NOT NULL DEFAULT '',
    updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_moment_drafts_author
    ON moment_drafts(author, updated_at);

-- 既有 moments 表补列：分组可见时记录作者的好友分组名（读时判定可见性）。
-- ALTER TABLE moments ADD COLUMN audience_group TEXT NOT NULL DEFAULT '';

-- ---------------------------------------------------------------------------
-- 内容 -> 媒体引用。删除内容时同步删行；用于回答"附件还被谁引用"
-- 以及未来按引用计数清理孤儿对象。唯一约束让重复发布幂等。
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS content_media_refs (
    media_id   TEXT NOT NULL,
    ref_kind   TEXT NOT NULL,
    ref_id     TEXT NOT NULL,
    owner      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (ref_kind, ref_id, media_id)
);
CREATE INDEX IF NOT EXISTS idx_cmr_media ON content_media_refs(media_id);

-- ---------------------------------------------------------------------------
-- G05-02 发布者（一个用户一个发布者；不承诺导入微信后台）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS publishers (
    id              TEXT PRIMARY KEY,
    owner           TEXT NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    bio             TEXT NOT NULL DEFAULT '',
    avatar_media_id TEXT NOT NULL DEFAULT '',
    created_at      INTEGER NOT NULL
);

-- 文章：doc 是受限文档结构的 JSON，body_html 是服务端渲染结果。
-- seq 同时是订阅未读游标（只增不减）。
CREATE TABLE IF NOT EXISTS articles (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    id           TEXT NOT NULL UNIQUE,
    publisher_id TEXT NOT NULL REFERENCES publishers(id) ON DELETE CASCADE,
    author       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title        TEXT NOT NULL DEFAULT '',
    doc          TEXT NOT NULL DEFAULT '{}',
    body_text    TEXT NOT NULL DEFAULT '',
    body_html    TEXT NOT NULL DEFAULT '',
    state        TEXT NOT NULL DEFAULT 'draft',
    version      INTEGER NOT NULL DEFAULT 1,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    published_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_articles_pub
    ON articles(publisher_id, seq);
CREATE INDEX IF NOT EXISTS idx_articles_state
    ON articles(state, seq);

CREATE TABLE IF NOT EXISTS article_versions (
    article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    version    INTEGER NOT NULL,
    doc        TEXT NOT NULL,
    saved_at   INTEGER NOT NULL,
    PRIMARY KEY (article_id, version)
);

-- 订阅：read_cursor = 已读到的文章 seq。未读 = 比它大的已发布文章。
CREATE TABLE IF NOT EXISTS publisher_follows (
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    publisher_id TEXT NOT NULL REFERENCES publishers(id) ON DELETE CASCADE,
    read_cursor  INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (user_id, publisher_id)
);
CREATE TABLE IF NOT EXISTS article_reads (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    read_at    INTEGER NOT NULL,
    PRIMARY KEY (user_id, article_id)
);
-- 在看/分享/收藏是三个独立动作，各自一行。
CREATE TABLE IF NOT EXISTS article_reactions (
    article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (article_id, user_id, kind)
);

-- ---------------------------------------------------------------------------
-- G05-03 笔记：version 是乐观锁；写入必须带 expected_version。
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS notes (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title      TEXT NOT NULL DEFAULT '',
    body       TEXT NOT NULL DEFAULT '',
    version    INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_notes_user ON notes(user_id, updated_at);
CREATE TABLE IF NOT EXISTS note_versions (
    note_id  TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    version  INTEGER NOT NULL,
    title    TEXT NOT NULL,
    body     TEXT NOT NULL,
    saved_at INTEGER NOT NULL,
    PRIMARY KEY (note_id, version)
);

-- 既有 favorites 表补列（对象收藏化）：
-- ALTER TABLE favorites ADD COLUMN source_kind    TEXT NOT NULL DEFAULT 'message';
-- ALTER TABLE favorites ADD COLUMN source_id      TEXT NOT NULL DEFAULT '';
-- ALTER TABLE favorites ADD COLUMN snapshot_label TEXT NOT NULL DEFAULT '';
-- ALTER TABLE favorites ADD COLUMN sort_order     INTEGER NOT NULL DEFAULT 0;
-- ALTER TABLE favorites ADD COLUMN revoked        INTEGER NOT NULL DEFAULT 0;
-- 部分唯一索引（对象收藏幂等）必须在补列之后建：
CREATE UNIQUE INDEX IF NOT EXISTS idx_favorites_object
    ON favorites(user_id, source_kind, source_id) WHERE source_id <> '';

-- 表情包：包级许可字段用于"版权"约束；安装是每用户独立行。
CREATE TABLE IF NOT EXISTS sticker_packs (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL DEFAULT '',
    name       TEXT NOT NULL,
    license    TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS stickers (
    id         TEXT PRIMARY KEY,
    pack_id    TEXT NOT NULL REFERENCES sticker_packs(id) ON DELETE CASCADE,
    media_id   TEXT NOT NULL,
    name       TEXT NOT NULL DEFAULT '',
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_stickers_pack ON stickers(pack_id, sort_order);
CREATE TABLE IF NOT EXISTS user_stickers (
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    sticker_id   TEXT NOT NULL REFERENCES stickers(id) ON DELETE CASCADE,
    sort_order   INTEGER NOT NULL DEFAULT 0,
    installed_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, sticker_id)
);

-- ---------------------------------------------------------------------------
-- G05-04 视频号：发布记录只是引用；能否播以 video_refs 的
-- state='succeeded' 为准（转码门禁）。
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS video_posts (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    author     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    media_id   TEXT NOT NULL,
    caption    TEXT NOT NULL DEFAULT '',
    state      TEXT NOT NULL DEFAULT 'published',
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_video_posts_author
    ON video_posts(author, seq);
CREATE INDEX IF NOT EXISTS idx_video_posts_state
    ON video_posts(state, seq);
CREATE TABLE IF NOT EXISTS video_likes (
    video_id   TEXT NOT NULL REFERENCES video_posts(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (video_id, user_id)
);
CREATE TABLE IF NOT EXISTS video_comments (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    video_id   TEXT NOT NULL REFERENCES video_posts(id) ON DELETE CASCADE,
    author     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    text       TEXT NOT NULL,
    deleted    INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_video_comments
    ON video_comments(video_id, seq);
CREATE TABLE IF NOT EXISTS video_follows (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    creator    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, creator)
);

-- ---------------------------------------------------------------------------
-- G05-05 公开内容索引：只放可被"公开检索"投影的字段。
-- 写入与内容发布同事务、删除同步，所以不存在异步落后窗口。
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS content_index (
    doc_id     TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    owner      TEXT NOT NULL,
    audience   TEXT NOT NULL DEFAULT 'public',
    group_name TEXT NOT NULL DEFAULT '',
    title      TEXT NOT NULL DEFAULT '',
    body       TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_content_index_recent
    ON content_index(created_at DESC, doc_id DESC);
CREATE INDEX IF NOT EXISTS idx_content_index_kind
    ON content_index(kind, created_at DESC);

-- 通知收件箱：read_at 在服务端，跨端共享。"事务事件生成"由调用方
-- 与业务写同事务插入；dedupe_key 保证重试/重复点赞不刷屏。
CREATE TABLE IF NOT EXISTS notifications (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    actor       TEXT NOT NULL DEFAULT '',
    target_kind TEXT NOT NULL DEFAULT '',
    target_id   TEXT NOT NULL DEFAULT '',
    text        TEXT NOT NULL DEFAULT '',
    dedupe_key  TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL,
    read_at     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_notifications_user
    ON notifications(user_id, seq DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_dedupe
    ON notifications(user_id, dedupe_key) WHERE dedupe_key <> '';

-- ---------------------------------------------------------------------------
-- G05-06 状态：到期由服务端 now 判定；cleared_at 保留审计/事件痕迹。
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS statuses (
    id                  TEXT PRIMARY KEY,
    owner               TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind                TEXT NOT NULL,
    text                TEXT NOT NULL DEFAULT '',
    media_id            TEXT NOT NULL DEFAULT '',
    background_media_id TEXT NOT NULL DEFAULT '',
    audience            TEXT NOT NULL DEFAULT 'friends',
    version             INTEGER NOT NULL DEFAULT 1,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    expires_at          INTEGER NOT NULL,
    cleared_at          INTEGER
);
CREATE INDEX IF NOT EXISTS idx_statuses_owner
    ON statuses(owner, updated_at DESC);
CREATE TABLE IF NOT EXISTS status_background_drafts (
    owner      TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    media_id   TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS status_reactions (
    status_id  TEXT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (status_id, user_id)
);
