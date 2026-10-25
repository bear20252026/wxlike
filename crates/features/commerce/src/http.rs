//! 命令面 DTO/分发。
//!
//! 这里只做参数解析、鉴权主体透传与稳定 JSON 形状；HTTP 路由、鉴权中间件
//! 与 OpenAPI 由 G00 组合根挂载。金额一律用字符串最小单位输出，避免
//! JavaScript 超过 2^53 后丢精度。

use serde_json::{json, Value};
use wxlike_ledger::{Currency, Money};

use crate::cards::{CardInput, CardStatus};
use crate::entitlements::EntitlementKind;
use crate::error::{CommerceError, Result};
use crate::payments::{IntentDirection, PaymentIntent};
use crate::store;
use crate::transfers::TransferDraft;
use crate::Commerce;

/// 分发表。
///
/// # Errors
/// 未知动作、参数非法、权限不足或领域错误。
pub fn dispatch(commerce: &Commerce, actor: &str, action: &str, payload: &Value) -> Result<Value> {
    store::validate_id("actor", actor)?;
    match action {
        "balance.get" => balance_get(commerce, actor, payload),
        "statement.page" => statement_page(commerce, actor, payload),
        "payment.create" => payment_create(commerce, actor, payload),
        "payment.pay" => payment_pay(commerce, actor, payload),
        "payment.get" => payment_get(commerce, actor, payload),
        "payment.qr.pay" => payment_qr_pay(commerce, actor, payload),
        "card.bind" => card_bind(commerce, actor, payload),
        "card.list" => card_list(commerce, actor),
        "card.unbind" => card_unbind(commerce, actor, payload),
        "topup.create" => topup_create(commerce, actor, payload),
        "topup.get" => topup_get(commerce, actor, payload),
        "topup.confirm" => topup_confirm(commerce, actor, payload),
        "withdrawal.create" => withdrawal_create(commerce, actor, payload),
        "withdrawal.get" => withdrawal_get(commerce, actor, payload),
        "withdrawal.refresh" => withdrawal_refresh(commerce, actor, payload),
        "refund.create" => refund_create(commerce, actor, payload),
        "refund.refresh" => refund_refresh(commerce, actor, payload),
        "reconcile.run" => reconcile_run(commerce),
        "reconcile.list" => reconcile_list(commerce, payload),
        "reconcile.resolve" => reconcile_resolve(commerce, actor, payload),
        "transfer.prepare" => transfer_prepare(commerce, actor, payload),
        "transfer.create" => transfer_create(commerce, actor, payload),
        "transfer.accept" => transfer_accept(commerce, actor, payload),
        "transfer.reject" => transfer_reject(commerce, actor, payload),
        "transfer.expire" => transfer_expire(commerce, actor, payload),
        "transfer.get" => transfer_get(commerce, actor, payload),
        "redpacket.create" => redpacket_create(commerce, actor, payload),
        "redpacket.claim" => redpacket_claim(commerce, actor, payload),
        "redpacket.expire" => redpacket_expire(commerce, actor, payload),
        "redpacket.get" => redpacket_get(commerce, actor, payload),
        "entitlement.rule.publish" => rule_publish(commerce, actor, payload),
        "entitlement.issue" => entitlement_issue(commerce, actor, payload),
        "entitlement.claim" => entitlement_claim(commerce, actor, payload),
        "entitlement.get" => entitlement_get(commerce, actor, payload),
        "entitlement.token" => entitlement_token(commerce, actor, payload),
        "entitlement.redeem" => entitlement_redeem(commerce, actor, payload),
        "entitlement.barcode" => entitlement_barcode(commerce, actor, payload),
        "order.create" => order_create(commerce, actor, payload),
        "order.pay" => order_pay(commerce, actor, payload),
        "order.fulfil" => order_fulfil(commerce, actor, payload),
        "order.cancel" => order_cancel(commerce, actor, payload),
        "order.get" => order_get(commerce, actor, payload),
        "order.payment" => order_payment(commerce, actor, payload),
        other => Err(CommerceError::BadInput(format!("unknown action {other}"))),
    }
}

/// 回调入口（不经过 actor 鉴权，靠签名）。
///
/// # Errors
/// 签名、时间窗、金额或状态错误。
pub fn webhook(commerce: &Commerce, raw_body: &str, signature: &str) -> Result<Value> {
    let outcome = commerce.handle_webhook(raw_body, signature)?;
    Ok(json!({
        "event_id": outcome.event_id,
        "applied": outcome.applied,
        "status": outcome.status.as_str(),
    }))
}

