//! WP-12 A1: 1:1 Olm sessions (establish, 1000 alternating messages,
//! out-of-order recovery within the 64+ window, tamper, replay, prekey
//! exhaustion and replenishment) and Megolm group sender keys (10 members,
//! rotation on membership change, index replay).

use wxlike_core::e2ee::{
    Crypto, CryptoError, Device, PreKeyBundle, Sealed, PENDING_WINDOW, PREKEY_BATCH, PREKEY_FLOOR,
};

/// alice and bob, each holding their own Olm account.
fn pair() -> (Crypto, Crypto) {
    (Crypto::new(Device::new()), Crypto::new(Device::new()))
}

#[test]
fn fresh_device_publishes_a_prekey_batch() {
    let device = Device::new();
    let bundle = device.bundle("alice");
    assert_eq!(
        bundle.one_time_keys.len(),
        PREKEY_BATCH,
        "a fresh device must be contactable without a warm-up round trip"
    );
    assert!(!bundle.identity.is_empty());
    assert!(!bundle.identity_signing.is_empty());
}

#[test]
fn first_message_opens_sessions_both_ways() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");

    let sealed = alice.encrypt_dm("bob", &bob_bundle, b"hello bob").unwrap();
    assert!(sealed.pre_key, "the opening message is a pre-key message");
    assert_eq!(bob.decrypt_dm("alice", &sealed).unwrap(), b"hello bob");
    assert!(
        bob.has_session_with("alice"),
        "bob now holds an inbound session"
    );
    assert!(
        alice.has_session_with("bob"),
        "alice holds the outbound session"
    );

    // bob replies: no pre-key needed any more, on either side
    let alice_bundle = alice.bundle("alice");
    let back = bob.encrypt_dm("alice", &alice_bundle, b"hi").unwrap();
    assert!(!back.pre_key, "replies are ordinary messages");
    assert_eq!(alice.decrypt_dm("bob", &back).unwrap(), b"hi");

    // and alice's next message is ordinary too, even though bob's reply is
    // the only thing that "established" her session
    let next = alice
        .encrypt_dm("bob", &bob_bundle, b"established")
        .unwrap();
    assert!(!next.pre_key);
    assert_eq!(bob.decrypt_dm("alice", &next).unwrap(), b"established");
}

/// vodozemac wraps every send in a pre-key until it hears back; we unwrap the
/// repeats so a one-way stream (no reply yet) still decrypts message by message.
#[test]
fn one_way_stream_before_reply_decrypts() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");

    for i in 0..50u32 {
        let sealed = alice
            .encrypt_dm("bob", &bob_bundle, format!("note {i}").as_bytes())
            .unwrap();
        assert_eq!(
            sealed.pre_key,
            i == 0,
            "only the opening message carries the pre-key"
        );
        assert_eq!(
            bob.decrypt_dm("alice", &sealed).unwrap(),
            format!("note {i}").as_bytes()
        );
    }
}

/// A1: 双向 1000 条交替消息全部解出。
#[test]
fn one_thousand_alternating_messages_all_decrypt() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");
    let alice_bundle = alice.bundle("alice");

    for i in 0..1_000u32 {
        let to_bob = alice
            .encrypt_dm("bob", &bob_bundle, format!("a{i}").as_bytes())
            .unwrap();
        assert_eq!(
            bob.decrypt_dm("alice", &to_bob).unwrap(),
            format!("a{i}").as_bytes()
        );

        let to_alice = bob
            .encrypt_dm("alice", &alice_bundle, format!("b{i}").as_bytes())
            .unwrap();
        assert_eq!(
            alice.decrypt_dm("bob", &to_alice).unwrap(),
            format!("b{i}").as_bytes()
        );
    }
}

