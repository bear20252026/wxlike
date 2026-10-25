//! G17-03 的"全覆盖"底座：把授权矩阵里的**每一条私有端点**都打一遍未认证请求。
//!
//! 为什么需要它：单点用例写得再好，也只覆盖写用例的人想到的地方。矩阵是从源码
//! 扫出来的（`python -m sourceaudit authz sync`），所以这里的覆盖随源码增长，
//! 新加一个 `/api/...` 而忘了鉴权，这条测试会红。
//!
//! 期望的形状很窄：**401**（用户态）/ **401 或 403**（管理面，缺 `x-admin-token`）。
//! 200 是漏鉴权，404 是路由没挂或参数先行，500 是校验抛错而不是拒绝——都不是"拒绝"。
//!
//! 已知缺口（`security/release-reviews/blockers.json` 的 SEC-AUTHZ-012）不在本 sweep
//! 的断言范围内：那三个信箱端点**有**鉴权（401 通过），缺的是"参与者归属"校验，
//! 由 `authorization.rs` 的 AUTHZ-012 独立验证参与者归属（#230 已修复）。

mod common;

use axum::http::StatusCode;
use serde_json::{json, Value};

const MATRIX: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../security/threat-model/authz-matrix.json"
));

/// 路径参数占位。用固定值而不是随机值，失败信息才可复现。
fn concretize(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 16);
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}').expect("路径参数必须闭合") + open;
        let param = &rest[open + 1..close];
        // `conv` 的形状决定 owner 类型（d- 前缀是直聊），给个合法直聊 id
        out.push_str(if param == "conv" {
            "d-alice~bob"
        } else {
            "id0"
        });
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

fn is_bodyless(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "DELETE")
}

#[tokio::test]
async fn unauth_sweep_rejects_every_private_route() {
    let app = common::app(common::platform());
    let matrix: Value = serde_json::from_str(MATRIX).expect("authz-matrix.json 必须可解析");
    let rows = matrix["routes"].as_array().expect("routes 必须是数组");

    let mut checked = 0usize;
    let mut failures = Vec::new();
    for row in rows {
        let auth = row["auth"].as_str().unwrap_or_default();
        if auth == "public" {
            continue;
        }
        let method = row["method"].as_str().expect("method");
        let path = concretize(row["path"].as_str().expect("path"));
        let body = if is_bodyless(method) {
            None
        } else {
            Some(json!({}))
        };
        let (status, resp) = common::call(&app, common::req(method, &path, body, None)).await;

        // 已登记偏差（matrix 里的 `exempt`，必须有 issue）：只要求"未认证拿不到数据"，
        // 允许 422 这类"鉴权顺序不对"的形状。条目本身由 `sourceaudit authz check`
        // 强制带 reason + issue，不能用来悄悄放水。
        if row.get("exempt").is_some() {
            if status.is_success() {
                failures.push(format!("{method} {path}（exempt）→ 200：未认证居然成功了"));
            }
            checked += 1;
            continue;
        }

        let denied = match auth {
            // 管理面缺令牌：401（先被要求登录）或 403（登录不适用于管理面）都算正确拒绝
            "admin" => matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
            _ => status == StatusCode::UNAUTHORIZED,
        };
        if !denied {
            failures.push(format!("{method} {path}（{auth}）→ {status} {resp}"));
        }
        checked += 1;
    }

    assert!(
        failures.is_empty(),
        "以下私有端点在未认证时没有拒绝（应为 401/403）：\n{}",
        failures.join("\n")
    );
    assert!(
        checked >= 100,
        "只扫到 {checked} 条私有端点：矩阵可能没生成，或路径前缀变了"
    );
}

/// 反面对照：公开端点不要求 `Authorization` 头。
///
/// `POST /api/login` 用错密码当然返回 401——那是**凭据错误**，不是"缺少会话"。
/// 所以分开断言：登录只看错误语义，其余公开面看"不是 401"。
#[tokio::test]
async fn public_routes_do_not_demand_a_session() {
    let app = common::app(common::platform());

    for (method, path, body) in [
        ("GET", "/api/profile/id0", None),
        ("POST", "/api/captcha", Some(json!({"purpose": "login"}))),
    ] {
        let (status, resp) = common::call(&app, common::req(method, path, body, None)).await;
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} 被矩阵标为公开却要求登录：{status} {resp}"
        );
    }

    let (status, resp) = common::call(
        &app,
        common::req(
            "POST",
            "/api/login",
            Some(json!({"name": "nobody", "password": "pw-123456"})),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "未知账号应当是凭据错误：{resp}"
    );
    let text = resp.to_string();
    assert!(
        text.contains("credentials") || text.contains("密码") || text.contains("账号"),
        "登录失败的错误语义应当指向凭据而不是「未登录」：{resp}"
    );
}
