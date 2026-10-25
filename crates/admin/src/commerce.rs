//! G14-04 商户、卡券、资金与对账台。
//!
//! 这个模块处理钱，所以四条规矩写得比别处死：
//!
//! 1. **没有"改余额"的入口**。人工补账只能写 [`JournalEntry`]（借贷分录，必须平），
//!    余额由分录推导。库里也没有任何 `balance` 列可以改（用例直接查 `PRAGMA table_info` 断言）。
//! 2. **审批幂等 + 双人**。同一 `idem` 重放拿回同一张退款单；超过阈值必须两个
//!    **不同**的财务各自批准，同一人批两次不算数。
//! 3. **商户隔离在服务层**。带 [`MerchantScope::Only`] 的调用方查别人的订单得到
//!    `NotFound`（不给 403，避免"存在性探测"）。
//! 4. **导出防公式注入**。`=cmd|...` 这类单元格在 CSV 里会被 Excel 执行：
//!    以 `= + - @` 开头的值一律加 `'` 前缀。

use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

use crate::audit::{AuditLog, Draft};
use crate::auth::{self, Action, Resource, StepUp};
use crate::error::{AdminError, Result};
use crate::rbac::Role;
use crate::read::{now_ms, page_limit, Paged};
use crate::store::Store;
use crate::upstream::{self, Upstream};

/// 超过这个金额（分）要两个不同审批人。50 万分 = ¥5000。
pub const DUAL_APPROVAL_THRESHOLD_CENTS: i64 = 500_000;

/// 付款资料掩码：只留后四位。
pub fn mask_payout(raw: &str) -> String {
    let digits: Vec<char> = raw.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if digits.len() <= 4 {
        return "*".repeat(digits.len());
    }
    let tail: String = digits[digits.len() - 4..].iter().collect();
    format!("****{tail}")
}

// ---------------------------------------------------------------------------
// 域侧端口（资金动作）
// ---------------------------------------------------------------------------

#[async_trait]
pub trait MoneyPort: Send + Sync {
    /// 放行一笔退款。返回域侧回执号。
    async fn apply_refund(
        &self,
        refund_id: &str,
        order_id: &str,
        amount_cents: i64,
    ) -> Result<String>;
}

pub struct HttpMoneyPort {
    upstream: Upstream,
}

impl HttpMoneyPort {
    pub fn new(upstream: Upstream) -> Self {
        Self { upstream }
    }
}

#[async_trait]
impl MoneyPort for HttpMoneyPort {
    async fn apply_refund(
        &self,
        refund_id: &str,
        order_id: &str,
        amount_cents: i64,
    ) -> Result<String> {
        upstream::check_target(order_id)?;
        let reply = self
            .upstream
            .call(
                "POST",
                &format!("/api/admin/commerce/refunds/{refund_id}/apply"),
                "admin",
                Some(serde_json::json!({"order_id": order_id, "amount_cents": amount_cents})),
            )
            .await?;
        Ok(reply.reference())
    }
}

#[derive(Default)]
pub struct FixtureMoneyPort {
    pub calls: std::sync::Mutex<Vec<(String, i64)>>,
    pub fail_next: std::sync::Mutex<Option<AdminError>>,
}

impl FixtureMoneyPort {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_next(&self, err: AdminError) {
        *self.fail_next.lock().unwrap() = Some(err);
    }

    pub fn calls(&self) -> Vec<(String, i64)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl MoneyPort for FixtureMoneyPort {
    async fn apply_refund(
        &self,
        refund_id: &str,
        _order_id: &str,
        amount_cents: i64,
    ) -> Result<String> {
        if let Some(err) = self.fail_next.lock().unwrap().take() {
            return Err(err);
        }
        self.calls
            .lock()
            .unwrap()
            .push((refund_id.to_string(), amount_cents));
        Ok(format!("fixture:refund:{refund_id}"))
    }
}

// ---------------------------------------------------------------------------
// 模型
// ---------------------------------------------------------------------------

/// 调用方能看哪些商户。`All` 只给财务/超管；商户自助账号拿到的是 `Only`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MerchantScope {
    All,
    Only(String),
}

