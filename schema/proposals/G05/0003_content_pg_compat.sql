-- M014 — 平台内容/工单 PG 兼容层（PostgreSQL）
--
-- 目的：给 G00-04 的旧 repo 分批切 PG 提供第一批真实表。`reports` 是举报工单，
-- `content_index` / `content_media_refs` 是 G05 公开内容索引与媒体引用。三张表
-- 在 SQLite 侧是 M001/M005/M008 建的，这里按同名、同列序落 PG，旧 SQL（列名
-- 与 `?N` 占位符）不改即可运行。
--
-- expand-only：只建表和索引，不改既有对象。全部 IF NOT EXISTS，可重入。
-- 回滚：DROP TABLE content_media_refs / content_index / reports（只回收本迁移
-- 新建的投影，不影响 identity/media/ledger 表）。

BEGIN;

CREATE TABLE IF NOT EXISTS reports (
    id           TEXT PRIMARY KEY,
    reporter     TEXT NOT NULL,
    target_kind  TEXT NOT NULL,
    target_id    TEXT NOT NULL,
    reason       TEXT NOT NULL,
    detail       TEXT NOT NULL,
    state        TEXT NOT NULL,
    action_json  TEXT,
    handler      TEXT,
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL,
    history_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_reports_reporter ON reports(reporter, created_at);
CREATE INDEX IF NOT EXISTS idx_reports_state ON reports(state, created_at);

CREATE TABLE IF NOT EXISTS content_media_refs (
    media_id   TEXT NOT NULL,
    ref_kind   TEXT NOT NULL,
    ref_id     TEXT NOT NULL,
    owner      TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (ref_kind, ref_id, media_id)
);

CREATE INDEX IF NOT EXISTS idx_cmr_media ON content_media_refs(media_id);

CREATE TABLE IF NOT EXISTS content_index (
    doc_id     TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    owner      TEXT NOT NULL,
    audience   TEXT NOT NULL DEFAULT 'public',
    group_name TEXT NOT NULL DEFAULT '',
    title      TEXT NOT NULL DEFAULT '',
    body       TEXT NOT NULL DEFAULT '',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_content_index_recent
    ON content_index(created_at DESC, doc_id DESC);
CREATE INDEX IF NOT EXISTS idx_content_index_kind
    ON content_index(kind, created_at DESC);

COMMIT;
