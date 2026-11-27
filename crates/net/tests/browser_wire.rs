//! 浏览器 bincode 镜像 ↔ 真实 hub wire 的字节兼容测试。
//!
//! C-04 的协议漂移不能只靠“两边字段看起来一样”：bincode 是位置编码，
//! 枚举序号/字段顺序/长度前缀任一变化都会静默读错。这里把客户端编码的
//! 控制帧交给服务端类型解码，再把服务端编码的帧交给客户端解码。

use wxlike_client_web::wire as browser;
use wxlike_net::wire::WireFrame as ServerFrame;

#[test]
fn browser_client_hello_is_the_server_frame() {
    let frame = browser::WireFrame::ClientHello {
        device_id: "web-abc".into(),
        protocol: browser::PROTOCOL_V3,
        caps: browser::CAP_RESUME,
    };
    let bytes = frame.to_bytes().expect("browser encode");
    let server = ServerFrame::from_bytes(&bytes).expect("server decode");
    match server {
        ServerFrame::ClientHello {
            device_id,
            protocol,
            caps,
        } => {
            assert_eq!(device_id, "web-abc");
            assert_eq!(protocol, 3);
            assert_eq!(caps, 2);
        }
        other => panic!("decoded wrong variant: {other:?}"),
    }
}

#[test]
fn browser_hello_and_subscribe_match_the_server() {
    let hello = browser::WireFrame::Hello {
        token: "tok".into(),
    };
    let bytes = hello.to_bytes().expect("encode");
    assert!(matches!(
        ServerFrame::from_bytes(&bytes).expect("decode"),
        ServerFrame::Hello { token } if token == "tok"
    ));

    let subscribe = browser::WireFrame::Subscribe {
        scopes: vec!["conv:*".into(), "conv:g-1".into()],
    };
    let bytes = subscribe.to_bytes().expect("encode");
    match ServerFrame::from_bytes(&bytes).expect("decode") {
        ServerFrame::Subscribe { scopes } => {
            assert_eq!(scopes, vec!["conv:*".to_string(), "conv:g-1".to_string()]);
        }
        other => panic!("decoded wrong variant: {other:?}"),
    }
}

#[test]
fn server_auth_and_resume_decode_in_the_browser() {
    let auth = ServerFrame::Auth {
        user_id: "u-alice".into(),
        device_id: "web-abc".into(),
        session_id: "s-1".into(),
        version: 3,
    };
    let bytes = auth.to_bytes().expect("server encode");
    match browser::WireFrame::from_bytes(&bytes).expect("browser decode") {
        browser::WireFrame::Auth {
            user_id,
            device_id,
            version,
            ..
        } => {
            assert_eq!(user_id, "u-alice");
            assert_eq!(device_id, "web-abc");
            assert_eq!(version, 3);
        }
        other => panic!("decoded wrong variant: {other:?}"),
    }

    let resume = ServerFrame::Resume {
        conv_id: "g-1".into(),
        cursor: 41,
    };
    let bytes = resume.to_bytes().expect("server encode");
    match browser::WireFrame::from_bytes(&bytes).expect("browser decode") {
        browser::WireFrame::Resume { conv_id, cursor } => {
            assert_eq!(conv_id, "g-1");
            assert_eq!(cursor, 41);
        }
        other => panic!("decoded wrong variant: {other:?}"),
    }
}

#[test]
fn server_message_decodes_into_the_browser_envelope() {
    let env = wxlike_core::Envelope::text("d-alice~bob", "u-alice", 42, "hello");
    let bytes = ServerFrame::Msg(env).to_bytes().expect("server encode");
    match browser::WireFrame::from_bytes(&bytes).expect("browser decode") {
        browser::WireFrame::Msg(env) => {
            assert_eq!(env.version, 2);
            assert_eq!(env.conv_id, "d-alice~bob");
            assert_eq!(env.sender, "u-alice");
            assert_eq!(env.seq, 42);
            assert_eq!(env.body, browser::MsgBody::Text("hello".into()));
        }
        other => panic!("decoded wrong variant: {other:?}"),
    }
}
