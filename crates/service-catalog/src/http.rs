//! HTTP projection of the service-catalog domain (G07 / D6).
//!
//! The router is intentionally dumb: it authenticates, parses, calls one
//! [`OrderService`] method and renders the shared error shape. Ownership,
//! idempotency, quote TTL and the order state machine all live in the service,
//! so a second transport cannot bypass them.
//!
//! Session verification is owned by the composition root (G02): it injects a
//! [`SessionVerifier`] backed by the platform `sessions` table — the same one
//! `Authed` reads. This crate never trusts a user id from the request body.
//!
//! Shape projection: the wire contract (`apps/desktop-web/src/features/discover`)
//! differs from the internal model in a few places, mapped here once:
//!   * `ServiceCard` → `ServiceEntry` (target kind by service type);
//!   * `Quote.amount_minor` → `price_minor`, `expires_at_ms` → `expires_at`;
//!   * `Order` → `ServiceOrder` with the state vocabulary aligned
//!     (paid→paying, fulfilling→processing, cancelling/cancelled→failed).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::catalog::{Catalog, ServiceCard, ServiceType};
use crate::error::{ServiceError, ServiceErrorCode as Code};
use crate::order::{CreateOrderRequest, Order, OrderService, OrderState};
use crate::quote::Quote;

/// Session verification port: bearer token -> trusted actor id. The composition
/// root injects the real platform-backed implementation; tests inject a fixture.
pub use wxlike_http_auth::{FnVerifier, SessionVerifier};

pub struct AppState {
    pub catalog: Arc<Catalog>,
    pub orders: Arc<OrderService>,
    pub verifier: Arc<dyn SessionVerifier>,
    /// 默认区域：目录按区域开关投影，组合根配置一个服务区域。
    pub region: String,
}

pub struct CatalogApp {
    state: Arc<AppState>,
}

impl CatalogApp {
    pub fn new(state: AppState) -> Self {
        Self {
            state: Arc::new(state),
        }
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/api/services", get(list_services))
            .route("/api/quotes", get(get_quote))
            .route("/api/orders", post(create_order).get(list_orders))
            .route("/api/orders/{id}", get(get_order))
            .route("/api/orders/{id}/refund", post(refund_order))
            .with_state(self.state)
    }
}

/// 从请求头换取可信 actor；没带/无效凭据一律 401，绝不相信 body 里的用户 id。
fn actor(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    state
        .verifier
        .verify(headers)
        .ok_or_else(|| ApiError::from(ServiceError::new(Code::Unauthorized, "missing session")))
}

/// 服务错误 → HTTP 状态 + 共享 `{"error": …}` 体。BLOCKED_EXTERNAL 归 501：
/// 没有供应商凭据/区域未开通是"上游未开通"，不是参数错，界面据此显示阻塞横幅。
struct ApiError {
    status: StatusCode,
    message: String,
}

impl From<ServiceError> for ApiError {
    fn from(err: ServiceError) -> Self {
        let status = match err.code() {
            Code::InvalidArgument => StatusCode::BAD_REQUEST,
            Code::Unauthorized => StatusCode::UNAUTHORIZED,
            Code::NotFound => StatusCode::NOT_FOUND,
            Code::Conflict | Code::PaymentDeclined => StatusCode::CONFLICT,
            Code::Expired => StatusCode::GONE,
            Code::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Code::Cancelled => StatusCode::CONFLICT,
            Code::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Code::DependencyUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Code::BlockedExternal => StatusCode::NOT_IMPLEMENTED,
            Code::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: err.detail().to_string(),
        }
    }
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

// ---------------------------------------------------------------------------
// 投影：内部模型 → 前端契约形状
// ---------------------------------------------------------------------------

/// 服务卡 → 服务格子。target 按服务类型映射到前端五态之一；这里都是报价/下单类。
fn service_entry(card: &ServiceCard) -> Value {
    json!({
        "service_id": card.service_id,
        "name": card.title,
        "category": card.kind.as_str(),
        "target": { "kind": "quote" },
    })
}

/// 报价 → 前端 Quote（amount_minor→price_minor、expires_at_ms→expires_at）。
fn quote_view(quote: &Quote) -> Value {
    json!({
        "quote_id": quote.quote_id,
        "service_id": quote.service_id,
        "price_minor": quote.amount_minor,
        "currency": quote.currency,
        "expires_at": quote.expires_at_ms,
    })
}

/// 订单状态词表对齐：前端八态，内部十态。paid→paying、fulfilling→processing、
/// cancelling/cancelled→failed（取消即未完成的失败终态）。不猜未知态。
fn order_state(state: OrderState) -> &'static str {
    match state {
        OrderState::Created => "created",
        OrderState::Paid => "paying",
        OrderState::Fulfilling => "processing",
        OrderState::Fulfilled => "fulfilled",
        OrderState::Cancelling | OrderState::Cancelled | OrderState::Failed => "failed",
        OrderState::Refunding => "refunding",
        OrderState::Refunded => "refunded",
        OrderState::PendingReconciliation => "pending_reconciliation",
    }
}

