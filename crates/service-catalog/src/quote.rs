use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::ServiceType;
use crate::error::{Result, ServiceError, ServiceErrorCode as Code};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub quote_id: String,
    pub actor: String,
    pub service_id: String,
    pub kind: ServiceType,
    pub provider: String,
    pub region: String,
    pub params: Value,
    pub quantity: u32,
    pub amount_minor: i64,
    pub currency: String,
    pub terms_version: String,
    pub card_revision: u64,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Default)]
pub struct QuoteBook {
    quotes: Mutex<HashMap<String, Quote>>,
}

impl QuoteBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, quote: Quote) -> Result<()> {
        self.quotes
            .lock()
            .map_err(|_| internal())?
            .insert(quote.quote_id.clone(), quote);
        Ok(())
    }

    pub fn get(&self, quote_id: &str) -> Result<Quote> {
        self.quotes
            .lock()
            .map_err(|_| internal())?
            .get(quote_id)
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::Expired, "quote not found or expired"))
    }

    pub fn purge_expired(&self, now_ms: i64) -> usize {
        let Ok(mut quotes) = self.quotes.lock() else {
            return 0;
        };
        let before = quotes.len();
        quotes.retain(|_, quote| quote.expires_at_ms > now_ms);
        before - quotes.len()
    }
}

fn internal() -> ServiceError {
    ServiceError::new(Code::Internal, "quote state")
}
