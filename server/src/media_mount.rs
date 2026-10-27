//! 把 media 域（`crates/features/media`）装进运行中的 server —— issue #295 / G00。
//!
//! 这个 crate 之前是"孤儿特性域"：路由与 29 条 `/api/v1` 接口都在
//! `MediaApp::router`（`crates/features/media/src/http.rs:101`），但工作区里
//! 没有任何 crate 依赖它，所以线上打不到。本模块是**组合根装配**，三条硬规矩：
//!
//! 1. **鉴权只有一条路**。`Authorization: Bearer` → `Platform::lookup_token`，
//!    读的就是 `Platform::router` 里 `Authed` 提取器读的同一张 `sessions` 表
//!    （token 存的是 `token_digest` 摘要，过期即删）。
//!    media crate 自带的 `StaticBearer` / `fixture_verifier` 在这里一次都不出现。
//! 2. **成员事实只问平台**。`AudiencePort` 落到 `may_read_conv` / `is_member` /
//!    `groups_of_user`，media 域不自己从 id 猜归属。
//! 3. **签名密钥不另起一套**。下载短期链接的 HMAC 密钥就是
//!    `PlatformOptions::media_signing_key`（由 `WXLIKE_MEDIA_SECRET` 配置），
//!    与已挂载的 `/api/media/{id}/url` 共用一把 —— 两处各配一把，早晚会有一处
//!    拿着旧 key 签新链接。
//!
//! 中间件口径与另外两个挂载域一致：平台域把 risk/audit 闸装在自己 router 内
//! （`crates/platform/src/lib.rs:505-511`，`risk_mw`/`audit_mw` 两个 mod 都是
//! 私有的，server 拿不到），视频域同理；server 顶层不套 layer。media 域自带的
//! `DefaultBodyLimit`（16 MiB = `http.rs:40 MAX_BODY_BYTES`）在
//! `MediaApp::router` 内部，挂载点不改它 —— 它与已挂载 `/api/media` 的
//! 100 MiB 是两份互不相干的上限。

use std::sync::Arc;

use axum::http::HeaderMap;
use axum::Router;
use wxlike_media::http::{MediaApp, SessionVerifier};
use wxlike_media::{AudiencePort, MediaConfig, MediaService};
use wxlike_platform::{Authed, PlatformState};

/// 签名密钥的最短字节数。短于此长度的"密钥"是给部署方的错觉，不是安全边界；
/// 平台自己随机生成时用的就是这个长度（`normalize_options`）。
pub const MIN_SECRET_BYTES: usize = 32;

/// 媒体签名密钥的环境变量名（生产必填）。
pub const SECRET_ENV: &str = "WXLIKE_MEDIA_SECRET";

/// 会话校验端口的生产实现：包着平台的 `sessions` 表，没有任何"临时放行"分支。
pub struct PlatformSessions(PlatformState);

impl PlatformSessions {
    pub fn new(platform: PlatformState) -> Self {
        Self(platform)
    }
}

impl SessionVerifier for PlatformSessions {
    fn verify(&self, headers: &HeaderMap) -> Option<String> {
        // bearer 解析复用 `Authed::bearer_from`：scheme 大小写不敏感、空白容忍、
        // 空 token 拒绝——与平台 40+ 条接口同一份语义，不在这里重写一遍。
        let token = Authed::bearer_from(headers)?;
        match self.0.lookup_token(token) {
            Ok(Some(user_id)) => Some(user_id),
            Ok(None) => None,
            // 查库失败不等于"没登录"，但也绝不能放行。与 hub 的
            // `SessionAuth::authenticate`（server/src/lib.rs:397-403）同一口径：
            // 都是拒绝，区别只留在日志里。
            Err(err) => {
                tracing::error!(error = %err, "media: lookup_token failed, rejecting");
                None
            }
        }
    }
}

/// 成员事实端口：全部问平台，media 域不持有第二份归属判断。
pub struct PlatformAudience(PlatformState);

impl PlatformAudience {
    pub fn new(platform: PlatformState) -> Self {
        Self(platform)
    }
}

impl AudiencePort for PlatformAudience {
    fn may_read_conversation(&self, user_id: &str, conv_id: &str) -> bool {
        // 规则只有一份（审计第 5 轮的挂账就是"两处各写一遍迟早漂移"）。
        self.0.may_read_conv(user_id, conv_id)
    }

    fn may_read_group(&self, user_id: &str, group_id: &str) -> bool {
        self.0.is_member(group_id, user_id).unwrap_or(false)
    }

    fn visible_groups(&self, user_id: &str) -> Vec<String> {
        self.0.groups_of_user(user_id).unwrap_or_default()
    }
    // `visible_conversations` 故意留 trait 默认（空集）：直聊成员关系没有按用户
    // 索引，只能全表扫 `store.list_conversations()`（server 侧已有
    // `MAX_DIRECT_CONV_SCAN` 挂账）。空集的含义是"文件中心不投影别人的共享文件"
    // ——宁可少显示，不泄露。等 G03 给出按用户的会话查询再接上。
}

