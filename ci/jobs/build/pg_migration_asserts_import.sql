-- M023 shape and behavior assertions (CI PostgreSQL 19.0).
-- Run after 0001_import_control.sql has been applied twice.

DO $$
DECLARE
    table_count INTEGER;
BEGIN
    SELECT count(*) INTO table_count
    FROM information_schema.tables
    WHERE table_schema = 'public'
      AND table_name IN ('import_run', 'import_table');
    IF table_count <> 2 THEN
        RAISE EXCEPTION 'M023: expected import_run + import_table, got %', table_count;
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'import_run'
          AND column_name = 'source_sha256' AND data_type = 'text'
    ) THEN
        RAISE EXCEPTION 'M023: import_run.source_sha256 is not text';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'import_table'
          AND column_name = 'source_digest' AND data_type = 'text'
    ) THEN
        RAISE EXCEPTION 'M023: import_table.source_digest is not text';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
        WHERE schemaname = 'public' AND indexname = 'idx_import_table_status'
    ) THEN
        RAISE EXCEPTION 'M023: idx_import_table_status is missing';
    END IF;

    INSERT INTO import_run (
        run_id, source_path, source_sha256, source_data_version,
        started_at, status, quiescent, notes
    ) VALUES (
        'ci-m023', '/tmp/ci.db', repeat('0', 64), 1,
        1, 'running', FALSE, 'ci'
    );

    INSERT INTO import_table (
        run_id, source_table, target_table, source_rows, loaded_rows, status
    ) VALUES (
        'ci-m023', 'items', 'items', 2, 2, 'loaded'
    );

    UPDATE import_table SET status = 'verified', target_digest = repeat('1', 64)
    WHERE run_id = 'ci-m023' AND source_table = 'items';

    BEGIN
        UPDATE import_table SET status = 'bogus'
        WHERE run_id = 'ci-m023' AND source_table = 'items';
        RAISE EXCEPTION 'M023: bogus status was accepted';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    DELETE FROM import_run WHERE run_id = 'ci-m023';
    IF EXISTS (SELECT 1 FROM import_table WHERE run_id = 'ci-m023') THEN
        RAISE EXCEPTION 'M023: ON DELETE CASCADE did not remove import_table';
    END IF;
END $$;

\echo 'M023 import control: OK'
