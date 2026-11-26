-- M019-M022 断言：20 张表、131 列、8 个索引存在，列类型与 SQLite 形状一致。
-- 只读系统目录，不写业务数据；可重复执行。
\set ON_ERROR_STOP on

DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(format('%s.%s(%s)', e.tbl, e.col, e.typ), ', ' ORDER BY e.tbl, e.col)
    INTO missing
    FROM (VALUES
        ('owner_seq', 'owner_type', 'TEXT'),
        ('owner_seq', 'owner_id', 'TEXT'),
        ('owner_seq', 'seq', 'BIGINT'),
        ('mailbox', 'owner_type', 'TEXT'),
        ('mailbox', 'owner_id', 'TEXT'),
        ('mailbox', 'seq', 'BIGINT'),
        ('mailbox', 'msg_id', 'TEXT'),
        ('mailbox', 'sender', 'TEXT'),
        ('mailbox', 'type_id', 'TEXT'),
        ('mailbox', 'payload', 'TEXT'),
        ('mailbox', 'sent_at_ms', 'BIGINT'),
        ('mailbox', 'withdrawn', 'BIGINT'),
        ('mailbox', 'sender_device', 'TEXT'),
        ('mailbox', 'body_kind', 'TEXT'),
        ('mailbox', 'edit_version', 'BIGINT'),
        ('mailbox', 'recalled_at_ms', 'BIGINT'),
        ('mailbox', 'forward_of', 'TEXT'),
        ('read_cursor', 'owner_type', 'TEXT'),
        ('read_cursor', 'owner_id', 'TEXT'),
        ('read_cursor', 'user_id', 'TEXT'),
        ('read_cursor', 'last_ack_seq', 'BIGINT'),
        ('read_cursor', 'updated_at', 'BIGINT'),
        ('msg_user_state', 'owner_type', 'TEXT'),
        ('msg_user_state', 'owner_id', 'TEXT'),
        ('msg_user_state', 'seq', 'BIGINT'),
        ('msg_user_state', 'user_id', 'TEXT'),
        ('msg_user_state', 'state_type', 'TEXT'),
        ('mailbox_change', 'owner_type', 'TEXT'),
        ('mailbox_change', 'owner_id', 'TEXT'),
        ('mailbox_change', 'version', 'BIGINT'),
        ('mailbox_change', 'msg_id', 'TEXT'),
        ('mailbox_change', 'kind', 'TEXT'),
        ('mailbox_change', 'actor', 'TEXT'),
        ('mailbox_change', 'at_ms', 'BIGINT'),
        ('ephemeral', 'rowid_hint', 'BIGINT'),
        ('ephemeral', 'owner_type', 'TEXT'),
        ('ephemeral', 'owner_id', 'TEXT'),
        ('ephemeral', 'msg_id', 'TEXT'),
        ('ephemeral', 'target_msg_id', 'TEXT'),
        ('ephemeral', 'type_id', 'TEXT'),
        ('ephemeral', 'payload', 'TEXT'),
        ('ephemeral', 'sent_at_ms', 'BIGINT'),
        ('member_log', 'owner_type', 'TEXT'),
        ('member_log', 'owner_id', 'TEXT'),
        ('member_log', 'version', 'BIGINT'),
        ('member_log', 'kind', 'TEXT'),
        ('member_log', 'user_id', 'TEXT'),
        ('member_log', 'payload', 'TEXT'),
        ('member_log', 'at_ms', 'BIGINT'),
        ('member_snapshot', 'owner_type', 'TEXT'),
        ('member_snapshot', 'owner_id', 'TEXT'),
        ('member_snapshot', 'user_id', 'TEXT'),
        ('message_submit', 'sender_device', 'TEXT'),
        ('message_submit', 'idempotency_key', 'TEXT'),
        ('message_submit', 'conv_id', 'TEXT'),
        ('message_submit', 'msg_id', 'TEXT'),
        ('message_submit', 'seq', 'BIGINT'),
        ('message_submit', 'payload_hash', 'TEXT'),
        ('message_submit', 'created_at', 'BIGINT'),
        ('delivery_task', 'msg_id', 'TEXT'),
        ('delivery_task', 'target_user', 'TEXT'),
        ('delivery_task', 'target_device', 'TEXT'),
        ('delivery_task', 'state', 'TEXT'),
        ('delivery_task', 'lease_owner', 'TEXT'),
        ('delivery_task', 'lease_until', 'BIGINT'),
        ('delivery_task', 'attempts', 'BIGINT'),
        ('delivery_task', 'next_at', 'BIGINT'),
        ('delivery_task', 'joined_seq', 'BIGINT'),
        ('delivery_task', 'left_seq', 'BIGINT'),
        ('delivery_task', 'created_at', 'BIGINT'),
        ('delivery_task', 'updated_at', 'BIGINT'),
        ('device_cursor', 'device_id', 'TEXT'),
        ('device_cursor', 'conv_id', 'TEXT'),
        ('device_cursor', 'stored_cursor', 'BIGINT'),
        ('device_cursor', 'read_cursor', 'BIGINT'),
        ('device_cursor', 'version', 'BIGINT'),
        ('device_cursor', 'updated_at', 'BIGINT'),
        ('conversation_projection', 'conv_id', 'TEXT'),
        ('conversation_projection', 'owner_type', 'TEXT'),
        ('conversation_projection', 'last_seq', 'BIGINT'),
        ('conversation_projection', 'last_at_ms', 'BIGINT'),
        ('conversation_projection', 'last_sender', 'TEXT'),
        ('conversation_projection', 'last_type', 'TEXT'),
        ('conversation_projection', 'preview', 'TEXT'),
        ('conversation_projection', 'member_count', 'BIGINT'),
        ('conversation_projection', 'deleted', 'BIGINT'),
        ('conversation_projection', 'last_withdrawn', 'BIGINT'),
        ('conversation_projection', 'updated_at', 'BIGINT'),
        ('conversation_pref', 'user_id', 'TEXT'),
        ('conversation_pref', 'conv_id', 'TEXT'),
        ('conversation_pref', 'pinned', 'BIGINT'),
        ('conversation_pref', 'muted', 'BIGINT'),
        ('conversation_pref', 'archived', 'BIGINT'),
        ('conversation_pref', 'hidden', 'BIGINT'),
        ('conversation_pref', 'version', 'BIGINT'),
        ('conversation_pref', 'updated_at', 'BIGINT'),
        ('conversation_clear', 'user_id', 'TEXT'),
        ('conversation_clear', 'conv_id', 'TEXT'),
        ('conversation_clear', 'cleared_before_seq', 'BIGINT'),
        ('conversation_clear', 'updated_at', 'BIGINT'),
        ('conversation_event', 'id', 'BIGINT'),
        ('conversation_event', 'actor', 'TEXT'),
        ('conversation_event', 'conv_id', 'TEXT'),
        ('conversation_event', 'kind', 'TEXT'),
        ('conversation_event', 'payload', 'TEXT'),
        ('conversation_event', 'at_ms', 'BIGINT'),
        ('push_device', 'device_id', 'TEXT'),
        ('push_device', 'user_id', 'TEXT'),
        ('push_device', 'platform', 'TEXT'),
        ('push_device', 'token', 'TEXT'),
        ('push_device', 'epoch', 'BIGINT'),
        ('push_device', 'revoked', 'BIGINT'),
        ('push_device', 'created_ms', 'BIGINT'),
        ('push_device', 'last_seen_ms', 'BIGINT'),
        ('friend_acs', 'user_id', 'TEXT'),
        ('friend_acs', 'peer_id', 'TEXT'),
        ('friend_acs', 'mode_want', 'TEXT'),
        ('friend_acs', 'mode_given', 'TEXT'),
        ('friend_acs', 'updated_at', 'BIGINT'),
        ('friend_policy', 'user_id', 'TEXT'),
        ('friend_policy', 'access_auth', 'TEXT'),
        ('friend_policy', 'access_anon', 'TEXT'),
        ('friend_policy', 'updated_at', 'BIGINT'),
        ('group_member_acs', 'group_id', 'TEXT'),
        ('group_member_acs', 'user_id', 'TEXT'),
        ('group_member_acs', 'mode_want', 'TEXT'),
        ('group_member_acs', 'mode_given', 'TEXT'),
        ('group_member_acs', 'updated_at', 'BIGINT'),
        ('identity_token_serial', 'id', 'BIGINT'),
        ('identity_token_serial', 'serial', 'BIGINT'),
        ('identity_token_serial', 'rotated_at', 'BIGINT')
    ) AS e(tbl, col, typ)
    WHERE NOT EXISTS (
        SELECT 1
        FROM information_schema.columns c
        WHERE c.table_schema = 'public'
          AND c.table_name = e.tbl
          AND c.column_name = e.col
          AND c.data_type = CASE e.typ WHEN 'TEXT' THEN 'text' WHEN 'BIGINT' THEN 'bigint' END
    );
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M019-M022: 缺列或类型不符: %', missing;
    END IF;
END $$;

DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(wanted.name, ', ' ORDER BY wanted.name)
    INTO missing
    FROM (VALUES
        ('idx_mailbox_msgid'),
        ('idx_mailbox_owner'),
        ('idx_ephemeral_owner'),
        ('idx_delivery_ready'),
        ('idx_delivery_device'),
        ('idx_conv_projection_page'),
        ('idx_conversation_event_conv'),
        ('idx_push_user')
    ) AS wanted(name)
    WHERE to_regclass('public.' || wanted.name) IS NULL;
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'M019-M022: 缺索引: %', missing;
    END IF;
END $$;