/// 订单 → 前端 ServiceOrder。
fn order_view(order: &Order) -> Value {
    json!({
        "order_id": order.order_id,
        "service_id": order.service_id,
        "quote_id": order.quote_id,
        "state": order_state(order.state),
        "provider_ref": order.provider_order_id,
        "created_at": order.created_at_ms,
        "updated_at": order.updated_at_ms,
    })
}

// ---------------------------------------------------------------------------
// 处理器
// ---------------------------------------------------------------------------

/// 服务目录：遍历全部服务类型，按默认区域投影启用且区域可用的卡片。
/// 空目录就是空数组（管理后台未配置），不拿静态假数据填充。
async fn list_services(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Value> {
    actor(&state, &headers)?;
    let mut entries = Vec::new();
    for kind in ServiceType::ALL {
        for card in state.catalog.list(kind, &state.region) {
            entries.push(service_entry(&card));
        }
    }
    entries.sort_by(|a, b| {
        a["service_id"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["service_id"].as_str().unwrap_or_default())
    });
    Ok(Json(json!({ "items": entries })))
}

#[derive(Deserialize)]
struct QuoteQuery {
    service_id: String,
}

/// 报价：对某服务取一份当前报价（数量 1、空参数）。供应商无凭据/区域未开通时
/// 服务层如实报 BLOCKED_EXTERNAL → 501，不伪造价格。
async fn get_quote(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<QuoteQuery>,
) -> ApiResult<Value> {
    let session = bearer(&headers)?;
    let quote = state
        .orders
        .quote(&session, &query.service_id, 1, json!({}))
        .await
        .map_err(ApiError::from)?;
    Ok(Json(quote_view(&quote)))
}

#[derive(Deserialize)]
struct CreateBody {
    service_id: String,
    quote_id: String,
}

/// 下单：先取报价补 confirmed_amount_minor（服务端校验金额一致），再创建。
/// 幂等键从 Idempotency-Key 头取；缺省由服务层拒绝（不替调用方编键）。
async fn create_order(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> ApiResult<Value> {
    let session = bearer(&headers)?;
    let idem = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let quote = state
        .orders
        .get_quote(&session, &body.quote_id)
        .await
        .map_err(ApiError::from)?;
    if quote.service_id != body.service_id {
        return Err(ApiError::from(ServiceError::new(
            Code::InvalidArgument,
            "service/quote mismatch",
        )));
    }
    let request = CreateOrderRequest {
        quote_id: body.quote_id,
        idempotency_key: idem,
        confirmed_amount_minor: quote.amount_minor,
        params: quote.params.clone(),
    };
    let order = state
        .orders
        .create(&session, request)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(order_view(&order)))
}

async fn get_order(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    let session = bearer(&headers)?;
    let order = state
        .orders
        .order(&session, &id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(order_view(&order)))
}

async fn list_orders(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult<Value> {
    let session = bearer(&headers)?;
    let orders = state
        .orders
        .list_orders(&session)
        .await
        .map_err(ApiError::from)?;
    let items: Vec<Value> = orders.iter().map(order_view).collect();
    Ok(Json(json!({ "items": items })))
}

async fn refund_order(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    let session = bearer(&headers)?;
    let order = state
        .orders
        .refund(&session, &id, "user_requested")
        .await
        .map_err(ApiError::from)?;
    Ok(Json(order_view(&order)))
}

/// 取原始 bearer token（服务层自己再鉴权一次换 actor）。解析复用平台口径：
/// scheme 大小写不敏感、空白容忍；这里只负责把 token 字符串交给服务层。
fn bearer(headers: &HeaderMap) -> Result<String, ApiError> {
    let raw = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::from(ServiceError::new(Code::Unauthorized, "missing session")))?;
    let token = wxlike_bearer_parse(raw)
        .ok_or_else(|| ApiError::from(ServiceError::new(Code::Unauthorized, "malformed bearer")))?;
    Ok(token)
}

/// 极简 bearer 解析（与平台 `Bearer::parse` 同口径：scheme 不区分大小写）。
/// 不引平台依赖，避免特性域反向耦合组合根；语义一致即可。
fn wxlike_bearer_parse(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let (scheme, token) = trimmed.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}
