//! P3 多级免打扰的持久面（ADR-19 决策层）：每用户 × 每会话的级别与关键词。
//!
//! 存储原始字符串（level: all|mention|none + JSON 关键词数组），`MuteRule`
//! 的解释权在 push 决策层（不变式 5：本 crate 不知道"要不要推"，只知道
//! "用户想要什么"）。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route(
            "/api/notification-rules/{conv_id}",
            put(set_notification_rule),
        )
        .route(
            "/api/notification-rules/{conv_id}",
            get(get_notification_rule),
        )
        .route("/api/notification-rules", get(list_notification_rules))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct NotificationRule {
    pub conv_id: String,
    pub level: String,
    pub keywords: Vec<String>,
}

impl Platform {
    pub fn set_notification_rule(
        &self,
        user: &str,
        conv: &str,
        level: &str,
        keywords: &[String],
    ) -> Result<()> {
        let (user, conv) = (validate::id(user)?, validate::conv(conv)?);
        if crate::relations::MUTE_LEVELS.iter().all(|l| *l != level) {
            return Err(PlatformError::BadInput("level must be all|mention|none"));
        }
        if keywords.len() > 32 {
            return Err(PlatformError::BadInput("too many keywords"));
        }
        for kw in keywords {
            if kw.is_empty() || kw.chars().count() > 64 {
                return Err(PlatformError::BadInput("keyword must be 1..=64 chars"));
            }
        }
        self.db().lock().execute(
            "INSERT INTO notification_settings(user_id, conv_id, level, keywords)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, conv_id) DO UPDATE SET
               level = excluded.level, keywords = excluded.keywords",
            params![user, conv, level, serde_json::to_string(keywords)?],
        )?;
        Ok(())
    }

    pub fn notification_rule(&self, user: &str, conv: &str) -> Result<Option<NotificationRule>> {
        let conv = validate::conv(conv)?;
        let c = self.db().lock();
        let row: Option<(String, String)> = c
            .query_row(
                "SELECT level, keywords FROM notification_settings
                 WHERE user_id = ?1 AND conv_id = ?2",
                params![user, conv],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((level, keywords_json)) => Ok(Some(NotificationRule {
                conv_id: conv.to_string(),
                level,
                keywords: serde_json::from_str(&keywords_json)?,
            })),
            None => Ok(None),
        }
    }

    pub fn notification_rules(&self, user: &str) -> Result<Vec<NotificationRule>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT conv_id, level, keywords FROM notification_settings
             WHERE user_id = ?1 ORDER BY conv_id",
        )?;
        let rows = stmt.query_map(params![user], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (conv_id, level, keywords_json) = row?;
            out.push(NotificationRule {
                conv_id,
                level,
                keywords: serde_json::from_str(&keywords_json)?,
            });
        }
        Ok(out)
    }
}

#[derive(Deserialize)]
struct RuleBody {
    level: String,
    #[serde(default)]
    keywords: Vec<String>,
}

async fn set_notification_rule(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    Json(body): Json<RuleBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_notification_rule(&authed.user_id, &conv, &body.level, &body.keywords)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_notification_rule(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "rule": platform.notification_rule(&authed.user_id, &conv)?
    })))
}

async fn list_notification_rules(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "rules": platform.notification_rules(&authed.user_id)?
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_roundtrip_and_validation() {
        let p = Platform::in_memory().unwrap();
        let uid = p.register("alice", "pw").unwrap();
        let conv = "g-12345678";

        // 无规则 = None
        assert!(p.notification_rule(&uid, conv).unwrap().is_none());

        // 设置 mention + 关键词
        p.set_notification_rule(&uid, conv, "mention", &["广告".into(), "加微".into()])
            .unwrap();
        let rule = p.notification_rule(&uid, conv).unwrap().unwrap();
        assert_eq!(rule.level, "mention");
        assert_eq!(rule.keywords, vec!["广告".to_string(), "加微".to_string()]);

        // 覆盖更新
        p.set_notification_rule(&uid, conv, "none", &[]).unwrap();
        let rule = p.notification_rule(&uid, conv).unwrap().unwrap();
        assert_eq!(rule.level, "none");
        assert!(rule.keywords.is_empty());

        // 非法级别/关键词被拒
        assert!(p.set_notification_rule(&uid, conv, "loud", &[]).is_err());
        assert!(p
            .set_notification_rule(&uid, conv, "all", &["".into()])
            .is_err());
        let many: Vec<String> = (0..33).map(|i| format!("k{i}")).collect();
        assert!(p.set_notification_rule(&uid, conv, "all", &many).is_err());

        // 列表
        p.set_notification_rule(&uid, "g-other0000", "all", &[])
            .unwrap();
        let all = p.notification_rules(&uid).unwrap();
        assert_eq!(all.len(), 2);
        // 他人隔离
        let bob = p.register("bob", "pw").unwrap();
        assert!(p.notification_rules(&bob).unwrap().is_empty());
    }
}
