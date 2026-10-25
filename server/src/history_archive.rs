//! Do not publish a history cursor until every selected mailbox body is available.

use std::collections::HashMap;
use std::time::Duration;

use axum::http::StatusCode;
use wxlike_core::store::StoredMessage;
use wxlike_core::Store;
use wxlike_message::MailMessage;

type HistoryError = (StatusCode, String);
const ARCHIVE_WAIT: Duration = Duration::from_secs(2);
const ARCHIVE_RETRY: Duration = Duration::from_millis(50);

pub(super) async fn read_page(
    store: &Store,
    conv: &str,
    page: &[MailMessage],
    ttl_ms: u64,
) -> Result<Vec<serde_json::Value>, HistoryError> {
    let ids: Vec<Vec<u8>> = page
        .iter()
        .map(|m| hex::decode(&m.msg_id).map_err(invalid_archive))
        .collect::<Result<_, _>>()?;
    let bodies = wait_for_bodies(store, &ids).await?;
    let mut messages = Vec::with_capacity(page.len());
    for (message, id) in page.iter().zip(&ids) {
        let row = bodies.get(id).ok_or_else(archive_pending)?;
        if row.envelope.conv_id != conv {
            return Err(invalid_archive("body belongs to another conversation"));
        }
        let seq = i64::try_from(message.seq).map_err(invalid_archive)?;
        let value = super::message_json_with_seq(row, seq);
        if !super::secret_ttl_expired(ttl_ms, &value) {
            messages.push(value);
        }
    }
    Ok(messages)
}

async fn wait_for_bodies(
    store: &Store,
    ids: &[Vec<u8>],
) -> Result<HashMap<Vec<u8>, StoredMessage>, HistoryError> {
    let deadline = tokio::time::Instant::now() + ARCHIVE_WAIT;
    loop {
        let bodies = store.by_msg_ids(ids).map_err(invalid_archive)?;
        if ids.iter().all(|id| bodies.contains_key(id)) {
            return Ok(bodies);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(archive_pending());
        }
        tokio::time::sleep(ARCHIVE_RETRY).await;
    }
}

fn archive_pending() -> HistoryError {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "history archive pending; retry the same cursor".into(),
    )
}

fn invalid_archive(error: impl std::fmt::Display) -> HistoryError {
    tracing::warn!(%error, "history archive read failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "history archive unavailable".into(),
    )
}