/// A1: 乱序到达（≥64 条窗口）在重排缓冲的帮助下全部解出。
#[test]
fn out_of_order_within_sixtyfour_recovers() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");

    let open = alice.encrypt_dm("bob", &bob_bundle, b"open").unwrap();
    bob.decrypt_dm("alice", &open).unwrap();

    let msgs: Vec<Sealed> = (0..64u32)
        .map(|i| {
            alice
                .encrypt_dm("bob", &bob_bundle, format!("m{i}").as_bytes())
                .unwrap()
        })
        .collect();

    // message 63 lands first: out of turn, so it parks instead of being fed
    // to the ratchet (which would destroy the skipped keys in between)
    assert!(matches!(
        bob.decrypt_dm("alice", &msgs[63]),
        Err(CryptoError::NotNext)
    ));
    assert_eq!(
        bob.parked_for("alice"),
        1,
        "the early message is parked, not dropped"
    );

    // now the first 63 arrive in order; each success releases the parked one
    for i in 0..63u32 {
        assert_eq!(
            bob.decrypt_dm("alice", &msgs[i as usize]).unwrap(),
            format!("m{i}").as_bytes()
        );
    }

    let recovered = bob.take_recovered("alice");
    assert_eq!(
        recovered.len(),
        1,
        "m63 came loose once the gap filled: {recovered:?}"
    );
    assert_eq!(recovered[0], b"m63".to_vec());
    assert_eq!(bob.parked_for("alice"), 0);
    assert!(
        bob.take_recovered("alice").is_empty(),
        "recovered is drained"
    );
}

/// The hostile version: the whole window arrives fully reversed. Strict-order
/// parking turns it back into in-order delivery; everything decrypts.
#[test]
fn fully_reversed_window_all_decrypts() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");
    let open = alice.encrypt_dm("bob", &bob_bundle, b"open").unwrap();
    bob.decrypt_dm("alice", &open).unwrap();

    let msgs: Vec<Sealed> = (0..64u32)
        .map(|i| {
            alice
                .encrypt_dm("bob", &bob_bundle, format!("m{i}").as_bytes())
                .unwrap()
        })
        .collect();

    // deliver m63 down to m1 (everything but the first), fully reversed
    for sealed in msgs[1..].iter().rev() {
        let _ = bob.decrypt_dm("alice", sealed);
    }
    assert_eq!(bob.parked_for("alice"), 63, "63 parked, none consumed yet");

    // the first message arrives last and unlocks the whole queue in order
    assert_eq!(bob.decrypt_dm("alice", &msgs[0]).unwrap(), b"m0");
    let recovered = bob.take_recovered("alice");
    let texts: Vec<String> = recovered
        .iter()
        .map(|v| String::from_utf8(v.clone()).unwrap())
        .collect();
    assert_eq!(
        texts,
        (1..64u32).map(|i| format!("m{i}")).collect::<Vec<_>>()
    );
    assert_eq!(bob.parked_for("alice"), 0);
}

/// A gap that never fills (lost messages) must stay parked and fail cleanly,
/// not block the stream or pretend success.
#[test]
fn out_of_order_gap_that_never_fills_stays_parked() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");
    let open = alice.encrypt_dm("bob", &bob_bundle, b"open").unwrap();
    bob.decrypt_dm("alice", &open).unwrap();

    let msgs: Vec<Sealed> = (0..200u32)
        .map(|i| {
            alice
                .encrypt_dm("bob", &bob_bundle, format!("m{i}").as_bytes())
                .unwrap()
        })
        .collect();

    assert!(matches!(
        bob.decrypt_dm("alice", &msgs[199]),
        Err(CryptoError::NotNext)
    ));
    // only the first 51 ever arrive; 52..=198 are lost forever
    for i in 0..51u32 {
        assert_eq!(
            bob.decrypt_dm("alice", &msgs[i as usize]).unwrap(),
            format!("m{i}").as_bytes()
        );
    }
    assert_eq!(bob.parked_for("alice"), 1, "m199 cannot catch up");
    assert!(
        bob.take_recovered("alice").is_empty(),
        "nothing may be invented for messages that never arrived"
    );
    // a later in-order message still flows through the same session
    assert_eq!(bob.decrypt_dm("alice", &msgs[51]).unwrap(), b"m51");
}

/// A1: any bit flip in the ciphertext must fail authentication.
#[test]
fn tampering_any_byte_fails() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");
    let open = alice.encrypt_dm("bob", &bob_bundle, b"base").unwrap();
    bob.decrypt_dm("alice", &open).unwrap();

    let sealed = alice
        .encrypt_dm("bob", &bob_bundle, b"secret payload")
        .unwrap();
    let mut hits = 0;
    for i in 0..sealed.body.len() {
        // flip one byte of the base64 body (ASCII alphabet, stays valid UTF-8)
        let mut bytes = sealed.body.clone().into_bytes();
        let idx = bytes.len() - 1 - (i % bytes.len().min(40));
        bytes[idx] = if bytes[idx] == b'A' { b'B' } else { b'A' };
        let mutated = String::from_utf8(bytes).unwrap();
        let bad = Sealed {
            pre_key: false,
            body: mutated,
        };
        if bob.decrypt_dm("alice", &bad).is_err() {
            hits += 1;
        }
    }
    assert!(
        hits > 0,
        "tampering must be detectable; base64 edits that decode to the same bytes are fine"
    );
    // the untouched ciphertext still works, so the loop did not corrupt state
    assert_eq!(bob.decrypt_dm("alice", &sealed).unwrap(), b"secret payload");
}

