-- M023 — PostgreSQL cutover import control (G00-04 S4)
--
-- The importer needs a resumable ledger outside the imported tables:
--   * one `import_run` row pins the source file, its SHA-256 and the SQLite
--     `data_version` observed at planning time;
--   * one `import_table` row per source table records load/verify state and the
--     multiset digest both sides agreed on.
--
-- This is expand-only. Nothing here is read by the product runtime; it is the
-- cutover tool's own audit trail. Rollback: DROP the two tables.

BEGIN;

CREATE TABLE IF NOT EXISTS import_run (
    run_id              TEXT PRIMARY KEY,
    source_path         TEXT NOT NULL,
    source_sha256       TEXT NOT NULL,
    source_data_version BIGINT NOT NULL DEFAULT 0,
    started_at          BIGINT NOT NULL,
    finished_at         BIGINT,
    status              TEXT NOT NULL DEFAULT 'running'
                        CHECK (status IN ('running', 'verified', 'failed', 'aborted')),
    quiescent           BOOLEAN NOT NULL DEFAULT FALSE,
    window_ms           BIGINT,
    notes               TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS import_table (
    run_id        TEXT NOT NULL REFERENCES import_run(run_id) ON DELETE CASCADE,
    source_table  TEXT NOT NULL,
    target_table  TEXT NOT NULL,
    source_rows   BIGINT NOT NULL DEFAULT -1,
    loaded_rows   BIGINT NOT NULL DEFAULT -1,
    source_digest TEXT NOT NULL DEFAULT '',
    target_digest TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'loading', 'loaded', 'verified', 'failed', 'skipped')),
    started_at    BIGINT NOT NULL DEFAULT 0,
    finished_at   BIGINT NOT NULL DEFAULT 0,
    error         TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (run_id, source_table)
);

CREATE INDEX IF NOT EXISTS idx_import_table_status
    ON import_table(status);
CREATE INDEX IF NOT EXISTS idx_import_table_target
    ON import_table(target_table);

COMMIT;
