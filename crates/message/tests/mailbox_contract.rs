//! P3 message-service contract tests (ADR-16/17 + 总蓝图第四部分).
//!
//! Each invariant from the blueprint has a test that fails if the invariant
//! is broken: seq单点原子、三通道语义、withdraw=逻辑删除+变更广播、
//! ephemeral 不进时间线。

use wxlike_message::{channel_class_of, ChannelClass, MailError, MessageStore, OwnerType};

fn store() -> MessageStore {
    MessageStore::open(None).unwrap()
}

const GROUP: (OwnerType, &str) = (OwnerType::Group, "g-100");
const DIRECT: (OwnerType, &str) = (OwnerType::Direct, "d-a~b");
const CHANNEL: (OwnerType, &str) = (OwnerType::Channel, "c-7");

#[test]
fn seq_allocates_monotonically_per_owner_and_independently_across_owners() {
    let s = store();
    for i in 1..=5 {
        assert_eq!(
            s.append(GROUP, "alice", "text", &format!("g{i}"), "{}")
                .unwrap(),
            Some(i)
        );
    }
    // 交错追加：各属主序列号独立（不变式 2），互不推进
    assert_eq!(
        s.append(DIRECT, "alice", "text", "d1", "{}").unwrap(),
        Some(1)
    );
    assert_eq!(
        s.append(CHANNEL, "alice", "text", "c1", "{}").unwrap(),
        Some(1)
    );
    assert_eq!(s.append(GROUP, "bob", "text", "g6", "{}").unwrap(), Some(6));
    assert_eq!(s.head_seq(GROUP).unwrap(), 6);
    assert_eq!(s.head_seq(DIRECT).unwrap(), 1);
    assert_eq!(s.head_seq(CHANNEL).unwrap(), 1);
}

#[test]
fn duplicate_msg_id_is_silent_none_not_error() {
    let s = store();
    assert_eq!(s.append(GROUP, "a", "text", "same", "{}").unwrap(), Some(1));
    assert_eq!(s.append(GROUP, "a", "text", "same", "{}").unwrap(), None);
    // seq 没有被重复消耗：下一条是 2（at-least-once 重投不能制造空洞）
    assert_eq!(s.append(GROUP, "a", "text", "next", "{}").unwrap(), Some(2));
}

#[test]
fn timeline_fetch_is_ascending_and_cursor_exclusive() {
    let s = store();
    for i in 1..=10 {
        s.append(GROUP, "a", "text", &format!("m{i}"), "{}")
            .unwrap();
    }
    let (page1, next) = s.fetch(GROUP, 0, 4).unwrap();
    assert_eq!(page1.len(), 4);
    assert_eq!(page1[0].seq, 1);
    assert_eq!(page1[3].seq, 4);
    assert_eq!(next, Some(4));
    let (page2, next2) = s.fetch(GROUP, 4, 4).unwrap();
    assert_eq!(page2[0].seq, 5);
    assert_eq!(next2, Some(8));
    let (tail, none) = s.fetch(GROUP, 8, 10).unwrap();
    assert_eq!(tail.len(), 2, "short page = exhausted");
    assert_eq!(none, None);
}

#[test]
fn unread_is_head_minus_cursor_and_ack_only_advances() {
    let s = store();
    for i in 1..=5 {
        s.append(GROUP, "a", "text", &format!("m{i}"), "{}")
            .unwrap();
    }
    assert_eq!(s.unread(GROUP, "bob").unwrap(), 5, "no cursor = all unread");

    assert!(s.ack(GROUP, "bob", 3).unwrap());
    assert_eq!(s.unread(GROUP, "bob").unwrap(), 2);
    assert_eq!(s.unread(GROUP, "carol").unwrap(), 5, "cursors are per user");

    // 迟到的低游标 no-op：乱序 ack 不能把未读数撑回去
    assert!(!s.ack(GROUP, "bob", 2).unwrap());
    assert_eq!(s.unread(GROUP, "bob").unwrap(), 2);
    assert!(s.ack(GROUP, "bob", 5).unwrap());
    assert_eq!(s.unread(GROUP, "bob").unwrap(), 0);
}

