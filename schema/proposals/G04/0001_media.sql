-- G04 media domain proposal. 编号由 G00 赋予：**M009**（`schema/registry/migrations.json:20`，
-- next_id 已推进到 M010）。DDL 只有一份，就在这里；执行点是 `MediaDb::open`
-- （`CREATE TABLE IF NOT EXISTS`，幂等可重入），由 server 组合根在平台同一个 SQLite
-- 文件上调用（`server/src/media_mount.rs::assemble`，issue #295）。
-- Rollback compatibility: every table is created with IF NOT EXISTS and no
-- data backfill is required, so dropping this migration is equivalent to
-- dropping the media_* tables; existing messaging/content tables are not
-- touched and no NOT NULL is added to shared tables.
-- One row per content-addressed object. `digest` never grants read access:
-- authorization is owner + active media_ref audiences + short-lived signed URLs.
CREATE TABLE IF NOT EXISTS media_object (
    object_id    TEXT PRIMARY KEY,
    owner        TEXT NOT NULL,
    size         INTEGER NOT NULL CHECK (size >= 0),
    digest       TEXT NOT NULL,
    mime         TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('ready', 'deleted')),
    access_epoch INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    deleted_at   INTEGER
);
CREATE INDEX IF NOT EXISTS idx_media_object_owner
    ON media_object(owner, created_at DESC);

-- A logical reference is the only thing that keeps an object alive. Deleting a
-- message/file removes its ref; physical GC runs only when no active ref,
-- non-trashed file row or active share points at the object.
CREATE TABLE IF NOT EXISTS media_ref (
    ref_id     TEXT PRIMARY KEY,
    object_id  TEXT NOT NULL REFERENCES media_object(object_id),
    ref_type   TEXT NOT NULL,
    ref_id_ext TEXT NOT NULL,
    owner      TEXT NOT NULL,
    audience   TEXT NOT NULL,
    purpose    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_media_ref_object ON media_ref(object_id, deleted_at);
CREATE UNIQUE INDEX IF NOT EXISTS uq_media_ref_logical
    ON media_ref(ref_type, ref_id_ext, object_id, audience)
    WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS media_upload (
    upload_id       TEXT PRIMARY KEY,
    owner           TEXT NOT NULL,
    purpose         TEXT NOT NULL,
    name            TEXT NOT NULL,
    mime            TEXT NOT NULL,
    declared_size   INTEGER NOT NULL CHECK (declared_size >= 0),
    declared_digest TEXT NOT NULL DEFAULT '',
    offset          INTEGER NOT NULL DEFAULT 0,
    chunk_size      INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('open', 'completed', 'canceled', 'expired')),
    object_id       TEXT,
    expires_at      INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_upload_owner ON media_upload(owner, status);
CREATE INDEX IF NOT EXISTS idx_media_upload_expiry ON media_upload(status, expires_at);

CREATE TABLE IF NOT EXISTS media_upload_chunk (
    upload_id  TEXT NOT NULL REFERENCES media_upload(upload_id) ON DELETE CASCADE,
    chunk_off  INTEGER NOT NULL,
    len        INTEGER NOT NULL,
    digest     TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    PRIMARY KEY (upload_id, chunk_off)
);

CREATE TABLE IF NOT EXISTS media_file (
    file_id    TEXT PRIMARY KEY,
    object_id  TEXT NOT NULL REFERENCES media_object(object_id),
    owner      TEXT NOT NULL,
    name       TEXT NOT NULL,
    folder     TEXT NOT NULL DEFAULT '',
    category   TEXT NOT NULL,
    size       INTEGER NOT NULL,
    trashed_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_file_owner
    ON media_file(owner, trashed_at, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_media_file_object ON media_file(object_id, trashed_at);

CREATE TABLE IF NOT EXISTS media_share (
    share_id   TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    object_id  TEXT NOT NULL REFERENCES media_object(object_id),
    owner      TEXT NOT NULL,
    audience   TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_share_object ON media_share(object_id, revoked_at);

CREATE TABLE IF NOT EXISTS media_job (
    job_id        TEXT PRIMARY KEY,
    owner         TEXT NOT NULL,
    object_id     TEXT NOT NULL REFERENCES media_object(object_id),
    preset        TEXT NOT NULL,
    state         TEXT NOT NULL CHECK (state IN ('pending', 'running', 'succeeded', 'failed', 'canceled')),
    attempts      INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER NOT NULL,
    budget        TEXT NOT NULL,
    outputs       TEXT NOT NULL DEFAULT '[]',
    error         TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    UNIQUE (object_id, preset)
);
CREATE INDEX IF NOT EXISTS idx_media_job_due ON media_job(state, next_retry_at);
CREATE INDEX IF NOT EXISTS idx_media_job_owner ON media_job(owner, created_at DESC);

CREATE TABLE IF NOT EXISTS media_backup (
    backup_id      TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    state          TEXT NOT NULL,
    manifest       TEXT NOT NULL,
    package_path   TEXT NOT NULL,
    package_digest TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_backup_owner ON media_backup(owner, created_at DESC);

CREATE TABLE IF NOT EXISTS media_backup_entry (
    backup_id  TEXT NOT NULL REFERENCES media_backup(backup_id) ON DELETE CASCADE,
    entry_idx  INTEGER NOT NULL,
    object_id  TEXT NOT NULL,
    digest     TEXT NOT NULL,
    size       INTEGER NOT NULL,
    imported   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (backup_id, entry_idx)
);