fn money(payload: &Value, max_minor: i64) -> Result<Money> {
    let currency = payload
        .get("currency")
        .and_then(Value::as_str)
        .unwrap_or("CNY");
    let currency = Currency::parse(currency)?;
    let raw = payload
        .get("amount")
        .and_then(Value::as_str)
        .ok_or_else(|| CommerceError::BadInput("amount must be a decimal string".into()))?;
    let minor = store::parse_decimal_minor(raw, max_minor)?;
    Ok(Money::from_minor(minor, currency))
}

fn text<'a>(payload: &'a Value, key: &str) -> Result<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| CommerceError::BadInput(format!("{key} is required")))
}

fn key(payload: &Value) -> Result<&str> {
    text(payload, "idempotency_key")
}

fn number(payload: &Value, key: &str) -> Result<u32> {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| CommerceError::BadInput(format!("{key} must be a positive integer")))
}

fn currency(payload: &Value) -> Result<Currency> {
    Ok(Currency::parse(
        payload
            .get("currency")
            .and_then(Value::as_str)
            .unwrap_or("CNY"),
    )?)
}

fn money_json(amount: Money) -> Value {
    json!({ "minor": amount.minor().to_string(), "currency": amount.currency().as_str() })
}

fn intent_json(intent: &PaymentIntent) -> Value {
    json!({
        "intent_id": intent.id,
        "direction": intent.direction.as_str(),
        "actor_id": intent.actor_id,
        "payee_id": intent.payee_id,
        "amount": money_json(intent.amount),
        "status": intent.status.as_str(),
        "provider_ref": intent.provider_ref,
        "refunded": money_json(intent.refundable()),
        "expires_at": intent.expires_at,
    })
}

fn balance_get(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let balance = commerce.wallet_balance(actor, currency(payload)?)?;
    Ok(json!({
        "currency": balance.currency.as_str(),
        "total": money_json(balance.total()),
        "available": money_json(balance.available()),
        "locked": money_json(balance.locked()),
    }))
}

fn statement_page(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let cursor = payload.get("cursor").and_then(Value::as_i64);
    let limit = payload.get("limit").and_then(Value::as_u64).unwrap_or(50) as u32;
    let page = commerce.wallet_statement(actor, currency(payload)?, cursor, limit)?;
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|entry| {
            json!({
                "entry_id": entry.id.to_string(),
                "journal_id": entry.journal_id,
                "amount": money_json(Money::from_minor(entry.amount, entry.currency)),
                "memo": entry.memo,
                "created_at": entry.created_at,
            })
        })
        .collect();
    Ok(json!({
        "items": items,
        "next_cursor": page.next_cursor.map(|cursor| cursor.to_string()),
        "has_more": page.has_more,
    }))
}

fn payment_create(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let direction = match payload
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("order")
    {
        "topup" => IntentDirection::Topup,
        "order" => IntentDirection::Order,
        other => {
            return Err(CommerceError::BadInput(format!(
                "unknown direction {other}"
            )))
        }
    };
    let payee = payload
        .get("payee_id")
        .and_then(Value::as_str)
        .unwrap_or(actor);
    let amount = money(payload, commerce.config().max_payment_minor)?;
    let (intent, ticket) =
        commerce.create_payment_intent(direction, actor, payee, amount, key(payload)?)?;
    Ok(json!({
        "intent": intent_json(&intent),
        "ticket": ticket.token,
    }))
}

fn payment_pay(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let intent = commerce.pay_payment_intent(actor, text(payload, "intent_id")?)?;
    Ok(json!({ "intent": intent_json(&intent) }))
}

fn payment_get(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let intent = commerce.payment_intent(actor, text(payload, "intent_id")?)?;
    Ok(json!({ "intent": intent_json(&intent) }))
}

fn payment_qr_pay(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let intent = commerce.pay_with_ticket(actor, text(payload, "ticket")?)?;
    Ok(json!({ "intent": intent_json(&intent) }))
}

fn card_bind(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let pan = text(payload, "pan")?;
    let cvv = text(payload, "cvv")?;
    let brand = text(payload, "brand")?;
    let exp_month = number(payload, "exp_month")?;
    let exp_year = number(payload, "exp_year")?;
    let card = commerce.bind_card(
        actor,
        &CardInput {
            pan,
            cvv,
            brand,
            exp_month,
            exp_year,
        },
        key(payload)?,
    )?;
    Ok(json!({
        "card_id": card.id,
        "brand": card.brand,
        "last4": card.last4,
        "exp_month": card.exp_month,
        "exp_year": card.exp_year,
        "status": card.status.as_str(),
    }))
}