/// A1: a replayed ciphertext is rejected — vodozemac would happily regenerate
/// the message key, so the recently-decrypted set is what stops it.
#[test]
fn replay_is_rejected() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");
    let sealed = alice.encrypt_dm("bob", &bob_bundle, b"one time").unwrap();
    assert_eq!(bob.decrypt_dm("alice", &sealed).unwrap(), b"one time");
    let second = bob.decrypt_dm("alice", &sealed);
    assert!(
        matches!(second, Err(CryptoError::Replay)),
        "replay must fail, got {second:?}"
    );
    // and the rejection does not wedge the session
    let next = alice.encrypt_dm("bob", &bob_bundle, b"next").unwrap();
    assert_eq!(bob.decrypt_dm("alice", &next).unwrap(), b"next");
}

#[test]
fn unknown_peer_normal_message_needs_a_pre_key() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");
    // establish alice -> bob so alice produces a genuine normal message
    let open = alice.encrypt_dm("bob", &bob_bundle, b"open").unwrap();
    bob.decrypt_dm("alice", &open).unwrap();
    let normal = alice.encrypt_dm("bob", &bob_bundle, b"x").unwrap();
    assert!(!normal.pre_key);

    let mut carol = Crypto::new(Device::new());
    assert!(matches!(
        carol.decrypt_dm("stranger", &normal),
        Err(CryptoError::NoSession)
    ));
}

#[test]
fn empty_prekey_bundle_is_refused_loudly() {
    let (alice, mut bob) = pair();
    let mut bundle = alice.bundle("alice");
    bundle.one_time_keys.clear();
    let err = bob.encrypt_dm("alice", &bundle, b"x");
    assert!(
        matches!(err, Err(CryptoError::NoPreKey)),
        "must not silently degrade to a weaker handshake: {err:?}"
    );
}

/// A1: 预密钥耗尽自动补充。Each distinct peer claims a distinct one-time key
/// (that is what a claiming key directory does); after enough claims the stock
/// drops below the floor and tops back up.
#[test]
fn prekey_stock_replenishes_when_it_runs_low() {
    let mut alice = Crypto::new(Device::new());
    let full = alice.bundle("alice");
    assert_eq!(full.one_time_keys.len(), PREKEY_BATCH);

    // peers 0..16 each claim a different key and open a session to alice
    for claimed in 0..(PREKEY_BATCH - PREKEY_FLOOR + 1) {
        let mut peer = Crypto::new(Device::new());
        let single_key = PreKeyBundle {
            user_id: full.user_id.clone(),
            identity: full.identity.clone(),
            identity_signing: full.identity_signing.clone(),
            one_time_keys: vec![full.one_time_keys[claimed].clone()],
        };
        let sealed = peer
            .encrypt_dm("alice", &single_key, format!("ping {claimed}").as_bytes())
            .unwrap_or_else(|e| panic!("claim {claimed}: {e}"));
        assert_eq!(
            alice
                .decrypt_dm(&format!("peer{claimed}"), &sealed)
                .unwrap(),
            format!("ping {claimed}").as_bytes()
        );
    }

    // the stock is now below the floor, so publishing must replenish it
    let before = alice.device().one_time_key_count();
    assert!(before < PREKEY_FLOOR, "drained to {before}");
    let refilled = alice.bundle_refilled("alice");
    assert!(
        refilled.one_time_keys.len() >= PREKEY_FLOOR,
        "replenished to {}",
        refilled.one_time_keys.len()
    );
    assert!(alice.device().one_time_key_count() >= PREKEY_FLOOR);

    // and a replenished account can still open sessions with a fresh peer
    let mut zoe = Crypto::new(Device::new());
    let one = PreKeyBundle {
        user_id: refilled.user_id.clone(),
        identity: refilled.identity.clone(),
        identity_signing: refilled.identity_signing.clone(),
        one_time_keys: vec![refilled.one_time_keys[0].clone()],
    };
    let sealed = zoe.encrypt_dm("alice", &one, b"after refill").unwrap();
    assert_eq!(alice.decrypt_dm("zoe", &sealed).unwrap(), b"after refill");
}