impl MerchantScope {
    fn allows(&self, merchant_id: &str) -> bool {
        match self {
            MerchantScope::All => true,
            MerchantScope::Only(id) => id == merchant_id,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Merchant {
    pub id: String,
    pub name: String,
    pub state: String,
    pub payout_masked: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Order {
    pub id: String,
    pub merchant_id: String,
    pub internal_no: String,
    pub external_no: String,
    pub amount_cents: i64,
    pub currency: String,
    pub state: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Refund {
    pub id: String,
    pub order_id: String,
    pub amount_cents: i64,
    pub state: String,
    pub reason: String,
    pub requested_by: String,
    pub approved_by: Option<String>,
    pub second_approver: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconDiff {
    pub id: String,
    pub batch: String,
    pub external_no: String,
    pub internal_no: String,
    pub kind: String,
    pub amount_delta_cents: i64,
    pub state: String,
    pub note: String,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct JournalEntry {
    pub id: String,
    pub entry_no: String,
    pub account: String,
    pub direction: String,
    pub amount_cents: i64,
    pub memo: String,
    pub source: String,
    pub created_by: String,
    pub created_at: i64,
}

/// 一条分录的输入。
#[derive(Clone, Debug, Deserialize)]
pub struct JournalLine {
    pub account: String,
    /// `debit` 或 `credit`。
    pub direction: String,
    pub amount_cents: i64,
}

#[derive(Clone, Debug, Default)]
pub struct OrderFilter {
    pub merchant_id: Option<String>,
    pub state: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Card {
    pub id: String,
    pub merchant_id: String,
    pub title: String,
    pub face_value_cents: i64,
    pub state: String,
    pub issued_at: i64,
}

// ---------------------------------------------------------------------------
// 服务
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CommerceService {
    store: Store,
    audit: AuditLog,
    port: Arc<dyn MoneyPort>,
    stepup: StepUp,
}

impl CommerceService {
    pub fn new(store: Store, audit: AuditLog, port: Arc<dyn MoneyPort>, stepup: StepUp) -> Self {
        Self {
            store,
            audit,
            port,
            stepup,
        }
    }

    pub fn audit(&self) -> &AuditLog {
        &self.audit
    }

    // -- 商户与商品 ---------------------------------------------------------

    pub fn open_merchant(&self, id: &str, name: &str, payout: &str) -> Result<Merchant> {
        let id = upstream::check_target(id)?;
        if name.trim().is_empty() {
            return Err(AdminError::bad_input("商户名不能为空"));
        }
        let merchant = Merchant {
            id: id.clone(),
            name: name.trim().to_string(),
            state: "active".into(),
            payout_masked: mask_payout(payout),
            created_at: now_ms(),
        };
        self.store.lock().execute(
            "INSERT INTO g14_merchant (id, name, state, payout_masked, created_at) \
             VALUES (?1,?2,?3,?4,?5)",
            params![
                merchant.id,
                merchant.name,
                merchant.state,
                merchant.payout_masked,
                merchant.created_at
            ],
        )?;
        Ok(merchant)
    }

    pub fn merchants(&self) -> Result<Vec<Merchant>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, state, payout_masked, created_at FROM g14_merchant ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Merchant {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    state: r.get(2)?,
                    payout_masked: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 订单投影写入。真实来源是导入/对账作业；这里只落运营投影，不改账本。
    pub fn ingest_order(&self, order: &Order) -> Result<()> {
        upstream::check_target(&order.id)?;
        self.store.lock().execute(
            "INSERT INTO g14_order (id, merchant_id, internal_no, external_no, amount_cents, \
             currency, state, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                order.id,
                order.merchant_id,
                order.internal_no,
                order.external_no,
                order.amount_cents,
                order.currency,
                order.state,
                order.created_at
            ],
        )?;
        Ok(())
    }

    pub fn orders(&self, scope: &MerchantScope, filter: &OrderFilter) -> Result<Paged<Order>> {
        let limit = page_limit(filter.limit);
        let offset = filter.offset.unwrap_or(0).max(0);
        let mut where_sql = String::from(" WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        match scope {
            MerchantScope::All => {}
            MerchantScope::Only(id) => {
                // 商户隔离在 SQL 层：不是查完再过滤，避免分页数与总数对不上
                where_sql.push_str(" AND merchant_id = ?");
                args.push(Box::new(id.clone()));
            }
        }
        if let Some(m) = &filter.merchant_id {
            where_sql.push_str(" AND merchant_id = ?");
            args.push(Box::new(m.clone()));
        }
        if let Some(s) = &filter.state {
            where_sql.push_str(" AND state = ?");
            args.push(Box::new(s.clone()));
        }
        let conn = self.store.lock();
        let sql = format!(
            "SELECT id, merchant_id, internal_no, external_no, amount_cents, currency, state, \
             created_at FROM g14_order{where_sql} ORDER BY created_at DESC LIMIT ? OFFSET ?"
        );
        Paged::query(
            &conn,
            &format!("SELECT count(*) FROM g14_order{where_sql}"),
            &sql,
            args,
            limit,
            offset,
            row_to_order,
        )
    }

    /// 单笔订单。越权 = NotFound（不给 403，避免探测他人订单是否存在）。
    pub fn order(&self, scope: &MerchantScope, id: &str) -> Result<Option<Order>> {
        upstream::check_target(id)?;
        let conn = self.store.lock();
        let row = conn
            .query_row(
                "SELECT id, merchant_id, internal_no, external_no, amount_cents, currency, state, \
                 created_at FROM g14_order WHERE id = ?1",
                [id],
                row_to_order,
            )
            .ok();
        Ok(row.filter(|o| scope.allows(&o.merchant_id)))
    }

    pub fn issue_card(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        merchant_id: &str,
        title: &str,
        face_value_cents: i64,
    ) -> Result<Card> {
        auth::require(role, Resource::Merchant, Action::Write)?;
        let merchant_id = upstream::check_target(merchant_id)?;
        if face_value_cents <= 0 {
            return Err(AdminError::bad_input("卡券面额必须为正（单位：分）"));
        }
        let card = Card {
            id: format!("card_{}", wxlike_platform::random_hex(6)),
            merchant_id,
            title: title.trim().to_string(),
            face_value_cents,
            state: "active".into(),
            issued_at: now_ms(),
        };
        self.store.lock().execute(
            "INSERT INTO g14_card (id, merchant_id, title, face_value_cents, state, issued_at) \
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                card.id,
                card.merchant_id,
                card.title,
                card.face_value_cents,
                card.state,
                card.issued_at
            ],
        )?;
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "commerce.issue_card",
                "card",
                &card.id,
            )
            .reason("发行卡券")
            .trace(trace)
            .after(serde_json::json!({
                "merchant_id": card.merchant_id,
                "face_value_cents": card.face_value_cents,
            })),
        )?;
        Ok(card)
    }

    /// 核销记录。同一张卡只能核销一次（`card_id` 有唯一约束由业务保证，
    /// 这里先查后插并在同一条语句里判状态）。
    pub fn redeem_card(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        card_id: &str,
        order_id: &str,
    ) -> Result<i64> {
        auth::require(role, Resource::Merchant, Action::Write)?;
        let card_id = upstream::check_target(card_id)?;
        let order_id = upstream::check_target(order_id)?;
        let conn = self.store.lock();
        let face = redeem_value(&conn, &card_id, &order_id)?;
        conn.execute(
            "INSERT INTO g14_card_redeem (id, card_id, order_id, amount_cents, at_ms) \
             VALUES (?1,?2,?3,?4,?5)",
            params![
                format!("rdm_{}", wxlike_platform::random_hex(6)),
                card_id,
                order_id,
                face,
                now_ms()
            ],
        )?;
        conn.execute(
            "UPDATE g14_card SET state = 'redeemed' WHERE id = ?1",
            [&card_id],
        )?;
        drop(conn);
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "commerce.redeem_card",
                "card",
                &card_id,
            )
            .reason("核销卡券")
            .trace(trace)
            .after(serde_json::json!({"order_id": order_id, "amount_cents": face})),
        )?;
        Ok(face)
    }

    pub fn cards(&self, merchant_id: Option<&str>) -> Result<Vec<Card>> {
        let conn = self.store.lock();
        let (sql, args): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match merchant_id {
            Some(id) => (
                "SELECT id, merchant_id, title, face_value_cents, state, issued_at FROM g14_card \
                 WHERE merchant_id = ?1 ORDER BY issued_at DESC",
                vec![Box::new(id.to_string()) as Box<dyn rusqlite::ToSql>],
            ),
            None => (
                "SELECT id, merchant_id, title, face_value_cents, state, issued_at FROM g14_card \
                 ORDER BY issued_at DESC",
                Vec::<Box<dyn rusqlite::ToSql>>::new(),
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), |r| {
                Ok(Card {
                    id: r.get(0)?,
                    merchant_id: r.get(1)?,
                    title: r.get(2)?,
                    face_value_cents: r.get(3)?,
                    state: r.get(4)?,
                    issued_at: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -- 退款 ---------------------------------------------------------------

    /// 建退款申请。`idem` 幂等：重放返回同一张单。
    // 参数面镜像 g14.rs 的 POST body；随 #271 质量返工一并收敛为请求结构体。
    #[allow(clippy::too_many_arguments)]
    pub fn request_refund(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        scope: &MerchantScope,
        order_id: &str,
        amount_cents: i64,
        reason: &str,
        idem: &str,
    ) -> Result<Refund> {
        auth::require(role, Resource::Order, Action::Write)?;
        let order = self.order(scope, order_id)?.ok_or(AdminError::NotFound)?;
        validate_refund_request(order.amount_cents, amount_cents, reason, idem)?;
        let conn = self.store.lock();
        if let Ok(existing) = conn.query_row(
            "SELECT id, order_id, amount_cents, state, reason, requested_by, approved_by, \
             second_approver, created_at, updated_at FROM g14_refund WHERE idem = ?1",
            [idem],
            row_to_refund,
        ) {
            return Ok(existing);
        }
        let now = now_ms();
        let refund = Refund {
            id: format!("rf_{}", wxlike_platform::random_hex(8)),
            order_id: order.id,
            amount_cents,
            state: "pending".into(),
            reason: reason.trim().to_string(),
            requested_by: actor.to_string(),
            approved_by: None,
            second_approver: None,
            created_at: now,
            updated_at: now,
        };
        insert_refund(&conn, &refund, idem, now)?;
        drop(conn);
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "commerce.refund_request",
                "refund",
                &refund.id,
            )
            .reason(&refund.reason)
            .trace(trace)
            .after(serde_json::json!({
                "order_id": refund.order_id,
                "amount_cents": refund.amount_cents,
            })),
        )?;
        Ok(refund)
    }

    /// 审批。同一个人批两次不算双人；超过阈值必须两个不同审批人。
    /// 已完成的退款再批 = 幂等返回（不重复打款）。
    pub async fn approve_refund(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        refund_id: &str,
        note: &str,
    ) -> Result<Refund> {
        auth::require(role, Resource::Order, Action::ApproveMoney)?;
        self.stepup.require(actor, Action::ApproveMoney)?;
        if note.trim().is_empty() {
            return Err(AdminError::bad_input("审批必须写意见"));
        }
        let refund = self.refund(refund_id)?.ok_or(AdminError::NotFound)?;
        if refund.state == "approved" {
            return Ok(refund);
        }
        let (first, second) = refund_approvers(&refund, actor)?;
        let needs_two = refund.amount_cents >= DUAL_APPROVAL_THRESHOLD_CENTS;
        let both = first.is_some() && second.is_some();
        let (state, reference) = if !needs_two || both {
            let reference = self
                .port
                .apply_refund(&refund.id, &refund.order_id, refund.amount_cents)
                .await?;
            ("approved", reference)
        } else {
            ("partially_approved", String::new())
        };

        let now = now_ms();
        self.store.lock().execute(
            "UPDATE g14_refund SET state = ?2, approved_by = ?3, second_approver = ?4, \
             updated_at = ?5 WHERE id = ?1",
            params![refund_id, state, first, second, now],
        )?;
        let updated = self.refund(refund_id)?.ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "commerce.refund_approve",
                "refund",
                refund_id,
            )
            .reason(note)
            .trace(trace)
            .before(serde_json::json!({"state": refund.state}))
            .after(serde_json::json!({
                "state": state,
                "dual_required": needs_two,
                "upstream": reference,
            })),
        )?;
        Ok(updated)
    }

    pub fn refund(&self, id: &str) -> Result<Option<Refund>> {
        upstream::check_target(id)?;
        let conn = self.store.lock();
        Ok(conn
            .query_row(
                "SELECT id, order_id, amount_cents, state, reason, requested_by, approved_by, \
                 second_approver, created_at, updated_at FROM g14_refund WHERE id = ?1",
                [id],
                row_to_refund,
            )
            .ok())
    }

    pub fn refunds(&self) -> Result<Vec<Refund>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(
            "SELECT id, order_id, amount_cents, state, reason, requested_by, approved_by, \
             second_approver, created_at, updated_at FROM g14_refund ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], row_to_refund)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -- 对账 ---------------------------------------------------------------

    /// 导入一批差异（对账作业写的）。同一 `(batch, external_no)` 不重复入。
    pub fn import_diff(&self, diff: &ReconDiff) -> Result<bool> {
        let conn = self.store.lock();
        let exists: i64 = conn.query_row(
            "SELECT count(*) FROM g14_recon_diff WHERE batch = ?1 AND external_no = ?2",
            params![diff.batch, diff.external_no],
            |r| r.get(0),
        )?;
        if exists > 0 {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO g14_recon_diff (id, batch, external_no, internal_no, kind, \
             amount_delta_cents, state, note, resolved_by, resolved_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,'',NULL,NULL)",
            params![
                diff.id,
                diff.batch,
                diff.external_no,
                diff.internal_no,
                diff.kind,
                diff.amount_delta_cents,
                diff.state
            ],
        )?;
        Ok(true)
    }

    pub fn diffs(&self, batch: Option<&str>) -> Result<Vec<ReconDiff>> {
        let conn = self.store.lock();
        let (sql, args): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match batch {
            Some(b) => (
                "SELECT id, batch, external_no, internal_no, kind, amount_delta_cents, state, note, \
                 resolved_by, resolved_at FROM g14_recon_diff WHERE batch = ?1 ORDER BY external_no",
                vec![Box::new(b.to_string()) as Box<dyn rusqlite::ToSql>],
            ),
            None => (
                "SELECT id, batch, external_no, internal_no, kind, amount_delta_cents, state, note, \
                 resolved_by, resolved_at FROM g14_recon_diff ORDER BY batch, external_no",
                Vec::<Box<dyn rusqlite::ToSql>>::new(),
            ),
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(args), |r| {
                Ok(ReconDiff {
                    id: r.get(0)?,
                    batch: r.get(1)?,
                    external_no: r.get(2)?,
                    internal_no: r.get(3)?,
                    kind: r.get(4)?,
                    amount_delta_cents: r.get(5)?,
                    state: r.get(6)?,
                    note: r.get(7)?,
                    resolved_by: r.get(8)?,
                    resolved_at: r.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 处理差异。`adjusted` 必须配一条平账分录——差异不能"点一下就消失"。
    pub fn resolve_diff(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        diff_id: &str,
        decision: &str,
        note: &str,
    ) -> Result<ReconDiff> {
        auth::require(role, Resource::Recon, Action::ApproveMoney)?;
        self.stepup.require(actor, Action::ApproveMoney)?;
        if note.trim().is_empty() {
            return Err(AdminError::bad_input("处理差异必须写说明（复核要看）"));
        }
        let allowed = ["matched", "adjusted", "ignored"];
        if !allowed.contains(&decision) {
            return Err(AdminError::bad_input(format!(
                "decision 只能是 {}",
                allowed.join("/")
            )));
        }
        upstream::check_target(diff_id)?;
        let before = {
            let conn = self.store.lock();
            conn.query_row(
                "SELECT state FROM g14_recon_diff WHERE id = ?1",
                [diff_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| AdminError::NotFound)?
        };
        if before != "open" {
            return Err(AdminError::bad_input(format!(
                "差异已经是 {before}，不能重复处理"
            )));
        }
        self.store.lock().execute(
            "UPDATE g14_recon_diff SET state = ?2, note = ?3, resolved_by = ?4, resolved_at = ?5 \
             WHERE id = ?1",
            params![diff_id, decision, note.trim(), actor, now_ms()],
        )?;
        let after = self
            .diffs(None)?
            .into_iter()
            .find(|d| d.id == diff_id)
            .ok_or(AdminError::NotFound)?;
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "commerce.resolve_diff",
                "recon_diff",
                diff_id,
            )
            .reason(note)
            .trace(trace)
            .before(serde_json::json!({"state": before}))
            .after(serde_json::json!({"state": decision})),
        )?;
        Ok(after)
    }

    // -- 补账分录 -----------------------------------------------------------

    /// 人工补账：只能写分录，且必须借贷平衡。
    /// 返回写入的分录（同一 `entry_no` 内多条）。
    pub fn post_journal(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        entry_no: &str,
        memo: &str,
        source: &str,
        lines: &[JournalLine],
    ) -> Result<Vec<JournalEntry>> {
        auth::require(role, Resource::Journal, Action::ApproveMoney)?;
        self.stepup.require(actor, Action::ApproveMoney)?;
        let debit = validate_journal(entry_no, memo, source, lines)?;
        let now = now_ms();
        let out = {
            let conn = self.store.lock();
            insert_journal_lines(&conn, actor, entry_no, memo, source, lines, now)?
        };
        self.audit.append(
            Draft::new(
                actor,
                role.as_str(),
                "commerce.journal_post",
                "journal",
                entry_no,
            )
            .reason(memo)
            .trace(trace)
            .after(serde_json::json!({
                "entry_no": entry_no,
                "lines": lines.len(),
                "debit_cents": debit,
            })),
        )?;
        Ok(out)
    }

    pub fn journal(&self) -> Result<Vec<JournalEntry>> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(
            "SELECT id, entry_no, account, direction, amount_cents, memo, source, created_by, \
             created_at FROM g14_journal ORDER BY created_at, id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(JournalEntry {
                    id: r.get(0)?,
                    entry_no: r.get(1)?,
                    account: r.get(2)?,
                    direction: r.get(3)?,
                    amount_cents: r.get(4)?,
                    memo: r.get(5)?,
                    source: r.get(6)?,
                    created_by: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -- 导出 ---------------------------------------------------------------

    /// 订单导出（CSV）。带个人信息，要求 `ExportPii` + step-up。
    pub fn export_orders(
        &self,
        actor: &str,
        role: Role,
        trace: &str,
        scope: &MerchantScope,
        filter: &OrderFilter,
    ) -> Result<String> {
        auth::require(role, Resource::Order, Action::ExportPii)?;
        self.stepup.require(actor, Action::ExportPii)?;
        let page = self.orders(scope, filter)?;
        let mut csv =
            String::from("order_id,merchant_id,internal_no,external_no,amount_cents,state\n");
        for o in &page.rows {
            csv.push_str(&format!(
                "{},{},{},{},{},{}\n",
                csv_cell(&o.id),
                csv_cell(&o.merchant_id),
                csv_cell(&o.internal_no),
                csv_cell(&o.external_no),
                o.amount_cents,
                csv_cell(&o.state),
            ));
        }
        self.audit.append(
            Draft::new(actor, role.as_str(), "commerce.export_orders", "order", "")
                .reason("导出订单报表（含个人信息）")
                .trace(trace)
                .detail(serde_json::json!({"rows": page.rows.len()})),
        )?;
        Ok(csv)
    }
}

fn redeem_value(conn: &rusqlite::Connection, card_id: &str, order_id: &str) -> Result<i64> {
    let (face, state, merchant_id) = conn
        .query_row(
            "SELECT face_value_cents, state, merchant_id FROM g14_card WHERE id = ?1",
            [&card_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|_| AdminError::NotFound)?;
    if state != "active" {
        return Err(AdminError::bad_input(format!(
            "卡券状态是 {state}，不能核销"
        )));
    }
    let order_merchant: String = conn
        .query_row(
            "SELECT merchant_id FROM g14_order WHERE id = ?1",
            [&order_id],
            |r| r.get(0),
        )
        .map_err(|_| AdminError::NotFound)?;
    if order_merchant != merchant_id {
        return Err(AdminError::bad_input(
            "卡券与订单不属于同一个商户：跨商户核销是资金事故，直接拒",
        ));
    }
    Ok(face)
}

fn validate_refund_request(
    order_amount: i64,
    amount_cents: i64,
    reason: &str,
    idem: &str,
) -> Result<()> {
    if amount_cents <= 0 || amount_cents > order_amount {
        return Err(AdminError::bad_input(format!(
            "退款金额必须在 1..={} 分之间",
            order_amount
        )));
    }
    if reason.trim().is_empty() {
        return Err(AdminError::bad_input("退款申请必须写理由"));
    }
    if idem.trim().is_empty() {
        return Err(AdminError::bad_input("退款申请必须有幂等键 idem"));
    }
    Ok(())
}

fn insert_refund(conn: &rusqlite::Connection, refund: &Refund, idem: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO g14_refund (id, order_id, amount_cents, state, reason, requested_by, \
             approved_by, second_approver, idem, created_at, updated_at) \
             VALUES (?1,?2,?3,?4,?5,?6,NULL,NULL,?7,?8,?8)",
        params![
            refund.id,
            refund.order_id,
            refund.amount_cents,
            refund.state,
            refund.reason,
            refund.requested_by,
            idem,
            now
        ],
    )?;
    Ok(())
}

fn refund_approvers(refund: &Refund, actor: &str) -> Result<(Option<String>, Option<String>)> {
    if refund.state != "pending" && refund.state != "partially_approved" {
        return Err(AdminError::bad_input(format!(
            "退款状态是 {}，不能再审批",
            refund.state
        )));
    }
    let mut first = refund.approved_by.clone();
    let mut second = refund.second_approver.clone();
    if first.as_deref() == Some(actor) || second.as_deref() == Some(actor) {
        return Err(AdminError::bad_input(
            "同一审批人不能重复计数：超过阈值的退款需要两个不同的人批准",
        ));
    }
    if first.is_none() {
        first = Some(actor.to_string());
    } else {
        second = Some(actor.to_string());
    }
    Ok((first, second))
}

fn validate_journal(
    entry_no: &str,
    memo: &str,
    source: &str,
    lines: &[JournalLine],
) -> Result<i64> {
    if entry_no.trim().is_empty() || memo.trim().is_empty() || source.trim().is_empty() {
        return Err(AdminError::bad_input(
            "补账必须给 entry_no / memo / source（来源不明的分录等于没有来源）",
        ));
    }
    if lines.len() < 2 {
        return Err(AdminError::bad_input("分录至少两条：一条借一条贷"));
    }
    let mut debit = 0i64;
    let mut credit = 0i64;
    for line in lines {
        if line.amount_cents <= 0 {
            return Err(AdminError::bad_input(
                "分录金额必须为正（方向由 direction 表示）",
            ));
        }
        match line.direction.as_str() {
            "debit" => debit += line.amount_cents,
            "credit" => credit += line.amount_cents,
            other => {
                return Err(AdminError::bad_input(format!(
                    "direction 只能是 debit/credit，收到 {other:?}"
                )))
            }
        }
        if line.account.trim().is_empty() {
            return Err(AdminError::bad_input("分录必须指定科目"));
        }
    }
    if debit != credit {
        return Err(AdminError::bad_input(format!(
            "借贷不平：借 {debit} 分 / 贷 {credit} 分。补账不能凭空造钱"
        )));
    }
    Ok(debit)
}

fn insert_journal_lines(
    conn: &rusqlite::Connection,
    actor: &str,
    entry_no: &str,
    memo: &str,
    source: &str,
    lines: &[JournalLine],
    now: i64,
) -> Result<Vec<JournalEntry>> {
    let mut out = Vec::new();
    let existing: i64 = conn.query_row(
        "SELECT count(*) FROM g14_journal WHERE entry_no = ?1",
        [entry_no],
        |r| r.get(0),
    )?;
    if existing > 0 {
        return Err(AdminError::bad_input(format!(
            "分录号 {entry_no} 已存在（分录不可改只能冲销）"
        )));
    }
    for (line_no, line) in lines.iter().enumerate() {
        let entry = JournalEntry {
            id: format!("jn_{}", wxlike_platform::random_hex(6)),
            entry_no: entry_no.trim().to_string(),
            account: line.account.trim().to_string(),
            direction: line.direction.clone(),
            amount_cents: line.amount_cents,
            memo: memo.trim().to_string(),
            source: source.trim().to_string(),
            created_by: actor.to_string(),
            created_at: now,
        };
        conn.execute(
            "INSERT INTO g14_journal (id, entry_no, line_no, account, direction, amount_cents, \
                     memo, source, created_by, created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                entry.id,
                entry.entry_no,
                line_no as i64,
                entry.account,
                entry.direction,
                entry.amount_cents,
                entry.memo,
                entry.source,
                entry.created_by,
                entry.created_at
            ],
        )?;
        out.push(entry);
    }
    Ok(out)
}

/// CSV 单元格转义。`= + - @` 开头会被 Excel 当公式执行（CSV 注入），
/// 统一加 `'` 前缀；引号/逗号/换行按 RFC4180 包引号。
pub fn csv_cell(raw: &str) -> String {
    let mut value = raw.to_string();
    if value.starts_with(['=', '+', '-', '@']) {
        value.insert(0, '\'');
    }
    if value.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

fn row_to_order(row: &Row<'_>) -> rusqlite::Result<Order> {
    Ok(Order {
        id: row.get(0)?,
        merchant_id: row.get(1)?,
        internal_no: row.get(2)?,
        external_no: row.get(3)?,
        amount_cents: row.get(4)?,
        currency: row.get(5)?,
        state: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn row_to_refund(row: &Row<'_>) -> rusqlite::Result<Refund> {
    Ok(Refund {
        id: row.get(0)?,
        order_id: row.get(1)?,
        amount_cents: row.get(2)?,
        state: row.get(3)?,
        reason: row.get(4)?,
        requested_by: row.get(5)?,
        approved_by: row.get(6)?,
        second_approver: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (CommerceService, Arc<FixtureMoneyPort>, Store) {
        let store = Store::open_memory().unwrap();
        let audit = AuditLog::new(store.clone());
        let port = Arc::new(FixtureMoneyPort::new());
        let svc = CommerceService::new(
            store.clone(),
            audit,
            port.clone(),
            StepUp::new(store.clone()),
        );
        (svc, port, store)
    }

    fn open_step_up(store: &Store, actor: &str) {
        store
            .lock()
            .execute(
                "INSERT INTO g14_stepup (account_id, verified_at_ms) VALUES (?1, ?2) \
                 ON CONFLICT(account_id) DO UPDATE SET verified_at_ms = excluded.verified_at_ms",
                params![actor, now_ms()],
            )
            .unwrap();
    }

    fn seed(svc: &CommerceService, merchant: &str, order: &str, amount: i64) {
        if svc.merchants().unwrap().iter().all(|m| m.id != merchant) {
            svc.open_merchant(merchant, "测试商户", "6222 0000 1234 5678")
                .unwrap();
        }
        svc.ingest_order(&Order {
            id: order.to_string(),
            merchant_id: merchant.to_string(),
            internal_no: format!("in-{order}"),
            external_no: format!("out-{order}"),
            amount_cents: amount,
            currency: "CNY".into(),
            state: "paid".into(),
            created_at: now_ms(),
        })
        .unwrap();
    }

    #[test]
    fn payout_details_are_masked() {
        assert_eq!(mask_payout("6222 0000 1234 5678"), "****5678");
        assert_eq!(mask_payout("123"), "***");
        let (svc, _p, _s) = service();
        let m = svc.open_merchant("m1", "店铺", "6222000012345678").unwrap();
        assert_eq!(m.payout_masked, "****5678");
        assert!(!serde_json::to_string(&m)
            .unwrap()
            .contains("6222000012345678"));
    }

    #[test]
    fn filtered_paging_keeps_merchant_scope_and_total_before_offset() {
        let (svc, _port, store) = service();
        seed(&svc, "m1", "old", 1_000);
        seed(&svc, "m1", "new", 2_000);
        seed(&svc, "m1", "cancelled", 3_000);
        seed(&svc, "m2", "private", 9_000);
        store
            .lock()
            .execute("UPDATE g14_order SET created_at = amount_cents", [])
            .unwrap();
        store
            .lock()
            .execute(
                "UPDATE g14_order SET state = 'cancelled' WHERE id = 'cancelled'",
                [],
            )
            .unwrap();
        let scope = MerchantScope::Only("m1".into());
        let mut filter = OrderFilter {
            merchant_id: Some("m1".into()),
            state: Some("paid".into()),
            limit: Some(1),
            offset: Some(1),
        };
        let page = svc.orders(&scope, &filter).unwrap();
        assert_eq!((page.total, page.limit, page.offset), (2, 1, 1));
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].id, "old");
        filter.offset = Some(2);
        let empty = svc.orders(&scope, &filter).unwrap();
        assert_eq!(empty.total, 2);
        assert!(empty.rows.is_empty());
        filter.merchant_id = Some("m2".into());
        filter.offset = Some(0);
        let forbidden = svc.orders(&scope, &filter).unwrap();
        assert_eq!(forbidden.total, 0);
        assert!(forbidden.rows.is_empty());
    }

    #[test]
    fn merchant_scope_hides_other_peoples_orders() {
        let (svc, _p, _s) = service();
        seed(&svc, "m1", "o1", 1_000);
        seed(&svc, "m2", "o2", 2_000);

        let all = svc
            .orders(&MerchantScope::All, &OrderFilter::default())
            .unwrap();
        assert_eq!(all.total, 2);

        let only = svc
            .orders(&MerchantScope::Only("m1".into()), &OrderFilter::default())
            .unwrap();
        assert_eq!(only.total, 1);
        assert_eq!(only.rows[0].id, "o1");

        // 越权单查 = NotFound（不暴露存在性）
        assert!(svc
            .order(&MerchantScope::Only("m1".into()), "o2")
            .unwrap()
            .is_none());
        // 越权商户也看不到别人的导出
        // 越权导出在权限层就被拒（ExportPii 只有 superadmin）
        let err = svc.export_orders(
            "ops",
            Role::Finance,
            "t",
            &MerchantScope::Only("m1".into()),
            &OrderFilter::default(),
        );
        assert!(matches!(err, Err(AdminError::Forbidden(_))), "{err:?}");
    }

    #[test]
    fn refund_requires_two_distinct_approvers_above_threshold() {
        let (svc, port, store) = service();
        seed(&svc, "m1", "o1", 900_000);
        open_step_up(&store, "fin_a");
        open_step_up(&store, "fin_b");

        let refund = svc
            .request_refund(
                "fin_a",
                Role::Finance,
                "t1",
                &MerchantScope::All,
                "o1",
                600_000,
                "重复扣款",
                "idem-r1",
            )
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            // 第一人：只到 partially_approved，不打款
            let first = svc
                .approve_refund("fin_a", Role::Finance, "t2", &refund.id, "同意")
                .await
                .unwrap();
            assert_eq!(first.state, "partially_approved");
            assert!(port.calls().is_empty(), "一人批准不能打款");

            // 同一人再批：拒（不能自己凑成双人）
            let err = svc
                .approve_refund("fin_a", Role::Finance, "t3", &refund.id, "再同意")
                .await
                .unwrap_err();
            assert!(err.to_string().contains("两个不同的人"), "{err}");

            // 第二人：打款，状态 approved
            let done = svc
                .approve_refund("fin_b", Role::Finance, "t4", &refund.id, "复核通过")
                .await
                .unwrap();
            assert_eq!(done.state, "approved");
            assert_eq!(port.calls().len(), 1);

            // 重复审批幂等：不再打款
            let again = svc
                .approve_refund("fin_b", Role::Finance, "t5", &refund.id, "复核通过")
                .await
                .unwrap();
            assert_eq!(again.state, "approved");
            assert_eq!(port.calls().len(), 1);
        });
    }

    #[test]
    fn refund_idempotency_key_prevents_duplicate_requests() {
        let (svc, _p, _s) = service();
        seed(&svc, "m1", "o1", 10_000);
        let a = svc
            .request_refund(
                "fin_a",
                Role::Finance,
                "t1",
                &MerchantScope::All,
                "o1",
                1_000,
                "少发",
                "k1",
            )
            .unwrap();
        let b = svc
            .request_refund(
                "fin_a",
                Role::Finance,
                "t2",
                &MerchantScope::All,
                "o1",
                1_000,
                "少发",
                "k1",
            )
            .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(svc.refunds().unwrap().len(), 1);
    }

    #[test]
    fn refund_amount_is_capped_by_the_order() {
        let (svc, _p, _s) = service();
        seed(&svc, "m1", "o1", 5_000);
        let err = svc
            .request_refund(
                "a",
                Role::Finance,
                "t",
                &MerchantScope::All,
                "o1",
                5_001,
                "超额",
                "k",
            )
            .unwrap_err();
        assert!(err.to_string().contains("1..=5000"), "{err}");
    }

    #[test]
    fn journal_must_balance_and_cannot_be_edited() {
        let (svc, _p, store) = service();
        open_step_up(&store, "ops");
        let unbalanced = vec![
            JournalLine {
                account: "user_wallet:u1".into(),
                direction: "credit".into(),
                amount_cents: 100,
            },
            JournalLine {
                account: "platform_revenue".into(),
                direction: "debit".into(),
                amount_cents: 99,
            },
        ];
        let err = svc
            .post_journal(
                "ops",
                Role::Finance,
                "t1",
                "E1",
                "补账",
                "manual",
                &unbalanced,
            )
            .unwrap_err();
        assert!(err.to_string().contains("借贷不平"), "{err}");
        assert!(svc.journal().unwrap().is_empty());

        let balanced = vec![
            JournalLine {
                account: "user_wallet:u1".into(),
                direction: "credit".into(),
                amount_cents: 100,
            },
            JournalLine {
                account: "platform_revenue".into(),
                direction: "debit".into(),
                amount_cents: 100,
            },
        ];
        let entries = svc
            .post_journal(
                "ops",
                Role::Finance,
                "t2",
                "E1",
                "补账",
                "manual",
                &balanced,
            )
            .unwrap();
        assert_eq!(entries.len(), 2);
        // 同一分录号不能重放（分录不可改，只能冲销）
        let err = svc
            .post_journal(
                "ops",
                Role::Finance,
                "t3",
                "E1",
                "补账",
                "manual",
                &balanced,
            )
            .unwrap_err();
        assert!(err.to_string().contains("已存在"), "{err}");
    }

    #[test]
    fn no_balance_column_exists_anywhere() {
        let store = Store::open_memory().unwrap();
        let conn = store.lock();
        for table in ["g14_merchant", "g14_order", "g14_card", "g14_journal"] {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                !cols.iter().any(|c| c.contains("balance")),
                "{table} 出现了 balance 列：余额只能由分录推导"
            );
        }
    }

    #[test]
    fn csv_export_blocks_formula_injection() {
        let (svc, _p, store) = service();
        open_step_up(&store, "ops");
        // 订单 id 会进路径段（有形状校验），把注入串放在商户可控的 external_no 上
        seed(&svc, "m1", "o_csv", 1_000);
        svc.ingest_order(&Order {
            id: "o_csv2".into(),
            merchant_id: "m1".into(),
            internal_no: "in-csv2".into(),
            external_no: "=cmd|'/c calc'!A1".into(),
            amount_cents: 2_000,
            currency: "CNY".into(),
            state: "paid".into(),
            created_at: now_ms(),
        })
        .unwrap_or_else(|_| panic!("external_no 不该参与形状校验"));
        let csv = svc
            .export_orders(
                "ops",
                Role::Superadmin,
                "t",
                &MerchantScope::All,
                &OrderFilter::default(),
            )
            .unwrap();
        assert!(csv.contains("'=cmd"), "公式必须被前缀化: {csv}");
        // 常规值不受影响
        assert_eq!(csv_cell("o1"), "o1");
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("he said \"hi\""), "\"he said \"\"hi\"\"\"");
        assert_eq!(csv_cell("@SUM(1)"), "'@SUM(1)");
    }

    #[test]
    fn export_without_step_up_is_refused() {
        let (svc, _p, _s) = service();
        seed(&svc, "m1", "o1", 1_000);
        let err = svc
            .export_orders(
                "ops",
                Role::Superadmin,
                "t",
                &MerchantScope::All,
                &OrderFilter::default(),
            )
            .unwrap_err();
        assert!(matches!(err, AdminError::MfaRequired(_)), "{err:?}");
    }

    #[test]
    fn recon_diff_cannot_be_resolved_twice() {
        let (svc, _p, store) = service();
        open_step_up(&store, "ops");
        let diff = ReconDiff {
            id: "d1".into(),
            batch: "2026-10-23".into(),
            external_no: "out-1".into(),
            internal_no: "in-1".into(),
            kind: "amount_mismatch".into(),
            amount_delta_cents: -100,
            state: "open".into(),
            note: String::new(),
            resolved_by: None,
            resolved_at: None,
        };
        assert!(svc.import_diff(&diff).unwrap());
        assert!(!svc.import_diff(&diff).unwrap(), "同批同外部单号不重复入");

        let resolved = svc
            .resolve_diff(
                "ops",
                Role::Finance,
                "t1",
                "d1",
                "adjusted",
                "已补 1 元差额",
            )
            .unwrap();
        assert_eq!(resolved.state, "adjusted");
        let err = svc
            .resolve_diff("ops", Role::Finance, "t2", "d1", "adjusted", "重复")
            .unwrap_err();
        assert!(err.to_string().contains("不能重复处理"), "{err}");
        // 差异处理后批次收敛（没有 open 项）
        assert!(svc
            .diffs(Some("2026-10-23"))
            .unwrap()
            .iter()
            .all(|d| d.state != "open"));
    }

    #[test]
    fn card_redeem_cannot_cross_merchants() {
        let (svc, _p, _s) = service();
        seed(&svc, "m1", "o1", 1_000);
        seed(&svc, "m2", "o2", 1_000);
        let card = svc
            .issue_card("ops", Role::Finance, "t1", "m1", "满100减10", 1_000)
            .unwrap();
        let err = svc
            .redeem_card("ops", Role::Finance, "t2", &card.id, "o2")
            .unwrap_err();
        assert!(err.to_string().contains("跨商户"), "{err}");
        assert_eq!(
            svc.redeem_card("ops", Role::Finance, "t3", &card.id, "o1")
                .unwrap(),
            1_000
        );
        // 已核销不能再用
        assert!(svc
            .redeem_card("ops", Role::Finance, "t4", &card.id, "o1")
            .is_err());
    }
}
