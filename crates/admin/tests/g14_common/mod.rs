//! G14 集成测试脚手架：真路由、真会话、真审计落库；只有**域侧端口**用 fixture
//! （上游卡 G02-05/G05-05/G07-07/G08-04 未合入，任务单允许并行开发期用 fixture，
//! 但最终 ACCEPTED 必须换真实域——见 `docs/g14-delivery.md` 的"未结项"一节）。
//!
//! 与 `tests/common` 的分工：那边管"平台库 + 管理员登录"，这边只加 G14 需要的
//! 账号矩阵与带 6 位动态码的 step-up 流程。

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::Value;

use wxlike_admin::commerce::FixtureMoneyPort;
use wxlike_admin::identity::FixturePort;
use wxlike_admin::publishing::FixtureContentPort;
use wxlike_admin::rbac::Role;
use wxlike_admin::{AccountStore, App, PlatformRead, Store};

use crate::common;

/// G14 需要的全部角色各一人。密码统一，TOTP 全部绑定——因为敏感动作要求
/// step-up，没绑 TOTP 的账号根本测不到那条路径。
pub const ROLES: &[(&str, Role)] = &[
    ("root", Role::Superadmin),
    ("mod", Role::Moderator),
    ("pub", Role::Publisher),
    ("fin", Role::Finance),
    ("view", Role::Viewer),
];

pub struct Env {
    pub dir: common::TempDir,
    pub app: Arc<App>,
    pub identity: Arc<FixturePort>,
    pub content: Arc<FixtureContentPort>,
    pub money: Arc<FixtureMoneyPort>,
    secrets: HashMap<String, String>,
    tokens: HashMap<String, String>,
}

impl Env {
    pub fn new(tag: &str) -> Self {
        let dir = common::TempDir::create(tag);
        let platform_db = dir.join("wxlike.db");
        // 平台库用真迁移建出来：G14 的读面不走它，但 App 需要一个真的只读连接。
        let _ = common::platform(&platform_db);
        let accounts_db = dir.join("adminx.db");
        let accounts = AccountStore::open(&accounts_db).unwrap();
        let store = Store::open(&accounts_db).unwrap();
        let cfg = common::cfg(
            platform_db.clone(),
            accounts_db.clone(),
            "http://127.0.0.1:1".into(),
        );

        let identity = Arc::new(FixturePort::new());
        let content = Arc::new(FixtureContentPort::new());
        let money = Arc::new(FixtureMoneyPort::new());
        let app = App::assemble_with_ports(
            PlatformRead::open(&platform_db).unwrap(),
            accounts,
            store,
            cfg,
            identity.clone(),
            content.clone(),
            money.clone(),
        );

        let mut secrets = HashMap::new();
        for (name, role) in ROLES {
            app.accounts().add(name, common::PW, *role).unwrap();
            let secret = app.accounts().totp_enroll(name).unwrap();
            let code = common::current_totp_code(&secret);
            app.accounts().totp_confirm(name, &code).unwrap();
            secrets.insert((*name).to_string(), secret);
        }

        Self {
            dir,
            app,
            identity,
            content,
            money,
            secrets,
            tokens: HashMap::new(),
        }
    }

    pub fn accounts_db(&self) -> PathBuf {
        self.dir.join("adminx.db")
    }

    pub fn code(&self, name: &str) -> String {
        common::current_totp_code(self.secrets.get(name).expect("账号已初始化"))
    }

    /// 登录拿到 `mfa=true` 的会话令牌（走真登录，不是塞一条会话进库）。
    pub fn token(&mut self, name: &str) -> String {
        if let Some(t) = self.tokens.get(name) {
            return t.clone();
        }
        let code = self.code(name);
        let (session, token) = self
            .app
            .accounts()
            .login(
                name,
                common::PW,
                Some(&code),
                self.app.config().session_ttl_ms,
                "",
            )
            .unwrap();
        assert!(session.mfa, "{name} 的会话必须 mfa=true");
        self.tokens.insert(name.to_string(), token.clone());
        token
    }

    pub fn router(&self) -> Router {
        self.app.router()
    }

    pub async fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let r: Request<Body> = common::req(method, path, body, token);
        common::call(&self.router(), r).await
    }

    /// 完成一次步进验证（敏感动作的前置条件）。
    pub async fn step_up(&mut self, name: &str) {
        let token = self.token(name);
        let code = self.code(name);
        let (status, body) = self
            .call(
                "POST",
                "/api/g14/session/stepup",
                Some(&token),
                Some(serde_json::json!({ "code": code })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "step-up 失败: {body}");
    }

    /// 一次性完成"登录 + 步进验证"，返回令牌。
    pub async fn ready(&mut self, name: &str) -> String {
        let token = self.token(name);
        self.step_up(name).await;
        token
    }
}

/// 断言状态码并返回 body，失败时把 body 打出来——否则调试只有一句 "assert failed"。
pub fn expect(status: StatusCode, want: StatusCode, body: &Value) -> Value {
    assert_eq!(status, want, "实际响应: {body}");
    body.clone()
}