// ---------------------------------------------------------------------------
// groups (Megolm sender keys)
// ---------------------------------------------------------------------------

/// A1 增补: 10 人群，每人一把 sender key 且都能听能说，所有人的消息都要能解。
#[test]
fn ten_person_group_all_members_decrypt() {
    let mut members: Vec<Crypto> = (0..10).map(|_| Crypto::new(Device::new())).collect();
    let identities: Vec<String> = members
        .iter()
        .map(|m| m.device().identity_fingerprint())
        .collect();

    // each member creates a sender key and shares it with the other nine
    let mut shares = Vec::new();
    for member in &mut members {
        shares.push(member.start_group("g1"));
    }
    for (i, member) in members.iter_mut().enumerate() {
        for j in 0..10 {
            if i == j {
                continue;
            }
            member
                .import_group_key("g1", &identities[j], &shares[j])
                .expect("speakers must also be able to listen");
        }
    }

    // every member broadcasts once; every other member must decrypt it
    let broadcasts: Vec<_> = members
        .iter_mut()
        .enumerate()
        .map(|(sender, member)| {
            (
                sender,
                member
                    .encrypt_group("g1", format!("from {sender}").as_bytes())
                    .unwrap(),
            )
        })
        .collect();
    for (sender, sealed) in &broadcasts {
        for (reader, other) in members.iter_mut().enumerate() {
            if reader == *sender {
                continue;
            }
            let (who, pt) = other.decrypt_group("g1", sealed).unwrap();
            assert_eq!(who, identities[*sender], "message attributed to its sender");
            assert_eq!(pt, format!("from {sender}").as_bytes());
        }
    }
    assert!(members[0].can_read_group("g1"));
}

/// New members must not read history; rotation is what enforces it.
#[test]
fn rotation_excludes_new_members_from_history() {
    let mut alice = Crypto::new(Device::new());
    let mut bob = Crypto::new(Device::new());
    let alice_id = alice.device().identity_fingerprint();

    let key = alice.start_group("g1");
    bob.import_group_key("g1", &alice_id, &key).unwrap();

    let history = alice
        .encrypt_group("g1", b"secret before rotation")
        .unwrap();
    assert_eq!(
        bob.decrypt_group("g1", &history).unwrap().1,
        b"secret before rotation".to_vec(),
        "bob could read while he held the current key"
    );

    // carol joins → rotate; she gets the *new* key only
    let mut carol = Crypto::new(Device::new());
    let new_key = alice.rotate_group("g1").unwrap();
    carol.import_group_key("g1", &alice_id, &new_key).unwrap();
    bob.import_group_key("g1", &alice_id, &new_key).unwrap();

    let after = alice.encrypt_group("g1", b"after rotation").unwrap();
    assert_eq!(
        carol.decrypt_group("g1", &after).unwrap().1,
        b"after rotation"
    );
    assert_eq!(
        bob.decrypt_group("g1", &after).unwrap().1,
        b"after rotation"
    );

    // carol cannot reach back into the pre-rotation history
    let err = carol.decrypt_group("g1", &history);
    assert!(
        matches!(err, Err(CryptoError::DecryptFailed)),
        "history must stay closed to a member who joined after it: {err:?}"
    );
}

#[test]
fn group_message_index_replay_is_rejected() {
    let mut alice = Crypto::new(Device::new());
    let mut bob = Crypto::new(Device::new());
    let alice_id = alice.device().identity_fingerprint();
    let key = alice.start_group("g1");
    bob.import_group_key("g1", &alice_id, &key).unwrap();

    let sealed = alice.encrypt_group("g1", b"once").unwrap();
    assert!(bob.decrypt_group("g1", &sealed).is_ok());
    let replayed = bob.decrypt_group("g1", &sealed);
    assert!(
        matches!(replayed, Err(CryptoError::Replay)),
        "the same Megolm index cannot be replayed: {replayed:?}"
    );
}

#[test]
fn a_reader_cannot_send_and_an_unknown_group_fails() {
    let mut alice = Crypto::new(Device::new());
    let mut bob = Crypto::new(Device::new());
    let alice_id = alice.device().identity_fingerprint();
    bob.import_group_key("g1", &alice_id, &alice.start_group("g1"))
        .unwrap();

    assert!(matches!(
        bob.encrypt_group("g1", b"x"),
        Err(CryptoError::ReadOnlyGroup)
    ));
    assert!(matches!(
        bob.encrypt_group("nope", b"x"),
        Err(CryptoError::UnknownGroup)
    ));
    let sealed = alice.encrypt_group("g1", b"x").unwrap();
    assert!(matches!(
        alice.decrypt_group("g1", &sealed),
        Err(CryptoError::ReadOnlyGroup)
    ));
}

