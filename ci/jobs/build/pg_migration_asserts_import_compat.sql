-- M024 assertions: the 110-table SQLite snapshot must have a PG landing
-- shape for channel_role, friend_tag, group_governance_audit, relations_audit.
\set ON_ERROR_STOP on

DO $$
BEGIN
    IF (
        SELECT data_type FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'channel_role'
          AND column_name = 'role'
    ) <> 'text' THEN
        RAISE EXCEPTION 'channel_role.role must be text';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'channel_role'
          AND column_name = 'granted_by' AND is_nullable = 'NO'
    ) THEN
        RAISE EXCEPTION 'channel_role.granted_by is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'channel_role'
          AND column_name = 'created_at' AND is_nullable = 'NO'
    ) THEN
        RAISE EXCEPTION 'channel_role.created_at is missing';
    END IF;

    IF to_regclass('public.friend_tag') IS NULL THEN
        RAISE EXCEPTION 'friend_tag is missing';
    END IF;

    IF to_regclass('public.group_governance_audit') IS NULL THEN
        RAISE EXCEPTION 'group_governance_audit is missing';
    END IF;

    IF to_regclass('public.relations_audit') IS NULL THEN
        RAISE EXCEPTION 'relations_audit is missing';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_attribute
        WHERE attrelid = 'group_governance_audit'::regclass
          AND attname = 'seq' AND attidentity = 'd'
    ) THEN
        RAISE EXCEPTION 'group_governance_audit.seq must be an identity column';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_attribute
        WHERE attrelid = 'relations_audit'::regclass
          AND attname = 'seq' AND attidentity = 'd'
    ) THEN
        RAISE EXCEPTION 'relations_audit.seq must be an identity column';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_indexes
        WHERE schemaname = 'public' AND tablename = 'channel_role'
          AND indexname = 'idx_channel_role_user'
    ) THEN
        RAISE EXCEPTION 'idx_channel_role_user is missing';
    END IF;
END $$;

INSERT INTO channel_role (channel_id, user_id, role, granted_by, created_at)
VALUES ('ci-channel', 'ci-user', 'moderator', 'ci-admin', 1);

INSERT INTO friend_tag (user_id, friend_id, tag, created_at)
VALUES ('ci-user', 'ci-friend', 'ci-tag', 1);

INSERT INTO group_governance_audit (group_id, action, actor, reason, trace, at)
VALUES ('ci-group', 'ci-action', 'ci-actor', 'ci-reason', '', 1)
RETURNING seq;

INSERT INTO relations_audit (actor, subject, kind, at)
VALUES ('ci-actor', 'ci-subject', 'ci-kind', 1)
RETURNING seq;

TRUNCATE channel_role, friend_tag, group_governance_audit, relations_audit;
