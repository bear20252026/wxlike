//! 本地持久化：P2 消息镜像库（LocalStore）与秘密聊天快照（SecretStore）。
//!
//! G00-02 纯移动拆分：内容自 lib.rs 原样搬出，签名零改动；
//! 可见性经 lib.rs 的 pub(crate) 再导出 + `use super::*` 保持。

use super::*;

/// P2 local message library: the client's own SQLite mirror of the
/// conversations it has seen. "打开即渲染" — the chat list paints from here
/// before the network answers; the server history only contributes *older*
/// rows (and authoritative recall state).
///
/// Rows are the same JSON event payloads the webview renders, so the
/// frontend merges hub-live, local, and server pages with one function.
pub struct LocalStore {
    conn: Mutex<rusqlite::Connection>,
}

pub struct LocalRow {
    pub seq: i64,
    pub conv_id: String,
    pub sender: String,
    pub kind: String,
    pub text: Option<String>,
    pub media_id: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub msg_id: Option<String>,
    pub quote_of: Option<String>,
    pub sent_at_ms: i64,
}

impl LocalRow {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "conv_id": self.conv_id,
            "seq": self.seq,
            "sender": self.sender,
            "kind": self.kind,
            "sent_at_ms": self.sent_at_ms,
        });
        if let Some(t) = &self.text {
            v["text"] = t.as_str().into();
        }
        if let Some(m) = &self.media_id {
            v["media_id"] = m.as_str().into();
        }
        if let Some(p) = &self.payload {
            v["payload"] = p.clone();
        }
        if let Some(id) = &self.msg_id {
            v["msg_id"] = id.as_str().into();
        }
        if let Some(q) = &self.quote_of {
            v["quote_of"] = q.as_str().into();
        }
        v
    }
}

impl LocalStore {
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            -- 消失消息删除后不要把正文留在空闲页里
            PRAGMA secure_delete = ON;
            CREATE TABLE IF NOT EXISTS local_messages (
                conv_id    TEXT NOT NULL,
                seq        INTEGER NOT NULL,
                msg_id     TEXT NOT NULL,
                sender     TEXT NOT NULL DEFAULT '',
                kind       TEXT NOT NULL DEFAULT 'text',
                text       TEXT,
                media_id   TEXT,
                payload    TEXT,
                quote_of   TEXT,
                sent_at_ms INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (conv_id, seq, msg_id)
            );
            -- 会话 TTL 与开关：重启后离线也能继续按期限清理
            CREATE TABLE IF NOT EXISTS secret_ttls (
                conv_id TEXT PRIMARY KEY,
                enabled INTEGER NOT NULL DEFAULT 0,
                ttl_ms  INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One event payload → one row. Typing signals and receipts are refused
    /// by design: they are transient, and the local library must not replay
    /// "正在输入" on the next launch.
    pub fn append(&self, payload: &serde_json::Value) -> anyhow::Result<()> {
        let kind = payload["kind"].as_str().unwrap_or_default();
        if kind == "typing" || kind == "receipt" {
            return Ok(());
        }
        let conv_id = payload["conv_id"].as_str().unwrap_or_default().to_string();
        if conv_id.is_empty() {
            return Ok(()); // not a conversation event
        }
        let seq = payload["seq"].as_i64().unwrap_or(0);
        let msg_id = payload["msg_id"].as_str().unwrap_or_default().to_string();
        let sender = payload["sender"].as_str().unwrap_or_default().to_string();
        let text = payload["text"].as_str().map(str::to_string);
        let media_id = payload["media_id"].as_str().map(str::to_string);
        let quote_of = payload["quote_of"].as_str().map(str::to_string);
        let payload_json = payload.get("payload").map(|p| p.to_string());
        let sent_at_ms = payload["sent_at_ms"].as_i64().unwrap_or(0);
        self.lock().execute(
            "INSERT OR IGNORE INTO local_messages
             (conv_id, seq, msg_id, sender, kind, text, media_id, payload, quote_of, sent_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                conv_id,
                seq,
                msg_id,
                sender,
                kind,
                text,
                media_id,
                payload_json,
                quote_of,
                sent_at_ms
            ],
        )?;
        Ok(())
    }

    /// 记住会话的消失消息设置；服务端仍是唯一真相，这里只负责重启后还能清理。
    pub fn set_secret_ttl(&self, conv_id: &str, enabled: bool, ttl_ms: u64) -> anyhow::Result<()> {
        self.lock().execute(
            "INSERT INTO secret_ttls(conv_id, enabled, ttl_ms) VALUES(?1, ?2, ?3)
             ON CONFLICT(conv_id) DO UPDATE SET
               enabled = excluded.enabled,
               ttl_ms  = excluded.ttl_ms",
            rusqlite::params![conv_id, enabled as i64, ttl_ms as i64],
        )?;
        Ok(())
    }

    /// 启动时把持久化的 TTL 读回内存（只返回生效的会话）。
    pub fn load_secret_ttls(&self) -> anyhow::Result<std::collections::HashMap<String, u64>> {
        let c = self.lock();
        let mut stmt = c.prepare("SELECT conv_id, ttl_ms FROM secret_ttls WHERE enabled != 0")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (conv, ttl) = row?;
            out.insert(conv, ttl);
        }
        Ok(out)
    }