fn card_list(commerce: &Commerce, actor: &str) -> Result<Value> {
    let cards: Vec<Value> = commerce
        .list_cards(actor)?
        .into_iter()
        .map(|card| {
            json!({
                "card_id": card.id,
                "brand": card.brand,
                "last4": card.last4,
                "exp_month": card.exp_month,
                "exp_year": card.exp_year,
                "status": match card.status {
                    CardStatus::Active => "active",
                    CardStatus::Unbound => "unbound",
                },
            })
        })
        .collect();
    Ok(json!({ "cards": cards }))
}

fn card_unbind(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    commerce.unbind_card(actor, text(payload, "card_id")?)?;
    Ok(json!({ "ok": true }))
}

fn topup_create(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let amount = money(payload, commerce.config().max_payment_minor)?;
    let (topup, ticket) = commerce.create_topup(actor, amount, key(payload)?)?;
    Ok(json!({
        "topup_id": topup.id,
        "intent_id": topup.intent_id,
        "amount": money_json(topup.amount),
        "status": topup.status.as_str(),
        "ticket": ticket.token,
    }))
}

fn topup_get(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let topup = commerce.topup(actor, text(payload, "topup_id")?)?;
    Ok(json!({
        "topup_id": topup.id,
        "intent_id": topup.intent_id,
        "amount": money_json(topup.amount),
        "status": topup.status.as_str(),
    }))
}

fn topup_confirm(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let topup = commerce.confirm_topup(actor, text(payload, "topup_id")?)?;
    Ok(json!({
        "topup_id": topup.id,
        "status": topup.status.as_str(),
    }))
}

fn withdrawal_create(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let amount = money(payload, commerce.config().max_transfer_minor)?;
    let withdrawal =
        commerce.request_withdrawal(actor, amount, text(payload, "account_ref")?, key(payload)?)?;
    Ok(json!({
        "withdrawal_id": withdrawal.id,
        "amount": money_json(withdrawal.amount),
        "status": withdrawal.status.as_str(),
        "provider_ref": withdrawal.provider_ref,
    }))
}

fn withdrawal_get(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let withdrawal = commerce.withdrawal(actor, text(payload, "withdrawal_id")?)?;
    Ok(json!({
        "withdrawal_id": withdrawal.id,
        "amount": money_json(withdrawal.amount),
        "status": withdrawal.status.as_str(),
    }))
}

fn withdrawal_refresh(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let withdrawal = commerce.refresh_withdrawal(actor, text(payload, "withdrawal_id")?)?;
    Ok(json!({
        "withdrawal_id": withdrawal.id,
        "status": withdrawal.status.as_str(),
    }))
}

fn refund_create(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let amount = money(payload, commerce.config().max_payment_minor)?;
    let refund =
        commerce.request_refund(actor, text(payload, "intent_id")?, amount, key(payload)?)?;
    Ok(json!({
        "refund_id": refund.id,
        "intent_id": refund.intent_id,
        "amount": money_json(refund.amount),
        "status": refund.status.as_str(),
    }))
}

fn refund_refresh(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let refund = commerce.refresh_refund(actor, text(payload, "refund_id")?)?;
    Ok(json!({
        "refund_id": refund.id,
        "status": refund.status.as_str(),
    }))
}

fn reconcile_run(commerce: &Commerce) -> Result<Value> {
    let report = commerce.run_reconciliation()?;
    Ok(json!({
        "run_id": report.run_id,
        "checked": report.checked,
        "exceptions": report.exceptions.iter().map(exception_json).collect::<Vec<_>>(),
    }))
}

fn reconcile_list(commerce: &Commerce, payload: &Value) -> Result<Value> {
    let items = commerce.reconciliation_exceptions(text(payload, "run_id")?)?;
    Ok(json!({
        "items": items.iter().map(exception_json).collect::<Vec<_>>(),
    }))
}

fn reconcile_resolve(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let item = commerce.resolve_exception(
        actor,
        text(payload, "exception_id")?,
        text(payload, "approval_ref")?,
        key(payload)?,
    )?;
    Ok(json!({ "exception": exception_json(&item) }))
}

fn exception_json(item: &crate::settlement::ReconciliationException) -> Value {
    json!({
        "exception_id": item.id,
        "run_id": item.run_id,
        "provider_ref": item.provider_ref,
        "kind": item.local_kind,
        "local_id": item.local_id,
        "amount": money_json(item.amount),
        "reason": item.reason,
        "status": item.status,
        "resolved_by": item.resolved_by,
        "approval_ref": item.approval_ref,
        "journal_key": item.journal_key,
    })
}