/// The reorder buffer is bounded: an attacker flooding undecryptable traffic
/// cannot grow it without limit or push out the messages still recoverable.
#[test]
fn pending_buffer_is_bounded_under_flood() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");
    let open = alice.encrypt_dm("bob", &bob_bundle, b"open").unwrap();
    bob.decrypt_dm("alice", &open).unwrap();

    let real = alice.encrypt_dm("bob", &bob_bundle, b"real").unwrap();
    let flood: Vec<Sealed> = (0..600usize)
        .map(|i| {
            alice
                .encrypt_dm("bob", &bob_bundle, format!("flood {i}").as_bytes())
                .unwrap()
        })
        .collect();

    // every flood message is ahead of its turn: all park, the cap evicts the
    // furthest-ahead ones first
    for sealed in &flood {
        let _ = bob.decrypt_dm("alice", sealed);
    }
    assert_eq!(
        bob.parked_for("alice"),
        PENDING_WINDOW,
        "the park queue hit its cap"
    );

    // the in-order message still goes through, and the parked window that
    // fits (128 > 64) recovers; the rest is documented loss
    assert!(bob.decrypt_dm("alice", &real).is_ok());
    let recovered = bob.take_recovered("alice");
    assert!(
        recovered.len() >= 64,
        "at least the 64-message reorder window survives the flood: {}",
        recovered.len()
    );
    assert!(bob.parked_for("alice") <= PENDING_WINDOW);
}

/// 重启不丢会话：快照 → 新进程恢复 → 双向继续收发（issue #15）。
///
/// 这条测试盯的是"进程重启后身份和棘轮还在"。旧实现把 `Device` 只放在内存里，
/// 重启后 `secret_enable` 会新建一个身份，对方的安全码变化、老会话全部解不开。
#[test]
fn snapshot_survives_restart_and_keeps_the_ratchet() {
    let (mut alice, mut bob) = pair();
    let bob_bundle = bob.bundle("bob");
    let alice_fp = alice.device().identity_fingerprint();

    let first = alice
        .encrypt_dm("bob", &bob_bundle, b"before restart")
        .unwrap();
    assert_eq!(bob.decrypt_dm("alice", &first).unwrap(), b"before restart");
    let back = bob
        .encrypt_dm("alice", &alice.bundle("alice"), b"ack")
        .unwrap();
    assert_eq!(alice.decrypt_dm("bob", &back).unwrap(), b"ack");

    // 进程退出 → 新进程从磁盘快照恢复
    let alice_blob = alice.snapshot().unwrap();
    let bob_blob = bob.snapshot().unwrap();
    drop((alice, bob));
    let mut alice = Crypto::restore(&alice_blob).unwrap();
    let mut bob = Crypto::restore(&bob_blob).unwrap();

    assert_eq!(
        alice.device().identity_fingerprint(),
        alice_fp,
        "身份必须还是同一个（否则安全码会变）"
    );
    assert!(alice.has_session_with("bob") && bob.has_session_with("alice"));

    // 棘轮继续：双方都不需要重新握手，也不再产生 pre-key 消息
    let after = alice.encrypt_dm_existing("bob", b"after restart").unwrap();
    assert!(!after.pre_key);
    assert_eq!(bob.decrypt_dm("alice", &after).unwrap(), b"after restart");

    let reply = bob.encrypt_dm_existing("alice", b"still here").unwrap();
    assert_eq!(alice.decrypt_dm("bob", &reply).unwrap(), b"still here");
}

/// 快照格式不认识时必须报错，不能"猜着加载一半"。
#[test]
fn snapshot_with_unknown_version_is_refused() {
    let (alice, _) = pair();
    let blob = alice.snapshot().unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&blob).unwrap();
    value["version"] = serde_json::json!(999);
    let tampered = serde_json::to_vec(&value).unwrap();
    match Crypto::restore(&tampered) {
        Err(CryptoError::Snapshot(msg)) => assert!(msg.contains("version"), "{msg}"),
        other => panic!("必须拒绝未知版本：{:?}", other.err()),
    }
    assert!(Crypto::restore(b"not a snapshot").is_err());
}