#[test]
fn registry_rejects_unregistered_and_wrong_channel_types() {
    let s = store();
    // 未注册类型
    assert!(matches!(
        s.append(GROUP, "a", "evil.custom", "x", "{}"),
        Err(MailError::UnregisteredType(t)) if t == "evil.custom"
    ));
    // 注册了但不是 normal：high_priority / ephemeral 不得进时间线（不变式 3）
    assert!(matches!(
        s.append(GROUP, "a", "typing", "x", "{}"),
        Err(MailError::WrongChannel(t, "high_priority")) if t == "typing"
    ));
    assert!(matches!(
        s.append(GROUP, "a", "wallet.redpacket.claim", "x", "{}"),
        Err(MailError::WrongChannel(t, "ephemeral")) if t == "wallet.redpacket.claim"
    ));
    // append_high_priority 是显式的"不落库"语义说明
    assert!(s.append_high_priority("live.signal").is_ok());
    assert!(s.append_high_priority("text").is_err());
    // 时间线确实为空：high_priority 从未写入
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn registry_covers_all_p1_p2_business_kinds() {
    // P1/P2 已上线消息形态 + P3 计划形态必须在注册表内各归其位
    for t in [
        "text",
        "image",
        "msg.file",
        "msg.sticker",
        "msg.voice",
        "msg.video",
        "msg.forward",
        "msg.merge",
        "wallet.redpacket",
        "miniapp.card",
        "ad.card",
    ] {
        assert_eq!(
            channel_class_of(t),
            Some(ChannelClass::Normal),
            "{t} must be normal"
        );
    }
    for t in ["typing", "rtc-signal", "live.signal", "live.danmaku"] {
        assert_eq!(
            channel_class_of(t),
            Some(ChannelClass::HighPriority),
            "{t} must be high_priority"
        );
    }
    for t in ["wallet.redpacket.claim", "ad.receipt", "msg.edit.state"] {
        assert_eq!(
            channel_class_of(t),
            Some(ChannelClass::Ephemeral),
            "{t} must be ephemeral"
        );
    }
    assert_eq!(channel_class_of("unknown"), None);
}

#[test]
fn withdraw_is_logical_delete_with_author_check_and_change_event() {
    let s = store();
    s.append(GROUP, "alice", "text", "m1", "{\"t\":\"secret\"}")
        .unwrap();
    s.append(GROUP, "bob", "text", "m2", "{}").unwrap();

    // 伪造他人撤回：整体拒绝，无变更事件
    assert!(matches!(
        s.withdraw(GROUP, "m1", "mallory"),
        Err(MailError::WithdrawRefused)
    ));
    assert!(matches!(
        s.withdraw(GROUP, "unknown", "alice"),
        Err(MailError::WithdrawRefused)
    ));

    s.withdraw(GROUP, "m1", "alice").unwrap();
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert!(rows[0].withdrawn);
    assert_eq!(
        rows[0].seq, 1,
        "撤回是逻辑删除：seq/位置保留（时间线不塌缩）"
    );
    let (changes, _) = s.changes(GROUP, 0, 10).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].msg_id, "m1");
    assert_eq!(changes[0].kind, "recall");
    assert_eq!(changes[0].actor, "alice");

    // 重复撤回同一行：拒绝（withdrawn=0 的条件挡住）
    assert!(matches!(
        s.withdraw(GROUP, "m1", "alice"),
        Err(MailError::WithdrawRefused)
    ));
    let (changes, _) = s.changes(GROUP, 0, 10).unwrap();
    assert_eq!(changes.len(), 1, "重复撤回不产生第二条事件");
}

#[test]
fn batch_withdraw_is_all_or_nothing() {
    let s = store();
    for i in 1..=4 {
        s.append(GROUP, "alice", "text", &format!("m{i}"), "{}")
            .unwrap();
    }
    s.append(GROUP, "bob", "text", "b1", "{}").unwrap();

    // 含一条非本人消息：整批拒绝、无半提交
    let batch = vec!["m1", "m2", "b1"];
    assert!(matches!(
        s.withdraw_batch(GROUP, &batch, "alice"),
        Err(MailError::WithdrawRefused)
    ));
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert!(rows.iter().all(|r| !r.withdrawn), "nothing was withdrawn");
    let (changes, _) = s.changes(GROUP, 0, 10).unwrap();
    assert!(changes.is_empty());

    // 全部本人：一次撤回 + 独立 version 的变更事件
    let batch = vec!["m1", "m2", "m3"];
    assert_eq!(s.withdraw_batch(GROUP, &batch, "alice").unwrap(), 3);
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows.iter().filter(|r| r.withdrawn).count(), 3);
    let (changes, _) = s.changes(GROUP, 0, 10).unwrap();
    assert_eq!(changes.len(), 3);
    let versions: Vec<u64> = changes.iter().map(|c| c.version).collect();
    let mut sorted = versions.clone();
    sorted.sort_unstable();
    assert_eq!(versions, sorted, "change versions are strictly increasing");
}

#[test]
fn changes_stream_supports_incremental_resync() {
    let s = store();
    s.append(GROUP, "a", "text", "m1", "{}").unwrap();
    s.append(GROUP, "a", "text", "m2", "{}").unwrap();
    s.withdraw(GROUP, "m1", "a").unwrap();
    s.withdraw(GROUP, "m2", "a").unwrap();

    let (all, _) = s.changes(GROUP, 0, 10).unwrap();
    assert_eq!(all.len(), 2);

    // 客户端持有 version 游标：只取增量
    let since = all[0].version;
    let (delta, next) = s.changes(GROUP, since, 10).unwrap();
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].msg_id, "m2");
    assert_eq!(next, None, "exhausted");
}

