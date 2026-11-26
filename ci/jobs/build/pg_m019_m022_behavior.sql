-- M019-M022 行为冒烟：默认值、生成列、复合主键与唯一约束。
-- 只写 `m019_smoke`/`m020_smoke`/`m021_smoke`/`m022_smoke` 前缀的 fixture，
-- 最后全部删除；可重复执行。
\set ON_ERROR_STOP on

DO $$
DECLARE
    seq_value BIGINT;
    generated_id BIGINT;
    mode_want TEXT;
    epoch_value BIGINT;
BEGIN
    -- owner_seq：默认 0，UPDATE ... RETURNING 递增值。
    INSERT INTO owner_seq(owner_type, owner_id, seq)
    VALUES ('user', 'm019_smoke', DEFAULT)
    ON CONFLICT (owner_type, owner_id) DO UPDATE SET seq = owner_seq.seq + 1
    RETURNING seq INTO seq_value;
    IF seq_value <> 0 AND seq_value <> 1 THEN
        RAISE EXCEPTION 'M019: owner_seq 默认/递增异常: %', seq_value;
    END IF;

    -- mailbox：同一 msg_id 不能出现在两个信箱行。
    INSERT INTO mailbox(owner_type, owner_id, seq, msg_id, sender, type_id, sent_at_ms)
    VALUES ('user', 'm019_smoke', 1, 'm019_smoke_msg', 'u1', 'text', 1)
    ON CONFLICT (owner_type, owner_id, seq) DO NOTHING;
    BEGIN
        INSERT INTO mailbox(owner_type, owner_id, seq, msg_id, sender, type_id, sent_at_ms)
        VALUES ('user', 'm019_smoke_2', 1, 'm019_smoke_msg', 'u1', 'text', 2);
        RAISE EXCEPTION 'M019: mailbox.msg_id 唯一索引未生效';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- read_cursor：未读数默认从 0 开始。
    INSERT INTO read_cursor(owner_type, owner_id, user_id)
    VALUES ('user', 'm019_smoke', 'u1')
    ON CONFLICT (owner_type, owner_id, user_id) DO NOTHING;
    IF (SELECT last_ack_seq FROM read_cursor
        WHERE owner_type = 'user' AND owner_id = 'm019_smoke' AND user_id = 'u1') <> 0 THEN
        RAISE EXCEPTION 'M019: read_cursor.last_ack_seq 默认值不是 0';
    END IF;

    -- ephemeral：生成列必须给出正整数 id。
    INSERT INTO ephemeral(owner_type, owner_id, msg_id, type_id, sent_at_ms)
    VALUES ('user', 'm019_smoke', 'm019_smoke_eph', 'card', 1)
    RETURNING rowid_hint INTO generated_id;
    IF generated_id <= 0 THEN
        RAISE EXCEPTION 'M019: ephemeral 生成 id 异常: %', generated_id;
    END IF;

    -- message_submit：幂等键是复合主键，msg_id 另有唯一约束。
    INSERT INTO message_submit(sender_device, idempotency_key, conv_id, msg_id, seq, created_at)
    VALUES ('m019_smoke_dev', 'k1', 'c1', 'm019_smoke_m1', 1, 1)
    ON CONFLICT (sender_device, idempotency_key) DO NOTHING;
    BEGIN
        INSERT INTO message_submit(sender_device, idempotency_key, conv_id, msg_id, seq, created_at)
        VALUES ('m019_smoke_dev', 'k2', 'c1', 'm019_smoke_m1', 2, 2);
        RAISE EXCEPTION 'M020: message_submit.msg_id 唯一约束未生效';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- delivery_task：同一 msg/用户/设备只能有一行。
    INSERT INTO delivery_task(msg_id, target_user, target_device, created_at, updated_at)
    VALUES ('m019_smoke_m1', 'u1', '*', 1, 1)
    ON CONFLICT (msg_id, target_user, target_device) DO NOTHING;

    -- conversation_projection / conversation_pref：主键可写且默认值正确。
    INSERT INTO conversation_projection(conv_id, owner_type)
    VALUES ('m020_smoke_conv', 'user')
    ON CONFLICT (conv_id) DO NOTHING;
    INSERT INTO conversation_pref(user_id, conv_id)
    VALUES ('m021_smoke', 'm020_smoke_conv')
    ON CONFLICT (user_id, conv_id) DO NOTHING;
    IF (SELECT pinned FROM conversation_pref
        WHERE user_id = 'm021_smoke' AND conv_id = 'm020_smoke_conv') <> 0 THEN
        RAISE EXCEPTION 'M020: conversation_pref.pinned 默认值不是 0';
    END IF;

    -- push_device：epoch 默认 1，撤销默认 false。
    INSERT INTO push_device(device_id, user_id, platform, token, created_ms, last_seen_ms)
    VALUES ('m020_smoke_dev', 'm019_smoke', 'ios', 'tok', 1, 1)
    ON CONFLICT (device_id) DO NOTHING;
    SELECT epoch INTO epoch_value FROM push_device WHERE device_id = 'm020_smoke_dev';
    IF epoch_value <> 1 THEN
        RAISE EXCEPTION 'M020: push_device.epoch 默认值不是 1: %', epoch_value;
    END IF;

    -- friend_acs：空串是 UNSET，不是 NULL。
    INSERT INTO friend_acs(user_id, peer_id, updated_at)
    VALUES ('m021_smoke', 'm021_smoke_peer', 1)
    ON CONFLICT (user_id, peer_id) DO NOTHING;
    SELECT mode_want INTO mode_want FROM friend_acs
    WHERE user_id = 'm021_smoke' AND peer_id = 'm021_smoke_peer';
    IF mode_want IS NULL OR mode_want <> '' THEN
        RAISE EXCEPTION 'M021: friend_acs.mode_want 默认值不是空串';
    END IF;

    -- identity_token_serial：单行表的 CHECK 必须拒绝 id<>1。
    INSERT INTO identity_token_serial(id, serial, rotated_at)
    VALUES (1, 1, 1)
    ON CONFLICT (id) DO NOTHING;
    BEGIN
        INSERT INTO identity_token_serial(id, serial, rotated_at)
        VALUES (2, 2, 2);
        RAISE EXCEPTION 'M022: identity_token_serial 的 id=1 CHECK 未生效';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
END $$;

DELETE FROM mailbox WHERE msg_id = 'm019_smoke_msg';
DELETE FROM read_cursor WHERE owner_type = 'user' AND owner_id = 'm019_smoke';
DELETE FROM ephemeral WHERE msg_id = 'm019_smoke_eph';
DELETE FROM message_submit WHERE sender_device = 'm019_smoke_dev';
DELETE FROM delivery_task WHERE msg_id = 'm019_smoke_m1';
DELETE FROM conversation_pref WHERE user_id = 'm021_smoke';
DELETE FROM conversation_projection WHERE conv_id = 'm020_smoke_conv';
DELETE FROM push_device WHERE device_id = 'm020_smoke_dev';
DELETE FROM friend_acs WHERE user_id = 'm021_smoke';
DELETE FROM identity_token_serial WHERE id = 1;
DELETE FROM owner_seq WHERE owner_type = 'user' AND owner_id = 'm019_smoke';