/// 装配 media 域路由。返回值由 `build_app` 直接 `merge`。
///
/// 密钥不足长度是**装配失败**（`Err`），不是"降级继续跑"：一把弱密钥签出来的
/// 下载链接看上去一切正常，只有想伪造的人才会发现它其实谁都能算。
pub fn assemble(platform: &PlatformState, db_path: Option<&str>) -> anyhow::Result<Router> {
    let secret = platform.options().media_signing_key.clone();
    if secret.trim().is_empty() || secret.len() < MIN_SECRET_BYTES {
        anyhow::bail!(
            "{SECRET_ENV} 未配置到至少 {MIN_SECRET_BYTES} 字节：media 域拒绝装配。
             短期下载链接的 HMAC 密钥为空或过短等于没有签名；
             生产用 {SECRET_ENV} 配一个多实例一致的值，
             本地开发可显式设 WXLIKE_DEV_INSECURE=1（届时用本次启动的随机密钥，
             重启即失效、多实例互不认，只够跑单进程）。"
        );
    }
    if crate::non_empty_env(SECRET_ENV).is_none() {
        // 到这里的只有测试与 WXLIKE_DEV_INSECURE=1 的开发进程：
        // 二进制的 `check_secrets` 会在缺 env 时先拒绝启动（server/src/main.rs）。
        tracing::warn!(
            "{SECRET_ENV} 未设置：media 下载链接用本次启动的随机密钥签名，\
             重启即失效、多实例互不认（只允许开发/CI 走到这里）"
        );
    }
    let config = MediaConfig::new(object_root(db_path)).with_signing_key(secret.as_bytes());
    let service = Arc::new(MediaService::open(
        config,
        db_path,
        Arc::new(PlatformAudience::new(platform.clone())),
    )?);
    Ok(MediaApp::new(
        Arc::clone(&service),
        Arc::new(PlatformSessions::new(platform.clone())),
    )
    .router())
}

/// 对象根目录：优先 `WXLIKE_MEDIA_DIR`（与已挂载的 `/api/media` 同一份配置，
/// 见 `crates/platform/src/media.rs:756`），没配则与库同级 `media/`；
/// 内存库走唯一临时目录 —— 与 video/search 的 sidecar 惯例一致，测试不会
/// 写进工作目录。
fn object_root(db_path: Option<&str>) -> std::path::PathBuf {
    match crate::non_empty_env("WXLIKE_MEDIA_DIR") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => crate::sidecar_dir(db_path, "media"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    use wxlike_platform::{Platform, PlatformOptions};

    /// PBKDF2 轮数与平台测试一致：单测不该为此跑 10 万轮。
    const TEST_ROUNDS: u32 = 1_000;
    /// 会话 TTL：够一个测试跑完。
    const TEST_TTL_MS: u64 = 60_000;

    fn platform() -> PlatformState {
        Platform::in_memory_with(PlatformOptions {
            session_ttl_ms: TEST_TTL_MS,
            pbkdf2_rounds: TEST_ROUNDS,
            ..Default::default()
        })
        .unwrap()
        .shared()
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        h
    }

    /// 校验器认的就是平台 `sessions` 表： issued → 通过、吊销 → 立刻不通过。
    /// 这条是"没有第二套鉴权、也没有放行分支"的直接证据。
    #[test]
    fn verifier_follows_the_platform_session_table() {
        let platform = platform();
        let user_id = platform.register("verifier", "pw").unwrap();
        let verifier = PlatformSessions::new(platform.clone());

        assert_eq!(verifier.verify(&HeaderMap::new()), None, "匿名绝不该有主体");
        assert_eq!(verifier.verify(&bearer("not-a-token")), None);

        let token = platform.create_session(&user_id, TEST_TTL_MS).unwrap();
        assert_eq!(
            verifier.verify(&bearer(&token)).as_deref(),
            Some(user_id.as_str())
        );

        platform.revoke_session(&token).unwrap();
        assert_eq!(
            verifier.verify(&bearer(&token)),
            None,
            "退出登录后同一 token 必须立刻失效"
        );
    }

    /// 挂载后的路由真的会拒绝匿名、真的会落到 handler（oneshot，不起端口）。
    #[tokio::test]
    async fn mounted_router_gates_anonymous_and_serves_the_session_owner() {
        let platform = platform();
        let app = assemble(&platform, None).unwrap();
        let user_id = platform.register("mounted", "pw").unwrap();
        let token = platform.create_session(&user_id, TEST_TTL_MS).unwrap();

        let anonymous = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/media/capabilities")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

        let authed = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/media/capabilities")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authed.status(), StatusCode::OK);
        let bytes = authed.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // 1 GiB 是 G04 的能力承诺，落到挂载后的 app 上仍然是它。
        assert_eq!(value["max_object_bytes"], wxlike_media::GIB);
    }

    /// 密钥太短 = 装配失败，不降级继续跑。`normalize_options` 只把**空**密钥
    /// 换成随机值，"abc" 这种 3 字符密钥会原样留下——正是部署方手滑的样子。
    #[test]
    fn weak_or_absent_secret_refuses_to_assemble() {
        let weak = Platform::in_memory_with(PlatformOptions {
            media_signing_key: "abc".into(),
            ..Default::default()
        })
        .unwrap()
        .shared();
        assert_eq!(weak.options().media_signing_key, "abc", "非空即原样保留");
        let err = assemble(&weak, None).expect_err("3 字节密钥不该装出 media 域");
        assert!(
            err.to_string()
                .contains(&format!("{SECRET_ENV} 未配置到至少")),
            "报错要点名 env，实际：{err}"
        );

        let empty = Platform::in_memory_with(PlatformOptions {
            media_signing_key: "   ".into(),
            ..Default::default()
        })
        .unwrap()
        .shared();
        // 空白密钥由平台归一化成 32 字节随机值：装配成功，但那是"本次启动的
        // 随机密钥"，二进制入口在没配 env 时已经先拒绝启动了。
        assert_eq!(
            empty.options().media_signing_key.len(),
            MIN_SECRET_BYTES * 2
        );
        assert!(assemble(&empty, None).is_ok());
    }
}