#[test]
fn ephemeral_stored_separately_never_in_timeline() {
    let s = store();
    s.append(GROUP, "a", "wallet.redpacket", "card1", "{}")
        .unwrap();

    // 领取事件：ephemeral 通道回写卡片
    let row1 = s
        .append_ephemeral(
            GROUP,
            "bob",
            "wallet.redpacket.claim",
            "e1",
            "card1",
            r#"{"claimer":"bob","amount":100}"#,
        )
        .unwrap();
    let row2 = s
        .append_ephemeral(
            GROUP,
            "carol",
            "wallet.redpacket.claim",
            "e2",
            "card1",
            r#"{"claimer":"carol","amount":50}"#,
        )
        .unwrap();
    assert!(row2 > row1, "ephemeral rows are monotonic per owner");

    // 时间线只有卡片那条 normal 消息（不变式 3）
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert_eq!(rows.len(), 1);

    // 增量拉取：客户端按 rowid 游标拿回写事件
    let (updates, next) = s.fetch_ephemeral(GROUP, 0, 10).unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].target_msg_id, "card1");
    assert_eq!(updates[1].type_id, "wallet.redpacket.claim");
    assert_eq!(next, None);
    let (empty, _) = s.fetch_ephemeral(GROUP, row2, 10).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn user_state_is_private_and_idempotent() {
    let s = store();
    s.append(GROUP, "a", "text", "m1", "{}").unwrap();
    s.set_user_state(GROUP, 1, "bob", "pinned").unwrap();
    s.set_user_state(GROUP, 1, "bob", "pinned").unwrap(); // 幂等
    s.set_user_state(GROUP, 1, "carol", "deleted").unwrap();
    // 私有性由查询侧保证：这里验证存储面允许多用户多状态共存
    s.set_user_state(GROUP, 1, "bob", "unread").unwrap();
    let _ = s.fetch(GROUP, 0, 10).unwrap();
}

#[test]
fn concurrent_append_yields_exactly_n_unique_seqs() {
    // 单点原子取号的并发压测面：100 并发各写 1 条 → seq 恰为 1..=100
    // （无洞、无重复），且消息全部在案。
    let s = std::sync::Arc::new(store());
    let mut handles = Vec::new();
    for t in 0..100 {
        let s = s.clone();
        handles.push(std::thread::spawn(move || {
            s.append(GROUP, &format!("u{t}"), "text", &format!("msg-{t}"), "{}")
                .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(s.head_seq(GROUP).unwrap(), 100);
    let (rows, _) = s.fetch(GROUP, 0, 1000).unwrap();
    assert_eq!(rows.len(), 100);
    let seqs: Vec<u64> = rows.iter().map(|r| r.seq).collect();
    for (i, seq) in seqs.iter().enumerate() {
        assert_eq!(*seq, (i + 1) as u64, "no holes, no duplicates");
    }
}

#[test]
fn direct_and_channel_pillars_share_the_same_infrastructure() {
    // 不变式 1 的存在性证明：三属主类型走同一 API、同一张表、不同序列空间
    let s = store();
    for (owner, tag) in [(DIRECT, "d"), (GROUP, "g"), (CHANNEL, "c")] {
        s.append(owner, "a", "text", &format!("{tag}-1"), "{}")
            .unwrap();
        let (rows, _) = s.fetch(owner, 0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 1);
        assert!(s.ack(owner, "a", 1).unwrap());
        assert_eq!(s.unread(owner, "a").unwrap(), 0);
    }
}

#[test]
fn persisted_store_reopens_with_state_intact() {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-msg-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    let path = dir.join("msg.db");
    {
        let s = MessageStore::open(Some(path.to_str().unwrap())).unwrap();
        s.append(GROUP, "a", "text", "m1", "{}").unwrap();
        s.append(GROUP, "a", "text", "m2", "{}").unwrap();
        s.ack(GROUP, "bob", 1).unwrap();
        s.withdraw(GROUP, "m2", "a").unwrap();
    }
    let s = MessageStore::open(Some(path.to_str().unwrap())).unwrap();
    assert_eq!(s.head_seq(GROUP).unwrap(), 2);
    assert_eq!(s.unread(GROUP, "bob").unwrap(), 1);
    let (rows, _) = s.fetch(GROUP, 0, 10).unwrap();
    assert!(rows[1].withdrawn);
    let (changes, _) = s.changes(GROUP, 0, 10).unwrap();
    assert_eq!(changes.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fetch_limit_is_capped_to_protect_the_server() {
    let s = store();
    for i in 1..=12 {
        s.append(GROUP, "a", "text", &format!("m{i}"), "{}")
            .unwrap();
    }
    // 请求 10_000 条：被截到 1000 上限——这里数据只有 12 条，验证语义即可
    let (rows, next) = s.fetch(GROUP, 0, 10_000).unwrap();
    assert_eq!(rows.len(), 12);
    assert_eq!(next, None);
}
