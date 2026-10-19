import re

# ---------- ③' 扇出携带信箱序列号 ----------
p = 'crates/net/src/mq_hub.rs'
s = open(p, encoding='utf-8').read()

s = s.replace('''    let env = if let Some(policy) = &state.policy {''',
              '''    let mut env = if let Some(policy) = &state.policy {''')

old = '''        // 镜像进信箱（读扩散的唯一真相）。`d-` 前缀 = 直聊，其余按群处理；
        // 频道的消息不走这里（由 platform::send_channel_message 直接写信箱）。
        if let Some(mailbox) = &state.mailbox {
            if let Some((type_id, payload)) = wxlike_message::mirror_of(&env) {
                let owner = if env.conv_id.starts_with("d-") {
                    wxlike_message::OwnerType::Direct
                } else {
                    wxlike_message::OwnerType::Group
                };
                let msg_id = hex::encode(env.msg_id);
                if let Err(err) = mailbox.append(
                    (owner, &env.conv_id),
                    &env.sender,
                    &type_id,
                    &msg_id,
                    &payload,
                ) {
                    // 镜像失败不该阻断投递：归档已成功，用户仍能收到消息
                    eprintln!("[mq-hub] mailbox mirror failed: {err}");
                }
            }
        }
    }'''
new = '''        // 镜像进信箱（读扩散的唯一真相）。`d-` 前缀 = 直聊，其余按群处理；
        // 频道的消息不走这里（由 platform::send_channel_message 直接写信箱）。
        //
        // 顺带把**信箱序列号**写回 envelope：扇出到每个收件人的帧都带同一个
        // 会话内序号（1,2,3…），客户端因此有了服务端权威的排序，与未读、
        // ack、sync_plan 用的是同一套编号。之前 seq 是发送方填的（通常 0），
        // 客户端只能按到达顺序排——多端之间会不一致。
        if let Some(mailbox) = &state.mailbox {
            if let Some((type_id, payload)) = wxlike_message::mirror_of(&env) {
                let owner = if env.conv_id.starts_with("d-") {
                    wxlike_message::OwnerType::Direct
                } else {
                    wxlike_message::OwnerType::Group
                };
                let msg_id = hex::encode(env.msg_id);
                match mailbox.append(
                    (owner, &env.conv_id),
                    &env.sender,
                    &type_id,
                    &msg_id,
                    &payload,
                ) {
                    Ok(Some(seq)) => env.seq = seq,
                    // 重复 msg_id：信箱已有这一条，沿用它的序号
                    Ok(None) => {
                        if let Ok(seq) = mailbox.head_seq((owner, &env.conv_id)) {
                            env.seq = seq;
                        }
                    }
                    // 镜像失败不该阻断投递：归档已成功，用户仍能收到消息
                    Err(err) => eprintln!("[mq-hub] mailbox mirror failed: {err}"),
                }
            }
        }
    }'''
assert old in s, 'mirror block not found'
s = s.replace(old, new, 1)
open(p, 'w', encoding='utf-8').write(s)

# ---------- ④ 删掉 device_cursor / sync_since（信箱才是同步权威） ----------
p = 'crates/core/src/store.rs'
s = open(p, encoding='utf-8').read()

# 表
s = s.replace('''            CREATE TABLE IF NOT EXISTS device_cursor (
                device_id  TEXT NOT NULL,
                conv_id    TEXT NOT NULL,
                last_seq   INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (device_id, conv_id)
            );
''', '')

# 模块注释里的说明
s = s.replace('''//! * **multi-device sync** — `device_cursor` remembers how far each device has
//!   seen each conversation, so `sync_since` returns only the delta.''',
'''//! * ~~multi-device sync~~ — 已移除：多端同步改由 `wxlike_message` 的信箱
//!   （`head_seq` / `ack` / `changes` / `sync_plan`）承担，本 crate 不再保存
//!   任何"设备看到哪了"的状态，避免两套游标。''')

# 方法：sync_since / device_cursor / set_device_cursor
start = s.index('    /// Messages in `conv_id` this device has not received yet.')
end = s.index('    pub fn page_messages(')
removed = s[start:end]
assert 'sync_since' in removed and 'set_device_cursor' in removed
s = s[:start] + s[end:]
open(p, 'w', encoding='utf-8').write(s)
print('ok')