fn transfer_prepare(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let amount = money(payload, commerce.config().max_transfer_minor)?;
    let memo = payload.get("memo").and_then(Value::as_str).unwrap_or("");
    let draft = commerce.prepare_transfer(actor, text(payload, "payee_id")?, amount, memo)?;
    Ok(json!({ "draft": draft_json(&draft) }))
}

fn transfer_create(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let draft = draft_from(payload)?;
    let transfer = commerce.create_transfer(
        actor,
        &draft,
        text(payload, "confirmation_token")?,
        key(payload)?,
    )?;
    Ok(json!({ "transfer": transfer_json(&transfer) }))
}

fn draft_json(draft: &TransferDraft) -> Value {
    json!({
        "sender_id": draft.sender_id,
        "payee_id": draft.payee_id,
        "amount": money_json(draft.amount),
        "memo": draft.memo,
        "nonce": draft.nonce,
        "expires_at": draft.expires_at,
        "confirmation_token": draft.confirmation_token,
    })
}

fn draft_from(payload: &Value) -> Result<TransferDraft> {
    let draft = payload
        .get("draft")
        .ok_or_else(|| CommerceError::BadInput("draft is required".into()))?;
    let sender = text(draft, "sender_id")?;
    let payee = text(draft, "payee_id")?;
    let amount = money(draft, i64::MAX / 200)?;
    let memo = text(draft, "memo")?;
    let nonce = text(draft, "nonce")?;
    let expires_at = draft
        .get("expires_at")
        .and_then(Value::as_i64)
        .ok_or_else(|| CommerceError::BadInput("draft expires_at is required".into()))?;
    let confirmation_token = text(draft, "confirmation_token")?;
    Ok(TransferDraft {
        sender_id: sender.to_string(),
        payee_id: payee.to_string(),
        amount,
        memo: memo.to_string(),
        nonce: nonce.to_string(),
        expires_at,
        confirmation_token: confirmation_token.to_string(),
    })
}

fn transfer_json(transfer: &crate::transfers::Transfer) -> Value {
    json!({
        "transfer_id": transfer.id,
        "sender_id": transfer.sender_id,
        "payee_id": transfer.payee_id,
        "amount": money_json(transfer.amount),
        "memo": transfer.memo,
        "status": transfer.status.as_str(),
        "expires_at": transfer.expires_at,
    })
}

fn transfer_accept(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    // 收款方确认；actor 即收款人。
    let transfer = commerce.accept_transfer(actor, text(payload, "transfer_id")?)?;
    Ok(json!({ "transfer": transfer_json(&transfer) }))
}

fn transfer_reject(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let transfer = commerce.reject_transfer(actor, text(payload, "transfer_id")?)?;
    Ok(json!({ "transfer": transfer_json(&transfer) }))
}

fn transfer_expire(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let transfer = commerce.expire_transfer(actor, text(payload, "transfer_id")?)?;
    Ok(json!({ "transfer": transfer_json(&transfer) }))
}

fn transfer_get(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let transfer = commerce.transfer(actor, text(payload, "transfer_id")?)?;
    Ok(json!({ "transfer": transfer_json(&transfer) }))
}

fn redpacket_create(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let total = money(payload, commerce.config().max_transfer_minor)?;
    let count = number(payload, "count")?;
    let memo = payload.get("memo").and_then(Value::as_str).unwrap_or("");
    let packet = commerce.create_red_packet(
        actor,
        text(payload, "conversation_id")?,
        total,
        count,
        memo,
        key(payload)?,
    )?;
    Ok(json!({
        "packet_id": packet.id,
        "total": money_json(packet.total),
        "count": packet.count,
        "status": packet.status.as_str(),
        "expires_at": packet.expires_at,
    }))
}

fn redpacket_claim(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let claim = commerce.claim_red_packet(actor, text(payload, "packet_id")?)?;
    Ok(json!({
        "packet_id": claim.packet_id,
        "amount": money_json(claim.amount),
        "claimed_at": claim.claimed_at,
    }))
}

fn redpacket_expire(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let packet = commerce.expire_red_packet(actor, text(payload, "packet_id")?)?;
    Ok(json!({
        "packet_id": packet.id,
        "status": packet.status.as_str(),
    }))
}

