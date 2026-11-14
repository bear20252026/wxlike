#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_ms_formats_utc() {
        assert_eq!(iso_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_ms(1_000), "1970-01-01T00:00:01.000Z");
        // 2026-10-25 00:00:00 UTC = 1792838400
        assert_eq!(iso_ms(1_792_886_400_000), "2026-10-25T00:00:00.000Z");
        assert_eq!(iso_ms(86_399_000), "1970-01-01T23:59:59.000Z");
        // 闰日：2024-02-29 12:00:00 = 1709208000
        assert_eq!(iso_ms(1_709_208_000_000), "2024-02-29T12:00:00.000Z");
    }

    #[test]
    fn row_to_message_maps_kinds() {
        let me = "u1";
        let out = row_to_message(
            &json!({ "sender": "u1", "conv_id": "d-u1~u2", "seq": 3, "sent_at_ms": 1000, "msg_id": "ab", "kind": "text", "text": "hi" }),
            me,
        );
        assert_eq!(out["direction"], "out");
        assert_eq!(out["body"]["kind"], "text");
        assert_eq!(out["seq"], "3");
        let file = row_to_message(
            &json!({ "sender": "u2", "conv_id": "d-u1~u2", "seq": 4, "sent_at_ms": 2000, "msg_id": "cd", "kind": "msg.file", "payload": { "media_id": "m1", "name": "a.txt", "size": 5 } }),
            me,
        );
        assert_eq!(file["direction"], "in");
        assert_eq!(file["body"]["kind"], "file");
        assert_eq!(file["body"]["name"], "a.txt");
    }

    #[test]
    fn peer_of_finds_the_other_side() {
        assert_eq!(peer_of("d-u1~u2", "u1").as_deref(), Some("u2"));
        assert_eq!(peer_of("g-abc", "u1"), None);
    }

    #[test]
    fn bridge_error_codes() {
        assert_eq!(
            BridgeError::from("401 unauthorized".to_string()).code(),
            "UNAUTHENTICATED"
        );
        assert_eq!(
            BridgeError::from("未连接到服务器".to_string()).code(),
            "OFFLINE"
        );
        assert_eq!(BridgeError::from("boom".to_string()).code(), "INTERNAL");
        assert!(!BridgeError::NotImpl("x".into()).retryable());
    }

    #[test]
    fn cursor_parsing_accepts_string_and_number() {
        let p = json!({ "cursor": "12" });
        assert_eq!(cursor_usize(&p), 12);
        assert_eq!(cursor_i64(&p), 12);
        let p = json!({ "cursor": 7 });
        assert_eq!(cursor_usize(&p), 7);
        let p = json!({});
        assert_eq!(cursor_i64(&p), 0);
    }
}

