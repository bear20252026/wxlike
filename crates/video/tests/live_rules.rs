//! E2：直播清单窗口、凭据与路径校验的**规则**（不需要 ffmpeg，也不需要服务）。
//!
//! 落盘与 HTTP 面在 `live_hls.rs`，真分片端到端在 `live_ffmpeg.rs`。

use wxlike_video::{
    check_live_segment_name, live_playlist, live_url, playlist_of, LiveSegment, LiveWindow,
};

#[test]
fn live_window_trims_and_numbers_segments() {
    // 序号从 0 起，出窗口不回头
    let mut window = LiveWindow::new("k1", 3, 1_000);
    for i in 0..5 {
        window
            .append(&format!("seg_{i:03}.ts"), 4_000, 2_000 + i as i64)
            .unwrap();
    }
    assert_eq!(window.segments.len(), 3, "窗口只留最近 N 片");
    assert_eq!(window.next_seq, 5, "序号是全局递增的");
    let seqs: Vec<u64> = window.segments.iter().map(|s| s.seq).collect();
    assert_eq!(seqs, vec![2, 3, 4], "窗口里是第一/二/三片之后的三段");

    // 重推同一片必须挡住：清单里两条相同 EXTINF 会让播放器把时长算错
    let dup = window.append("seg_004.ts", 4_000, 8_000).unwrap_err();
    assert!(
        dup.to_string().contains("already packed"),
        "窗口内的重复分片名要报错: {dup}"
    );
    // 已经出窗口的旧名字：窗口只有 N 片的记忆，不再拦——它被当作"新的一片"
    // 赋新序号。真要防节点重放历史，正确做法是停播后换新流密钥，
    // 不是让打包器无界地记住所有见过的文件名。
    window.append("seg_000.ts", 4_000, 8_500).unwrap();
    assert_eq!(window.next_seq, 6, "旧名字重推会被当成新片");
    assert_eq!(window.segments.last().unwrap().seq, 5);

    // 停播后不再接受新片：推流端还在发而平台已停播，是要人看的问题
    window.end(9_000);
    assert!(window.ended);
    let err = window.append("seg_005.ts", 4_000, 10_000).unwrap_err();
    assert!(err.to_string().contains("already ended"), "{err}");
}

#[test]
fn live_playlist_is_a_rolling_window() {
    let segs = vec![
        LiveSegment {
            name: "seg_007.ts".into(),
            duration_ms: 4_000,
            seq: 7,
        },
        LiveSegment {
            name: "seg_008.ts".into(),
            duration_ms: 6_500,
            seq: 8,
        },
    ];
    let text = live_playlist(&segs, false);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "#EXTM3U");
    // MEDIA-SEQUENCE 取窗口第一片：播放器靠它判断"我手里第 6 片可以扔了"
    assert!(text.contains("#EXT-X-MEDIA-SEQUENCE:7"), "{text}");
    // TARGETDURATION 必须 ≥ 所有 EXTINF（RFC 8216），所以取最长片
    assert!(text.contains("#EXT-X-TARGETDURATION:7"), "{text}");
    assert!(text.contains("#EXTINF:4.000,\nseg_007.ts"), "{text}");
    assert!(text.contains("#EXTINF:6.500,\nseg_008.ts"), "{text}");
    assert!(!text.contains("#EXT-X-ENDLIST"), "没停播不能写 ENDLIST");

    // 停播：尾巴一份合法清单，观众能把它看完
    let ended = live_playlist(&segs, true);
    assert!(ended.ends_with("#EXT-X-ENDLIST\n"), "{ended}");

    // 空窗口也要合法（省略 TARGETDURATION 就是不合规清单）
    let empty = live_playlist(&[], false);
    assert!(empty.contains("#EXT-X-TARGETDURATION:"), "{empty}");
    assert!(empty.contains("#EXT-X-MEDIA-SEQUENCE:0"), "{empty}");
    assert!(!empty.contains("#EXTINF"), "{empty}");

    // window.json → 清单是同一条路径（handler 用的就是这个）
    let mut window = LiveWindow::new("k2", 2, 1_000);
    window.append("seg_000.ts", 3_000, 1_000).unwrap();
    assert!(playlist_of(&window).contains("seg_000.ts"));
}

#[test]
fn live_url_matches_the_platform_convention() {
    // 平台 `live_state()` 拼的是 `/live/{stream_key}/index.m3u8`
    // （crates/platform/src/live.rs）。这两处不一致 = 平台发给观众的地址打不开。
    assert_eq!(live_url("abc123"), "/live/abc123/index.m3u8");
    assert_eq!(
        live_url(&"f".repeat(24)),
        format!("/live/{}/index.m3u8", "f".repeat(24))
    );
}

#[test]
fn stream_keys_and_live_segment_names_refuse_traversal() {
    assert!(wxlike_video::check_stream_key("a1b2c3d4e5f6").is_ok());
    assert!(wxlike_video::check_stream_key("with-dash_and_underscore").is_ok());
    assert!(wxlike_video::check_stream_key("").is_err());
    assert!(wxlike_video::check_stream_key("../etc").is_err());
    assert!(wxlike_video::check_stream_key("has space").is_err());
    assert!(wxlike_video::check_stream_key(&"a".repeat(65)).is_err());

    assert!(check_live_segment_name("seg_000123.ts").is_ok());
    assert!(check_live_segment_name("init.mp4").is_ok());
    assert!(check_live_segment_name("media-0.m4s").is_ok());
    assert!(check_live_segment_name("../window.json").is_err());
    assert!(check_live_segment_name("a/b.ts").is_err());
    assert!(check_live_segment_name("a\\b.ts").is_err());
    assert!(check_live_segment_name("seg_000123.wav").is_err());
    assert!(check_live_segment_name("").is_err());
}

#[test]
fn ingest_token_round_trips_and_refuses_tampering() {
    let secret = "unit-test-secret";
    let key = "streamkey01";
    let now: u64 = 1_700_000_000_000;
    let exp = now + 60_000;
    let token = wxlike_video::ingest_token(secret, key, exp);

    // 形状：自包含 `<exp>.<hmac>`，凭这一步就能校验，不查库
    assert!(token.split_once('.').is_some(), "{token}");
    assert_eq!(token.split_once('.').unwrap().0, exp.to_string());
    assert_eq!(
        wxlike_video::sign_ingest(secret, key, exp),
        token.split_once('.').unwrap().1
    );

    assert!(wxlike_video::verify_ingest_token(secret, key, &token, now).is_ok());
    // 过期：过期即失效，不需要谁去吊销
    assert!(wxlike_video::verify_ingest_token(secret, key, &token, exp + 1).is_err());
    // 换流密钥（拿 A 路的凭据去推 B 路）
    assert!(wxlike_video::verify_ingest_token(secret, "other-key", &token, now).is_err());
    // 换 secret（接入节点配错密钥）
    assert!(wxlike_video::verify_ingest_token("wrong", key, &token, now).is_err());
    // 篡改过期时间往后延
    let (e, sig) = token.split_once('.').unwrap();
    let forged = format!("{}.{}", e.parse::<u64>().unwrap() + 10 * 60_000, sig);
    assert!(wxlike_video::verify_ingest_token(secret, key, &forged, now).is_err());
    // 缺签名 / 缺分隔符
    assert!(wxlike_video::verify_ingest_token(secret, key, "justexp", now).is_err());
    assert!(wxlike_video::verify_ingest_token(secret, key, "", now).is_err());
}