fn redpacket_get(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let view = commerce.red_packet(actor, text(payload, "packet_id")?)?;
    Ok(json!({
        "packet_id": view.packet.id,
        "total": money_json(view.packet.total),
        "count": view.packet.count,
        "status": view.packet.status.as_str(),
        "claims": view.claims.iter().map(|claim| json!({
            "user_id": claim.user_id,
            "amount": money_json(claim.amount),
            "claimed_at": claim.claimed_at,
        })).collect::<Vec<_>>(),
    }))
}

fn rule_publish(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let kind = EntitlementKind::parse(text(payload, "kind")?)?;
    let rule = payload
        .get("rule")
        .ok_or_else(|| CommerceError::BadInput("rule is required".into()))?;
    let rule = commerce.publish_entitlement_rule(actor, kind, rule)?;
    Ok(json!({
        "issuer_id": rule.issuer_id,
        "kind": rule.kind.as_str(),
        "version": rule.version.to_string(),
        "digest": rule.digest,
    }))
}

fn entitlement_issue(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let kind = EntitlementKind::parse(text(payload, "kind")?)?;
    let owner = payload.get("owner_id").and_then(Value::as_str);
    let body = payload
        .get("payload")
        .ok_or_else(|| CommerceError::BadInput("payload is required".into()))?;
    let entitlement = commerce.issue_entitlement(actor, owner, kind, body, key(payload)?)?;
    Ok(json!({
        "entitlement_id": entitlement.id,
        "kind": entitlement.kind.as_str(),
        "status": entitlement.status_name(),
        "valid_to": entitlement.valid_to,
    }))
}

fn entitlement_claim(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let entitlement = commerce.claim_entitlement(actor, text(payload, "entitlement_id")?)?;
    Ok(json!({
        "entitlement_id": entitlement.id,
        "status": entitlement.status_name(),
    }))
}

fn entitlement_get(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let entitlement = commerce.entitlement(actor, text(payload, "entitlement_id")?)?;
    Ok(json!({
        "entitlement_id": entitlement.id,
        "issuer_id": entitlement.issuer_id,
        "kind": entitlement.kind.as_str(),
        "rule_version": entitlement.rule_version.to_string(),
        "status": entitlement.status_name(),
        "valid_from": entitlement.valid_from,
        "valid_to": entitlement.valid_to,
        "payload": entitlement.payload,
    }))
}

fn entitlement_token(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let token = commerce.request_redemption_token(actor, text(payload, "entitlement_id")?)?;
    Ok(json!({
        "token": token.token,
        "expires_at": token.expires_at,
    }))
}

fn entitlement_redeem(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let redemption = commerce.redeem_entitlement(actor, text(payload, "token")?, key(payload)?)?;
    Ok(json!({
        "redemption_id": redemption.id,
        "entitlement_id": redemption.entitlement_id,
        "redeemed_at": redemption.redeemed_at,
    }))
}

fn entitlement_barcode(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let barcode = commerce.ticket_barcode(actor, text(payload, "entitlement_id")?)?;
    Ok(json!({
        "format": barcode.format,
        "value": barcode.value,
    }))
}

fn order_create(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let amount = money(payload, commerce.config().max_payment_minor)?;
    let order = commerce.create_order(
        actor,
        text(payload, "payee_id")?,
        text(payload, "quote_ref")?,
        amount,
        key(payload)?,
    )?;
    Ok(json!({ "order": order_json(&order) }))
}

fn order_pay(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let order = commerce.pay_order(actor, text(payload, "order_id")?, key(payload)?)?;
    Ok(json!({ "order": order_json(&order) }))
}

fn order_fulfil(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let order = commerce.fulfil_order(
        actor,
        text(payload, "order_id")?,
        text(payload, "provider_order_ref")?,
    )?;
    Ok(json!({ "order": order_json(&order) }))
}

fn order_cancel(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let order = commerce.cancel_order(actor, text(payload, "order_id")?)?;
    Ok(json!({ "order": order_json(&order) }))
}

fn order_get(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let order = commerce.order(actor, text(payload, "order_id")?)?;
    Ok(json!({ "order": order_json(&order) }))
}

fn order_payment(commerce: &Commerce, actor: &str, payload: &Value) -> Result<Value> {
    let intent = commerce.order_payment(actor, text(payload, "order_id")?)?;
    Ok(json!({ "intent": intent.as_ref().map(intent_json) }))
}

fn order_json(order: &crate::orders::Order) -> Value {
    json!({
        "order_id": order.id,
        "actor_id": order.actor_id,
        "payee_id": order.payee_id,
        "quote_ref": order.quote_ref,
        "amount": money_json(order.amount),
        "status": order.status.as_str(),
        "payment_intent_id": order.payment_intent_id,
        "provider_order_ref": order.provider_order_ref,
    })
}
