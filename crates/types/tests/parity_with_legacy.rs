//! "提取旧类型而非复制一份"的等价性证明。
//!
//! `crates/types` 里的实现是从旧代码搬过来的，不是重写的。搬完之后旧实现
//! 还在（由 G00/G02 在各自的卡里删），这段时间必须证明两边**逐字一致**：
//! 一旦有人只改了一边，客户端就会算出不同的会话 id —— 表现是"同一个人
//! 两个会话"，而且不会报错。

use wxlike_types::{direct_conv_id, ConvId, ParsedConv, UserId};

#[test]
fn direct_conv_id_matches_the_legacy_client_impl() {
    let pairs = [
        ("alice", "bob"),
        ("bob", "alice"),
        ("z", "a"),
        ("u-1", "u-2"),
        ("A", "a"),
        ("user.name", "user_name"),
    ];
    for (left, right) in pairs {
        assert_eq!(
            direct_conv_id(left, right),
            wxlike_client::direct_conv_id(left, right),
            "({left}, {right}) 的会话 id 与旧实现不一致"
        );
    }
}

#[test]
fn legacy_conv_ids_still_parse_as_direct() {
    // 旧实现产出的 id 必须能被新解析器认出来，否则历史数据全部变成"群"
    let legacy = wxlike_client::direct_conv_id("alice", "bob");
    let conv = ConvId::parse(legacy).expect("旧 id 仍然合规");
    match wxlike_types::parse_conv(&conv).expect("可解析") {
        ParsedConv::Direct { left, right } => {
            assert_eq!(left, UserId::parse("alice").expect("user"));
            assert_eq!(right, UserId::parse("bob").expect("user"));
        }
        ParsedConv::Group { .. } => panic!("旧直聊 id 被解析成了群"),
    }
}