    /// 删除所有 `sent_at + ttl <= now` 的本地行。返回删除条数。
    pub fn purge_expired(&self, ttls: &[(String, u64)], now_ms: u64) -> anyhow::Result<usize> {
        let mut c = self.lock();
        let tx = c.transaction()?;
        let mut removed = 0usize;
        for (conv, ttl) in ttls.iter().filter(|(_, ttl)| *ttl > 0) {
            removed += tx.execute(
                "DELETE FROM local_messages
                 WHERE conv_id = ?1 AND sent_at_ms > 0 AND sent_at_ms + ?2 <= ?3",
                rusqlite::params![conv, *ttl as i64, now_ms as i64],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Newest-first page (`before_seq = 0` = latest), returned ascending so
    /// the caller prepends directly; `limit`-sized pages, shorter page = end.
    pub fn page(
        &self,
        conv_id: &str,
        before_seq: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<LocalRow>> {
        let limit = limit.clamp(1, 500) as i64;
        let mut rows = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT seq, conv_id, sender, kind, text, media_id, payload, msg_id, quote_of, sent_at_ms
                 FROM local_messages
                 WHERE conv_id = ?1 AND (?2 = 0 OR seq < ?2)
                 ORDER BY seq DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(rusqlite::params![conv_id, before_seq, limit], |r| {
                let payload_text: Option<String> = r.get(6)?;
                Ok(LocalRow {
                    seq: r.get(0)?,
                    conv_id: r.get(1)?,
                    sender: r.get(2)?,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                    media_id: r.get(5)?,
                    payload: payload_text.and_then(|t| serde_json::from_str(&t).ok()),
                    msg_id: r.get(7)?,
                    quote_of: r.get(8)?,
                    sent_at_ms: r.get(9)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        rows.reverse();
        Ok(rows)
    }
}

/// 桌面端的秘密聊天状态：Olm 账户 + 落盘路径 + 设备密钥。
///
/// 单独包一层是因为 `Crypto` 的棘轮**每发/每收一条都会变**，而磁盘上的副本
/// 必须跟着变：只在退出时保存，等于"崩溃后所有会话都解不开"。所以每次
/// 变动都调 [`SecretStore::persist`]。
pub(crate) struct SecretStore {
    pub(crate) crypto: Crypto,
    key: [u8; DEVICE_KEY_BYTES],
    path: std::path::PathBuf,
}

impl SecretStore {
    const SNAPSHOT_FILE: &'static str = "secret-account.bin";
    const KEY_FILE: &'static str = "secret-device.key";

    /// 打开已有状态。`Ok(None)` = 这台设备还没有秘密聊天账户。
    ///
    /// **有快照但没有密钥文件时报错**：那是"数据在、解不开"，不是"新设备"。
    /// 悄悄新建一个身份会让对方的安全码变化而毫无提示。
    pub(crate) fn open(dir: &std::path::Path) -> Result<Option<Self>, String> {
        let path = dir.join(Self::SNAPSHOT_FILE);
        let key_path = dir.join(Self::KEY_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let key = match std::fs::read(&key_path) {
            Ok(bytes) if bytes.len() == DEVICE_KEY_BYTES => {
                let mut key = [0u8; DEVICE_KEY_BYTES];
                key.copy_from_slice(&bytes);
                key
            }
            _ => {
                return Err(format!(
                    "秘密聊天密钥文件缺失或损坏（{}）：无法恢复身份，请删除 {} 后重新启用（会让对方的安全码变化）",
                    key_path.display(),
                    path.display()
                ))
            }
        };
        let blob = std::fs::read(&path).map_err(|e| format!("读秘密聊天快照失败：{e}"))?;
        let plain = wxlike_core::e2ee::open_at_rest(&key, &blob).map_err(|e| e.to_string())?;
        let crypto = Crypto::restore(&plain).map_err(|e| e.to_string())?;
        Ok(Some(Self { crypto, key, path }))
    }

    /// 新建账户（或复用已有的设备密钥）。
    pub(crate) fn create(dir: &std::path::Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建数据目录失败：{e}"))?;
        let key_path = dir.join(Self::KEY_FILE);
        let key = match std::fs::read(&key_path) {
            Ok(bytes) if bytes.len() == DEVICE_KEY_BYTES => {
                let mut key = [0u8; DEVICE_KEY_BYTES];
                key.copy_from_slice(&bytes);
                key
            }
            _ => {
                let mut key = [0u8; DEVICE_KEY_BYTES];
                rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key);
                write_private(&key_path, &key)?;
                key
            }
        };
        Ok(Self {
            crypto: Crypto::new(wxlike_core::e2ee::Device::new()),
            key,
            path: dir.join(Self::SNAPSHOT_FILE),
        })
    }

    /// 原子写：先写 `.tmp` 再 rename，避免"写一半被杀"留下半截快照。
    pub(crate) fn persist(&self) -> Result<(), String> {
        let plain = self.crypto.snapshot().map_err(|e| e.to_string())?;
        let blob = wxlike_core::e2ee::seal_at_rest(&self.key, &plain).map_err(|e| e.to_string())?;
        write_private(&self.path, &blob)
    }
}

/// 写文件并收紧权限（Unix 0600）。Windows 上靠用户目录 ACL，这里只管 Unix。
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("替换 {} 失败：{e}", path.display()))?;
    Ok(())
}
