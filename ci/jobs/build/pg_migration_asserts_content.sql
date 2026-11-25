-- M014 断言：内容/工单三张表与索引存在；只读，不改数据。
\set ON_ERROR_STOP on
DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name) INTO missing
    FROM (VALUES
        ('reports'), ('content_index'), ('content_media_refs'),
        ('idx_reports_reporter'), ('idx_reports_state'),
        ('idx_content_index_recent'), ('idx_content_index_kind'),
        ('idx_cmr_media')
    ) AS wanted(name)
    WHERE to_regclass('public.' || wanted.name) IS NULL;
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M014: 缺对象: %', missing;
    END IF;
END $$;

INSERT INTO reports(id, reporter, target_kind, target_id, reason, detail, state,
                    action_json, handler, created_at, updated_at, history_json)
VALUES ('m014_assert_report', 'm014_assert', 'user', 'm014_target', 'spam', '', 'open',
        NULL, NULL, 0, 0, '[]')
ON CONFLICT (id) DO NOTHING;
INSERT INTO content_index(doc_id, kind, owner, created_at, updated_at)
VALUES ('m014_assert_doc', 'moment', 'm014_assert', 0, 0)
ON CONFLICT (doc_id) DO NOTHING;
INSERT INTO content_media_refs(media_id, ref_kind, ref_id, owner, created_at)
VALUES ('m014_assert_media', 'moment', 'm014_assert_doc', 'm014_assert', 0)
ON CONFLICT (ref_kind, ref_id, media_id) DO NOTHING;
