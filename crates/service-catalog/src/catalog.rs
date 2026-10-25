use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Result, ServiceError, ServiceErrorCode as Code};

pub const CURRENCY_LEN: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceType {
    MobileTopup,
    UtilityBill,
    Donation,
    Travel,
    Hotel,
    Movie,
    Medical,
    Retail,
    Game,
}

impl ServiceType {
    pub const ALL: [ServiceType; 9] = [
        ServiceType::MobileTopup,
        ServiceType::UtilityBill,
        ServiceType::Donation,
        ServiceType::Travel,
        ServiceType::Hotel,
        ServiceType::Movie,
        ServiceType::Medical,
        ServiceType::Retail,
        ServiceType::Game,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MobileTopup => "mobile_topup",
            Self::UtilityBill => "utility_bill",
            Self::Donation => "donation",
            Self::Travel => "travel",
            Self::Hotel => "hotel",
            Self::Movie => "movie",
            Self::Medical => "medical",
            Self::Retail => "retail",
            Self::Game => "game",
        }
    }

    pub fn requires_vendor_authorization(self) -> bool {
        matches!(
            self,
            Self::Travel | Self::Hotel | Self::Movie | Self::Medical
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceCard {
    pub service_id: String,
    pub kind: ServiceType,
    pub title: String,
    pub provider: String,
    pub region: String,
    pub currency: String,
    pub display_price_minor: i64,
    pub terms_version: String,
    pub enabled: bool,
    pub revision: u64,
}

impl ServiceCard {
    pub fn validate(&self) -> Result<()> {
        if self.service_id.is_empty() || self.title.is_empty() || self.provider.is_empty() {
            return Err(ServiceError::new(Code::InvalidArgument, "card identity"));
        }
        if self.region.is_empty() || self.terms_version.is_empty() {
            return Err(ServiceError::new(
                Code::InvalidArgument,
                "card region/terms",
            ));
        }
        if self.currency.len() != CURRENCY_LEN
            || !self.currency.bytes().all(|b| b.is_ascii_uppercase())
        {
            return Err(ServiceError::new(Code::InvalidArgument, "card currency"));
        }
        if self.display_price_minor < 0 {
            return Err(ServiceError::new(Code::InvalidArgument, "card price"));
        }
        Ok(())
    }
}

/// 服务目录与管理员区域开关；未配置的区域一律显式阻塞，不做默认放行。
#[derive(Default)]
pub struct Catalog {
    cards: Mutex<HashMap<String, ServiceCard>>,
    regions: Mutex<HashSet<(ServiceType, String)>>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&self, card: ServiceCard) -> Result<()> {
        card.validate()?;
        self.cards
            .lock()
            .map_err(|_| internal())?
            .insert(card.service_id.clone(), card);
        Ok(())
    }

    /// 管理员配置某类服务在某个区域可用/不可用。
    pub fn set_region_enabled(&self, kind: ServiceType, region: &str, enabled: bool) -> Result<()> {
        if region.is_empty() {
            return Err(ServiceError::new(Code::InvalidArgument, "region"));
        }
        let mut regions = self.regions.lock().map_err(|_| internal())?;
        let key = (kind, region.to_owned());
        if enabled {
            regions.insert(key);
        } else {
            regions.remove(&key);
        }
        Ok(())
    }

    pub fn region_enabled(&self, kind: ServiceType, region: &str) -> bool {
        self.regions
            .lock()
            .map(|regions| regions.contains(&(kind, region.to_owned())))
            .unwrap_or(false)
    }

    pub fn get(&self, service_id: &str) -> Result<ServiceCard> {
        let cards = self.cards.lock().map_err(|_| internal())?;
        cards
            .get(service_id)
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::NotFound, "service not found"))
    }

    /// 目录只返回启用且当前区域可用的卡片；空结果就是空结果，不拿静态假数据填充。
    pub fn list(&self, kind: ServiceType, region: &str) -> Vec<ServiceCard> {
        let Ok(cards) = self.cards.lock() else {
            return Vec::new();
        };
        let mut visible: Vec<ServiceCard> = cards
            .values()
            .filter(|card| card.kind == kind && card.region == region && card.enabled)
            .filter(|_| self.region_enabled(kind, region))
            .cloned()
            .collect();
        visible.sort_by(|a, b| a.service_id.cmp(&b.service_id));
        visible
    }

    /// 改价必须递增 revision，让旧报价可以被识别为过期而不是静默加价。
    pub fn update_price(&self, service_id: &str, price_minor: i64) -> Result<u64> {
        if price_minor < 0 {
            return Err(ServiceError::new(Code::InvalidArgument, "price"));
        }
        let mut cards = self.cards.lock().map_err(|_| internal())?;
        let card = cards
            .get_mut(service_id)
            .ok_or_else(|| ServiceError::new(Code::NotFound, "service not found"))?;
        card.display_price_minor = price_minor;
        card.revision = card.revision.wrapping_add(1);
        Ok(card.revision)
    }
}

fn internal() -> ServiceError {
    ServiceError::new(Code::Internal, "catalog state")
}
