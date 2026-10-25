use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::prelude::*;

const MAX_ACCOUNT_LEN: usize = 64;
const MAX_BUSINESS_NO_LEN: usize = 128;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const OFFER_TTL_MS: i64 = 5 * 60 * 1000;
pub const MIN_TOPUP_MINOR: i64 = 100;
pub const MAX_TOPUP_MINOR: i64 = 100_000;
pub const MIN_DONATION_MINOR: i64 = 100;
pub const MAX_DONATION_MINOR: i64 = 10_000_000;
pub const MAX_BILL_MINOR: i64 = 10_000_000;
pub const PHONE_LEN: usize = 11;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorBill {
    pub account: String,
    pub payee: String,
    pub amount_minor: i64,
    pub currency: String,
    pub bill_ref: String,
    pub signature: String,
}

impl VendorBill {
    pub fn canonical(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.account, self.payee, self.amount_minor, self.currency, self.bill_ref
        )
    }
}

pub fn sign_bill(key: &[u8], bill: &VendorBill) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(bill.canonical().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn verify_bill(key: &[u8], bill: &VendorBill) -> bool {
    let Ok(signature) = hex::decode(&bill.signature) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac key");
    mac.update(bill.canonical().as_bytes());
    mac.verify_slice(&signature).is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    Completed,
    Pending,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorReceipt {
    pub business_no: String,
    pub state: ReceiptState,
    pub message: String,
    pub issued_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorDonation {
    pub receipt: VendorReceipt,
    pub institution: String,
    pub project: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DonationProject {
    pub project_id: String,
    pub institution: String,
    pub title: String,
    pub authorized: bool,
}

#[async_trait]
pub trait UtilitiesVendor: Send + Sync {
    async fn lookup_bill(&self, account: &str) -> ProviderResult<VendorBill>;

    async fn mobile_topup(
        &self,
        phone: &str,
        amount_minor: i64,
        idempotency_key: &str,
    ) -> ProviderResult<VendorReceipt>;

    async fn pay_bill(
        &self,
        bill: &VendorBill,
        amount_minor: i64,
        idempotency_key: &str,
    ) -> ProviderResult<VendorReceipt>;

    async fn donate(
        &self,
        project_id: &str,
        amount_minor: i64,
        idempotency_key: &str,
    ) -> ProviderResult<VendorDonation>;

    async fn cancel(
        &self,
        business_no: &str,
        idempotency_key: &str,
    ) -> ProviderResult<ProviderCancellation>;

    async fn query(&self, business_no: &str) -> ProviderResult<VendorReceipt>;
}

#[derive(Deserialize)]
struct TopupParams {
    phone: String,
    amount_minor: i64,
}

#[derive(Deserialize)]
struct BillParams {
    account: String,
}

#[derive(Deserialize)]
struct DonationParams {
    project_id: String,
    amount_minor: i64,
}

pub struct UtilitiesProvider<V: UtilitiesVendor> {
    vendor: V,
    bill_key: Vec<u8>,
    evidence: Option<SandboxEvidence>,
    clock: Arc<dyn Clock>,
    projects: HashMap<String, DonationProject>,
    bills: Mutex<HashMap<String, VendorBill>>,
}

impl<V: UtilitiesVendor> UtilitiesProvider<V> {
    pub fn new(
        vendor: V,
        bill_key: Vec<u8>,
        evidence: Option<SandboxEvidence>,
        clock: Arc<dyn Clock>,
        projects: Vec<DonationProject>,
    ) -> Result<Self> {
        if bill_key.len() < 16 {
            return Err(ServiceError::new(Code::InvalidArgument, "bill signing key"));
        }
        let projects = projects
            .into_iter()
            .map(|project| (project.project_id.clone(), project))
            .collect();
        Ok(Self {
            vendor,
            bill_key,
            evidence,
            clock,
            projects,
            bills: Mutex::new(HashMap::new()),
        })
    }

    /// 户号账单必须先查询并验签，页面拿到的金额与服务端缓存一致才能支付。
    pub async fn lookup_bill(&self, account: &str) -> Result<VendorBill> {
        if account.is_empty() || account.len() > MAX_ACCOUNT_LEN {
            return Err(ServiceError::new(Code::InvalidArgument, "bill account"));
        }
        let bill = self
            .vendor
            .lookup_bill(account)
            .await
            .map_err(|err| crate::provider::map_provider_error(self.id(), err))?;
        if !verify_bill(&self.bill_key, &bill) || bill.account != account {
            return Err(ServiceError::new(Code::InvalidArgument, "bill signature"));
        }
        if bill.amount_minor <= 0 || bill.amount_minor > MAX_BILL_MINOR {
            return Err(ServiceError::new(Code::InvalidArgument, "bill amount"));
        }
        self.bills
            .lock()
            .map_err(|_| ServiceError::new(Code::Internal, "bill cache"))?
            .insert(account.to_owned(), bill.clone());
        Ok(bill)
    }

    pub fn support_contact(&self) -> String {
        format!("support://service?provider={}", self.id())
    }

    fn parse_topup(&self, params: &Value) -> ProviderResult<TopupParams> {
        let parsed: TopupParams = serde_json::from_value(params.clone())
            .map_err(|_| ProviderError::Rejected("params"))?;
        if !valid_phone(&parsed.phone)
            || !(MIN_TOPUP_MINOR..=MAX_TOPUP_MINOR).contains(&parsed.amount_minor)
        {
            return Err(ProviderError::Rejected("topup params"));
        }
        Ok(parsed)
    }

    fn parse_donation(&self, params: &Value) -> ProviderResult<(DonationParams, DonationProject)> {
        let parsed: DonationParams = serde_json::from_value(params.clone())
            .map_err(|_| ProviderError::Rejected("params"))?;
        if !(MIN_DONATION_MINOR..=MAX_DONATION_MINOR).contains(&parsed.amount_minor) {
            return Err(ProviderError::Rejected("donation amount"));
        }
        let project = self
            .projects
            .get(&parsed.project_id)
            .ok_or(ProviderError::Rejected("unknown project"))?
            .clone();
        if !project.authorized {
            return Err(ProviderError::Rejected("project not authorized"));
        }
        Ok((parsed, project))
    }

    async fn bill_from_params(&self, params: &Value) -> ProviderResult<VendorBill> {
        let parsed: BillParams = serde_json::from_value(params.clone())
            .map_err(|_| ProviderError::Rejected("params"))?;
        let cached = self
            .bills
            .lock()
            .ok()
            .and_then(|bills| bills.get(&parsed.account).cloned());
        match cached {
            Some(bill) if verify_bill(&self.bill_key, &bill) => Ok(bill),
            _ => Err(ProviderError::Rejected("bill must be looked up first")),
        }
    }
}

fn valid_phone(phone: &str) -> bool {
    phone.len() == PHONE_LEN
        && phone.bytes().all(|b| b.is_ascii_digit())
        && phone.starts_with('1')
        && phone.as_bytes()[1] != b'0'
        && phone.as_bytes()[1] != b'1'
        && phone.as_bytes()[1] != b'2'
}

fn receipt_state(state: ReceiptState) -> ProviderOrderState {
    match state {
        ReceiptState::Completed => ProviderOrderState::Fulfilled,
        ReceiptState::Pending => ProviderOrderState::Accepted,
        ReceiptState::Rejected => ProviderOrderState::Failed,
    }
}

fn to_provider_receipt(receipt: &VendorReceipt) -> ProviderReceipt {
    ProviderReceipt {
        business_no: receipt.business_no.clone(),
        status: match receipt.state {
            ReceiptState::Completed => "completed",
            ReceiptState::Pending => "pending",
            ReceiptState::Rejected => "rejected",
        }
        .to_owned(),
        message: receipt.message.clone(),
        issued_at_ms: receipt.issued_at_ms,
    }
}

fn ensure_receipt(receipt: &VendorReceipt) -> ProviderResult<()> {
    if receipt.business_no.is_empty() || receipt.business_no.len() > MAX_BUSINESS_NO_LEN {
        return Err(ProviderError::Rejected("missing business number"));
    }
    if receipt.state == ReceiptState::Rejected {
        return Err(ProviderError::Rejected("vendor rejected"));
    }
    Ok(())
}

#[async_trait]
impl<V: UtilitiesVendor> ServiceProvider for UtilitiesProvider<V> {
    fn id(&self) -> &'static str {
        "utilities"
    }

    fn supports(&self, kind: ServiceType) -> bool {
        matches!(
            kind,
            ServiceType::MobileTopup | ServiceType::UtilityBill | ServiceType::Donation
        )
    }

    fn sandbox_evidence(&self) -> Option<SandboxEvidence> {
        self.evidence.clone()
    }

    fn support_hint(&self) -> Option<String> {
        Some(self.support_contact())
    }

    async fn availability(&self, ctx: &QuoteContext<'_>) -> ProviderResult<Availability> {
        let (amount_minor, currency, terms) = match ctx.card.kind {
            ServiceType::MobileTopup => {
                let topup = self.parse_topup(ctx.params)?;
                (
                    topup.amount_minor,
                    "CNY".to_owned(),
                    "utilities:topup:v1".to_owned(),
                )
            }
            ServiceType::UtilityBill => {
                let bill = self.bill_from_params(ctx.params).await?;
                (
                    bill.amount_minor,
                    bill.currency,
                    format!("utilities:bill:{}", bill.bill_ref),
                )
            }
            ServiceType::Donation => {
                let (donation, _) = self.parse_donation(ctx.params)?;
                (
                    donation.amount_minor,
                    "CNY".to_owned(),
                    format!("utilities:donation:{}", donation.project_id),
                )
            }
            _ => return Err(ProviderError::Rejected("unsupported service")),
        };
        Ok(Availability {
            available: true,
            amount_minor,
            currency,
            terms_version: terms,
            valid_until_ms: self.clock.now_ms() + OFFER_TTL_MS,
            note: None,
        })
    }

    async fn place_order(&self, request: &ProviderOrderRequest) -> ProviderResult<ProviderOrder> {
        let receipt = self.execute(request).await?;
        ensure_receipt(&receipt)?;
        let state = receipt_state(receipt.state);
        Ok(ProviderOrder {
            provider_order_id: receipt.business_no.clone(),
            state,
            receipt: Some(to_provider_receipt(&receipt)),
        })
    }

    async fn fulfill(&self, order_ref: &str, _key: &str) -> ProviderResult<ProviderReceipt> {
        let receipt = self.vendor.query(order_ref).await?;
        if receipt.state != ReceiptState::Completed {
            return Err(ProviderError::Rejected("not completed"));
        }
        Ok(to_provider_receipt(&receipt))
    }

    async fn cancel(&self, order_ref: &str, key: &str) -> ProviderResult<ProviderCancellation> {
        self.vendor.cancel(order_ref, key).await
    }

    async fn query(&self, order_ref: &str) -> ProviderResult<ProviderStatus> {
        let receipt = self.vendor.query(order_ref).await?;
        Ok(ProviderStatus {
            state: receipt_state(receipt.state),
            receipt: Some(to_provider_receipt(&receipt)),
        })
    }
}

impl<V: UtilitiesVendor> UtilitiesProvider<V> {
    async fn execute(&self, request: &ProviderOrderRequest) -> ProviderResult<VendorReceipt> {
        match request.kind {
            ServiceType::MobileTopup => {
                let topup = self.parse_topup(&request.params)?;
                if topup.amount_minor != request.amount_minor {
                    return Err(ProviderError::Rejected("amount mismatch"));
                }
                let receipt = self
                    .vendor
                    .mobile_topup(&topup.phone, request.amount_minor, &request.idempotency_key)
                    .await?;
                Ok(receipt)
            }
            ServiceType::UtilityBill => {
                let bill = self.bill_from_params(&request.params).await?;
                if bill.amount_minor != request.amount_minor || bill.currency != request.currency {
                    return Err(ProviderError::Rejected("bill amount changed"));
                }
                let receipt = self
                    .vendor
                    .pay_bill(&bill, request.amount_minor, &request.idempotency_key)
                    .await?;
                Ok(receipt)
            }
            ServiceType::Donation => {
                let (donation, project) = self.parse_donation(&request.params)?;
                if donation.amount_minor != request.amount_minor {
                    return Err(ProviderError::Rejected("amount mismatch"));
                }
                let result = self
                    .vendor
                    .donate(
                        &donation.project_id,
                        request.amount_minor,
                        &request.idempotency_key,
                    )
                    .await?;
                if result.project != project.project_id || result.institution != project.institution
                {
                    return Err(ProviderError::Rejected("payee mismatch"));
                }
                Ok(result.receipt)
            }
            _ => Err(ProviderError::Rejected("unsupported service")),
        }
    }
}
