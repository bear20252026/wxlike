//! `wxlike-message` — the P3 read-diffusion mailbox (ADR-16/17, 总蓝图第四部分).
//!
//! 三支柱（direct / group / channel）共用**同一套信箱基础设施**：`owner_type`
//! 区分属主，序列号按属主独立、单点原子生成，只有 normal 通道占用序列号并进
//! 时间线（不变式 1–3）。存储今天是 SQLite（P6 迁 PG 分区），接口形状按
//! "按 seq 拉取 + 游标确认 + 变更事件流"设计，即 PG 分区版可直接平移。
//!
//! 数据模型（蓝图 4.1 的直接落法）：
//! * `mailbox` — 每属主有序信箱（normal 通道独占），`msg_id` UNIQUE 去重；
//! * `owner_seq` — 单点原子序列号（`UPDATE … RETURNING`，多写节点禁止各自生成）；
//! * `read_cursor` — 公共信箱不记个人进度，未读 = 最新 seq − 游标（ZSET 差值
//!   的 SQLite 过渡等价，接口签名不变）；
//! * `msg_user_state` — 删除/置顶/标记只对自己生效，不动物理信箱；
//! * `mailbox_change` — 信箱变更事件（撤回/批量撤回/编辑态），离线端重连按
//!   version 补同步（第六轮补充 17：读扩散下撤回 = 逻辑删除 + 变更广播）；
//! * `ephemeral` — ephemeral 通道：落库、不进时间线、不占序列号，只更新既有
//!   卡片（红包领取状态、广告回执）。
//!
//! 消息类型注册表（ADR-17）在编译期定死：每个 type_id 固定通道类别，append
//! 时校验，绕过注册表的帧直接拒绝。

use std::sync::{Mutex, MutexGuard, PoisonError};

use thiserror::Error;

use rusqlite::{params, Connection, OptionalExtension as _};
use wxlike_core::msgmeta::rich_kind;

/// Owner kinds (ADR-16). direct owners are normalized user pairs
/// (`d-{a}~{b}`), groups are group ids, channels are channel ids — the
/// string forms live with the social layer; this crate only needs the kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerType {
    Direct,
    Group,
    Channel,
}

impl OwnerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OwnerType::Direct => "direct",
            OwnerType::Group => "group",
            OwnerType::Channel => "channel",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "direct" => Some(OwnerType::Direct),
            "group" => Some(OwnerType::Group),
            "channel" => Some(OwnerType::Channel),
            _ => None,
        }
    }
}

/// Delivery channels (ADR-17). Only `Normal` occupies a seq and the timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelClass {
    /// 信箱 + 序列号 + 持久化。
    Normal,
    /// 不占序列号、只投在线 —— 本 crate **不存储**，投递是网关的事。
    HighPriority,
    /// 落库、不进时间线，只更新既有卡片。
    Ephemeral,
}

/// 消息类型注册表（ADR-17）：type_id → 通道类别。
///
/// 首批注册 P1/P2 已有的业务消息；新增类型必须在此登记，否则 `append`
/// 拒绝——注册表是通道语义的唯一裁决点，避免各调用点各自理解。
pub fn channel_class_of(type_id: &str) -> Option<ChannelClass> {
    match type_id {
        // normal: 进时间线的全部内容形态
        "text"
        | "system"
        | "image"
        | rich_kind::FILE
        | rich_kind::STICKER
        | rich_kind::VOICE
        | rich_kind::VIDEO
        | "msg.forward"
        | "msg.merge"
        | "miniapp.card"
        | "ad.card"
        | "wallet.redpacket" => Some(ChannelClass::Normal),
        // high_priority: 网关直投，不落库
        "typing" | "rtc-signal" | "live.signal" | "live.danmaku" => {
            Some(ChannelClass::HighPriority)
        }
        // ephemeral: 卡片状态回写
        "wallet.redpacket.claim" | "ad.receipt" | "msg.edit.state" => Some(ChannelClass::Ephemeral),
        _ => None,
    }
}

/// Envelope → 时间线条目 `(type_id, payload)`。
///
/// `None` 表示**不进时间线**：回执、正在输入、撤回（撤回走 `withdraw`）、
/// 以及注册表里没有的类型。宁可漏镜像也不要塞错——信箱是未读与同步的唯一
/// 真相，脏数据会一直跟着用户。
pub fn mirror_of(env: &wxlike_core::Envelope) -> Option<(String, String)> {
    use wxlike_core::types::MsgBody;
    match &env.body {
        MsgBody::Text(text) => Some(("text".to_string(), text.clone())),
        // 系统提示（入群/改名/撤回占位）也在时间线上，要计入未读
        MsgBody::Notice(text) => Some(("system".to_string(), text.clone())),
        MsgBody::Custom { kind, payload } => match channel_class_of(kind) {
            Some(ChannelClass::Normal) => Some((kind.clone(), payload.to_string())),
            // high_priority / ephemeral / 未注册：不进时间线
            _ => None,
        },
        // 裸字节没有类型信息（历史遗留的 Blob 路径）：不猜，跳过
        MsgBody::Bytes(_) | MsgBody::Recall { .. } => None,
    }
}

#[derive(Debug, Error)]
pub enum MailError {
    #[error("unregistered message type {0:?} (add it to the registry first)")]
    UnregisteredType(String),
    #[error("type {0:?} is a {1:?} message and must not enter the normal timeline")]
    WrongChannel(String, &'static str),
    #[error("unknown owner kind {0:?}")]
    BadOwner(String),
    #[error("storage failure: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("withdraw refused: unknown message or not the author's own")]
    WithdrawRefused,
}

pub type Result<T> = std::result::Result<T, MailError>;

/// One timeline row (normal channel). Recalled rows carry the placeholder —
/// the original body is overwritten, exactly like the transitional hub store.
#[derive(Clone, Debug, PartialEq)]
pub struct MailMessage {
    pub seq: u64,
    pub msg_id: String,
    pub sender: String,
    pub type_id: String,
    /// JSON payload text (bincode-free: the mailbox serves HTTP clients directly).
    pub payload: String,
    pub sent_at_ms: u64,
    pub withdrawn: bool,
}

/// A mailbox change event (recall / batch recall / edit-state), versioned for
/// offline resync.
#[derive(Clone, Debug, PartialEq)]
pub struct MailChange {
    pub version: u64,
    pub msg_id: String,
    pub kind: String,
    pub actor: String,
    pub at_ms: u64,
}

/// An ephemeral-channel card update (latest state wins by msg_id of its own).
#[derive(Clone, Debug, PartialEq)]
pub struct CardUpdate {
    pub rowid: i64,
    pub owner: (OwnerType, String),
    pub type_id: String,
    pub payload: String,
    pub target_msg_id: String,
    pub sent_at_ms: u64,
}

pub struct MessageStore {
    conn: Mutex<Connection>,
}

impl MessageStore {
    /// `None` opens in-memory (tests / demos); a path persists to disk.
    pub fn open(path: Option<&str>) -> Result<Self> {
        let conn = match path {
            Some(p) => {
                if let Some(dir) = std::path::Path::new(p).parent() {
                    if !dir.as_os_str().is_empty() {
                        std::fs::create_dir_all(dir)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    }
                }
                Connection::open(p)?
            }
            None => Connection::open_in_memory()?,
        };
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;

            -- 单点原子序列号：每属主一行，分配即 UPDATE … RETURNING
            CREATE TABLE IF NOT EXISTS owner_seq (
                owner_type TEXT NOT NULL,
                owner_id   TEXT NOT NULL,
                seq        INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (owner_type, owner_id)
            );

            -- normal 通道时间线（唯一占 seq 的表）
            CREATE TABLE IF NOT EXISTS mailbox (
                owner_type TEXT NOT NULL,
                owner_id   TEXT NOT NULL,
                seq        INTEGER NOT NULL,
                msg_id     TEXT NOT NULL,
                sender     TEXT NOT NULL,
                type_id    TEXT NOT NULL,
                payload    TEXT NOT NULL DEFAULT '',
                sent_at_ms INTEGER NOT NULL,
                withdrawn  INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (owner_type, owner_id, seq)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_mailbox_msgid ON mailbox(msg_id);
            CREATE INDEX IF NOT EXISTS idx_mailbox_owner ON mailbox(owner_type, owner_id, seq);

            -- 已读游标：公共信箱 + 个人进度，未读数是差值不是计数器
            CREATE TABLE IF NOT EXISTS read_cursor (
                owner_type  TEXT NOT NULL,
                owner_id    TEXT NOT NULL,
                user_id     TEXT NOT NULL,
                last_ack_seq INTEGER NOT NULL DEFAULT 0,
                updated_at  INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (owner_type, owner_id, user_id)
            );

            -- 消息个人状态：只对自己生效
            CREATE TABLE IF NOT EXISTS msg_user_state (
                owner_type TEXT NOT NULL,
                owner_id   TEXT NOT NULL,
                seq        INTEGER NOT NULL,
                user_id    TEXT NOT NULL,
                state_type TEXT NOT NULL,
                PRIMARY KEY (owner_type, owner_id, seq, user_id, state_type)
            );

            -- 信箱变更事件：撤回/批量撤回/编辑态，按 version 增量同步
            CREATE TABLE IF NOT EXISTS mailbox_change (
                owner_type TEXT NOT NULL,
                owner_id   TEXT NOT NULL,
                version    INTEGER NOT NULL,
                msg_id     TEXT NOT NULL,
                kind       TEXT NOT NULL,
                actor      TEXT NOT NULL,
                at_ms      INTEGER NOT NULL,
                PRIMARY KEY (owner_type, owner_id, version)
            );

            -- ephemeral 通道：卡片状态回写（不进时间线，不占序列号）
            CREATE TABLE IF NOT EXISTS ephemeral (
                rowid_hint    INTEGER PRIMARY KEY AUTOINCREMENT,
                owner_type    TEXT NOT NULL,
                owner_id      TEXT NOT NULL,
                msg_id        TEXT NOT NULL,
                target_msg_id TEXT NOT NULL DEFAULT '',
                type_id       TEXT NOT NULL,
                payload       TEXT NOT NULL DEFAULT '',
                sent_at_ms    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ephemeral_owner ON ephemeral(owner_type, owner_id, rowid_hint);

            -- sync-service：成员变更日志（version 单调，五层差分的增量源）
            CREATE TABLE IF NOT EXISTS member_log (
                owner_type TEXT NOT NULL,
                owner_id   TEXT NOT NULL,
                version    INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                user_id    TEXT NOT NULL,
                payload    TEXT NOT NULL DEFAULT '',
                at_ms      INTEGER NOT NULL,
                PRIMARY KEY (owner_type, owner_id, version)
            );

            -- 成员快照：全量兜底与懒加载分页的真相源
            CREATE TABLE IF NOT EXISTS member_snapshot (
                owner_type TEXT NOT NULL,
                owner_id   TEXT NOT NULL,
                user_id    TEXT NOT NULL,
                PRIMARY KEY (owner_type, owner_id, user_id)
            );
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn owner_pair<'a>(&self, owner: (OwnerType, &'a str)) -> Result<(&'static str, &'a str)> {
        Ok((owner.0.as_str(), owner.1))
    }

    /// **normal 通道**追加：注册表校验 → 原子分配序列号 → 写入时间线。
    ///
    /// `msg_id` 冲突时返回 `Ok(None)`（at-least-once 投递下的重复不是错误）。
    /// seq 分配与写入在同一个连接锁内完成——单点生成，禁止多写节点各自分配。
    pub fn append(
        &self,
        owner: (OwnerType, &str),
        sender: &str,
        type_id: &str,
        msg_id: &str,
        payload: &str,
    ) -> Result<Option<u64>> {
        let (ot, oid) = self.owner_pair(owner)?;
        match channel_class_of(type_id) {
            Some(ChannelClass::Normal) => {}
            Some(ChannelClass::HighPriority) => {
                return Err(MailError::WrongChannel(
                    type_id.to_string(),
                    "high_priority",
                ))
            }
            Some(ChannelClass::Ephemeral) => {
                return Err(MailError::WrongChannel(type_id.to_string(), "ephemeral"))
            }
            None => return Err(MailError::UnregisteredType(type_id.to_string())),
        }
        let mut c = self.c();
        let tx = c.transaction()?;
        // 原子序列号：单语句完成取号（不变式 2）
        tx.execute(
            "INSERT INTO owner_seq(owner_type, owner_id, seq) VALUES(?1, ?2, 1)
             ON CONFLICT(owner_type, owner_id) DO UPDATE SET seq = seq + 1",
            params![ot, oid],
        )?;
        let seq: u64 = tx.query_row(
            "SELECT seq FROM owner_seq WHERE owner_type = ?1 AND owner_id = ?2",
            params![ot, oid],
            |r| r.get::<_, i64>(0).map(|v| v as u64),
        )?;
        let inserted = tx.execute(
            "INSERT INTO mailbox(owner_type, owner_id, seq, msg_id, sender, type_id, payload, sent_at_ms, withdrawn)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            params![
                ot,
                oid,
                seq as i64,
                msg_id,
                sender,
                type_id,
                payload,
                wxlike_core::types::now_ms() as i64
            ],
        );
        match inserted {
            Ok(_) => {
                tx.commit()?;
                Ok(Some(seq))
            }
            Err(e) if is_unique(&e) => {
                tx.rollback()?;
                Ok(None) // 重复投递：静默，不是错误
            }
            Err(e) => Err(e.into()),
        }
    }

    /// **high_priority 通道**显式拒绝落库的入口：存在性说明语义（网关直投），
    /// 调用方可据此做编译期/运行期双重防护。
    pub fn append_high_priority(&self, type_id: &str) -> Result<()> {
        match channel_class_of(type_id) {
            Some(ChannelClass::HighPriority) => Ok(()),
            Some(other) => Err(MailError::WrongChannel(
                type_id.to_string(),
                other.as_static(),
            )),
            None => Err(MailError::UnregisteredType(type_id.to_string())),
        }
    }

    /// **ephemeral 通道**：落库、不占序列号、不进时间线。`target_msg_id`
    /// 指向被更新的卡片（红包领取 → 红包卡片）。返回该属主内单调的 rowid，
    /// 客户端按 rowid 增量拉取。
    pub fn append_ephemeral(
        &self,
        owner: (OwnerType, &str),
        sender: &str,
        type_id: &str,
        msg_id: &str,
        target_msg_id: &str,
        payload: &str,
    ) -> Result<i64> {
        let (ot, oid) = self.owner_pair(owner)?;
        match channel_class_of(type_id) {
            Some(ChannelClass::Ephemeral) => {}
            Some(other) => {
                return Err(MailError::WrongChannel(
                    type_id.to_string(),
                    other.as_static(),
                ))
            }
            None => return Err(MailError::UnregisteredType(type_id.to_string())),
        }
        let _ = sender; // ephemeral 事件记录在 payload 内；保留签名对称性
        let c = self.c();
        c.execute(
            "INSERT INTO ephemeral(owner_type, owner_id, msg_id, target_msg_id, type_id, payload, sent_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ot,
                oid,
                msg_id,
                target_msg_id,
                type_id,
                payload,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// 倒序取一页：`before_seq` 之前的 `limit` 条（history 的翻页语义）。
    ///
    /// 返回 (升序 rows, next_before)：`next_before` 是这一页最老的 seq，
    /// 下一页拿它继续往前翻；`None` = 已经到头。
    ///
    /// 为什么单独一个方法而不是给 `fetch` 加参数：两者的游标语义相反
    /// （增量同步用 after，历史翻页用 before），混在一个签名里迟早传错。
    pub fn fetch_before(
        &self,
        owner: (OwnerType, &str),
        before_seq: u64,
        limit: usize,
    ) -> Result<(Vec<MailMessage>, Option<u64>)> {
        let (ot, oid) = self.owner_pair(owner)?;
        let limit = limit.clamp(1, 200);
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT seq, msg_id, sender, type_id, payload, sent_at_ms, withdrawn
               FROM mailbox
              WHERE owner_type = ?1 AND owner_id = ?2 AND (?3 = 0 OR seq < ?3)
              ORDER BY seq DESC LIMIT ?4",
        )?;
        let mut rows = stmt
            .query_map(params![ot, oid, before_seq as i64, limit as i64], |r| {
                Ok(MailMessage {
                    seq: r.get::<_, i64>(0)? as u64,
                    msg_id: r.get(1)?,
                    sender: r.get(2)?,
                    type_id: r.get(3)?,
                    payload: r.get(4)?,
                    sent_at_ms: r.get::<_, i64>(5)? as u64,
                    withdrawn: r.get::<_, i64>(6)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Ok((Vec::new(), None));
        }
        let next_before = rows[rows.len() - 1].seq;
        let exhausted = rows.len() < limit;
        rows.reverse();
        Ok((rows, if exhausted { None } else { Some(next_before) }))
    }

    /// 拉取属主某属主内 `after_seq` 之后的 timeline（升序、限量）。
    /// 返回 `next_after`：客户端持续翻页的游标；`None` = 已到头。
    pub fn fetch(
        &self,
        owner: (OwnerType, &str),
        after_seq: u64,
        limit: usize,
    ) -> Result<(Vec<MailMessage>, Option<u64>)> {
        let (ot, oid) = self.owner_pair(owner)?;
        let limit = limit.clamp(1, 1000) as i64;
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT seq, msg_id, sender, type_id, payload, sent_at_ms, withdrawn
             FROM mailbox WHERE owner_type = ?1 AND owner_id = ?2 AND seq > ?3
             ORDER BY seq LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![ot, oid, after_seq as i64, limit], row_to_mail)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() < limit as usize {
            return Ok((rows, None));
        }
        let next = rows.last().map(|m| m.seq);
        Ok((rows, next))
    }

    /// 补洞探测：返回属主当前最大 seq 与“客户端声称已见到的最大 seq”。
    /// 差值超过阈值即触发兜底全量（蓝图 4.3，OpenIM 五层的第 1/3 层）。
    pub fn head_seq(&self, owner: (OwnerType, &str)) -> Result<u64> {
        let (ot, oid) = self.owner_pair(owner)?;
        let c = self.c();
        Ok(c.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM mailbox WHERE owner_type = ?1 AND owner_id = ?2",
            params![ot, oid],
            |r| r.get::<_, i64>(0).map(|v| v as u64),
        )?)
    }

    /// 已读确认：游标只前进不后退（迟到的高游标被拒绝是协议层的错序防护）。
    pub fn ack(&self, owner: (OwnerType, &str), user: &str, seq: u64) -> Result<bool> {
        let (ot, oid) = self.owner_pair(owner)?;
        let c = self.c();
        let changed = c.execute(
            "UPDATE read_cursor SET last_ack_seq = ?4, updated_at = ?5
             WHERE owner_type = ?1 AND owner_id = ?2 AND user_id = ?3 AND last_ack_seq < ?4",
            params![
                ot,
                oid,
                user,
                seq as i64,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        if changed == 1 {
            return Ok(true);
        }
        // 无行或游标已更高：无行则插入（首次 ack 允许任意值）
        let has: Option<i64> = c
            .query_row(
                "SELECT last_ack_seq FROM read_cursor
                 WHERE owner_type = ?1 AND owner_id = ?2 AND user_id = ?3",
                params![ot, oid, user],
                |r| r.get(0),
            )
            .optional()?;
        if has.is_none() {
            c.execute(
                "INSERT INTO read_cursor(owner_type, owner_id, user_id, last_ack_seq, updated_at)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    ot,
                    oid,
                    user,
                    seq as i64,
                    wxlike_core::types::now_ms() as i64
                ],
            )?;
            return Ok(true);
        }
        Ok(false) // 已有更高游标：no-op
    }

    /// 未读数 = 属主最新 seq − 个人游标。线性差值是 ZSET 排名差的 SQLite 过渡
    /// 等价（蓝图 4.2），万人规模迁移 Redis 时此签名不变。
    pub fn unread(&self, owner: (OwnerType, &str), user: &str) -> Result<u64> {
        let (ot, oid) = self.owner_pair(owner)?;
        let c = self.c();
        let head: u64 = c.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM mailbox WHERE owner_type = ?1 AND owner_id = ?2",
            params![ot, oid],
            |r| r.get::<_, i64>(0).map(|v| v as u64),
        )?;
        let cursor: u64 = c
            .query_row(
                "SELECT last_ack_seq FROM read_cursor
                 WHERE owner_type = ?1 AND owner_id = ?2 AND user_id = ?3",
                params![ot, oid, user],
                |r| r.get::<_, i64>(0).map(|v| v as u64),
            )
            .optional()?
            .unwrap_or(0);
        Ok(head.saturating_sub(cursor))
    }

    /// 撤回（逻辑删除 + 变更事件）：只有作者本人在信箱内可行；目标不存在或
    /// 非本人消息时整体拒绝（与过渡 hub 语义一致， forged recall 无痕迹）。
    pub fn withdraw(&self, owner: (OwnerType, &str), msg_id: &str, actor: &str) -> Result<()> {
        let (ot, oid) = self.owner_pair(owner)?;
        let mut c = self.c();
        let tx = c.transaction()?;
        let touched = tx.execute(
            "UPDATE mailbox SET withdrawn = 1
             WHERE owner_type = ?1 AND owner_id = ?2 AND msg_id = ?3 AND sender = ?4 AND withdrawn = 0",
            params![ot, oid, msg_id, actor],
        )?;
        if touched != 1 {
            return Err(MailError::WithdrawRefused);
        }
        bump_change(&tx, ot, oid, msg_id, "recall", actor)?;
        tx.commit()?;
        Ok(())
    }

    /// 批量撤回（“一键撤回本次全部”）：全部成功才提交，否则整体回滚。
    /// 返回实际撤回的条数。
    pub fn withdraw_batch(
        &self,
        owner: (OwnerType, &str),
        msg_ids: &[&str],
        actor: &str,
    ) -> Result<usize> {
        let (ot, oid) = self.owner_pair(owner)?;
        let mut c = self.c();
        let tx = c.transaction()?;
        let mut done = 0usize;
        for msg_id in msg_ids {
            let touched = tx.execute(
                "UPDATE mailbox SET withdrawn = 1
                 WHERE owner_type = ?1 AND owner_id = ?2 AND msg_id = ?3 AND sender = ?4 AND withdrawn = 0",
                params![ot, oid, msg_id, actor],
            )?;
            if touched == 1 {
                done += 1;
                bump_change(&tx, ot, oid, msg_id, "recall", actor)?;
            }
        }
        if done != msg_ids.len() {
            return Err(MailError::WithdrawRefused); // 整批拒绝，不半提交
        }
        tx.commit()?;
        Ok(done)
    }

    /// 信箱变更事件流：`since_version` 之后的增量（离线端重连补同步——
    /// 撤回跨端状态同步的读扩散形态）。
    pub fn changes(
        &self,
        owner: (OwnerType, &str),
        since_version: u64,
        limit: usize,
    ) -> Result<(Vec<MailChange>, Option<u64>)> {
        let (ot, oid) = self.owner_pair(owner)?;
        let limit = limit.clamp(1, 1000) as i64;
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT version, msg_id, kind, actor, at_ms FROM mailbox_change
             WHERE owner_type = ?1 AND owner_id = ?2 AND version > ?3 ORDER BY version LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![ot, oid, since_version as i64, limit], |r| {
                Ok(MailChange {
                    version: r.get::<_, i64>(0)? as u64,
                    msg_id: r.get(1)?,
                    kind: r.get(2)?,
                    actor: r.get(3)?,
                    at_ms: r.get::<_, i64>(4)? as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let next = rows.last().map(|c| c.version);
        if rows.len() < limit as usize {
            return Ok((rows, None));
        }
        Ok((rows, next))
    }

    /// ephemeral 卡片更新增量拉取（rowid 游标）。
    pub fn fetch_ephemeral(
        &self,
        owner: (OwnerType, &str),
        after_rowid: i64,
        limit: usize,
    ) -> Result<(Vec<CardUpdate>, Option<i64>)> {
        let (ot, oid) = self.owner_pair(owner)?;
        let limit = limit.clamp(1, 1000) as i64;
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT rowid_hint, type_id, payload, target_msg_id, sent_at_ms
             FROM ephemeral WHERE owner_type = ?1 AND owner_id = ?2 AND rowid_hint > ?3
             ORDER BY rowid_hint LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![ot, oid, after_rowid, limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let next = rows
            .last()
            .map(|r| r.0)
            .filter(|_| rows.len() == limit as usize);
        if rows.is_empty() {
            return Ok((Vec::new(), None));
        }
        let updates = rows
            .into_iter()
            .map(
                |(rowid, type_id, payload, target_msg_id, sent_at_ms)| CardUpdate {
                    rowid,
                    owner: (owner.0, oid.to_string()),
                    type_id,
                    payload,
                    target_msg_id,
                    sent_at_ms: sent_at_ms as u64,
                },
            )
            .collect();
        Ok((updates, next))
    }

    /// 个人状态（删除/置顶/标记未读）：只动自己的视图。
    pub fn set_user_state(
        &self,
        owner: (OwnerType, &str),
        seq: u64,
        user: &str,
        state: &str,
    ) -> Result<()> {
        let (ot, oid) = self.owner_pair(owner)?;
        let c = self.c();
        c.execute(
            "INSERT OR IGNORE INTO msg_user_state(owner_type, owner_id, seq, user_id, state_type)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![ot, oid, seq as i64, user, state],
        )?;
        Ok(())
    }
}

/// 变更事件的 version 也走单点原子分配（与消息 seq 同机制、独立空间）。
fn bump_change(
    tx: &rusqlite::Transaction<'_>,
    ot: &str,
    oid: &str,
    msg_id: &str,
    kind: &str,
    actor: &str,
) -> rusqlite::Result<i64> {
    // change version 使用独立的 owner_seq 行（owner_id 加 '#change' 后缀）
    tx.execute(
        "INSERT INTO owner_seq(owner_type, owner_id, seq) VALUES(?1, ?2 || '#change', 1)
         ON CONFLICT(owner_type, owner_id) DO UPDATE SET seq = seq + 1",
        params![ot, oid],
    )?;
    let version: i64 = tx.query_row(
        "SELECT seq FROM owner_seq WHERE owner_type = ?1 AND owner_id = ?2 || '#change'",
        params![ot, oid],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO mailbox_change(owner_type, owner_id, version, msg_id, kind, actor, at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            ot,
            oid,
            version,
            msg_id,
            kind,
            actor,
            wxlike_core::types::now_ms() as i64
        ],
    )?;
    Ok(version)
}

fn row_to_mail(r: &rusqlite::Row<'_>) -> rusqlite::Result<MailMessage> {
    Ok(MailMessage {
        seq: r.get::<_, i64>(0)? as u64,
        msg_id: r.get(1)?,
        sender: r.get(2)?,
        type_id: r.get(3)?,
        payload: r.get(4)?,
        sent_at_ms: r.get::<_, i64>(5)? as u64,
        withdrawn: r.get::<_, i64>(6)? != 0,
    })
}

impl ChannelClass {
    fn as_static(&self) -> &'static str {
        match self {
            ChannelClass::Normal => "normal",
            ChannelClass::HighPriority => "high_priority",
            ChannelClass::Ephemeral => "ephemeral",
        }
    }
}

fn is_unique(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.contains("UNIQUE"),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// sync-service（OpenIM 五层差分契约的总蓝图落法）+ read-service 门槛
// ---------------------------------------------------------------------------

/// 成员变更日志行（蓝图 4.1 表 ③ 的 crate 内落法）。
#[derive(Clone, Debug, PartialEq)]
pub struct MemberChange {
    pub version: u64,
    pub kind: String,
    pub user_id: String,
    pub payload: String,
}

/// 五层同步决策：客户端报 `(自己持有的版本, 自己持有的成员数)`，服务端按
/// 契约给出下一动作。阈值：增量 > [`DELTA_MAX`] 时收口为全量（第 4 层）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncDirective {
    /// 第 1 层：版本一致，无事可做。
    UpToDate,
    /// 第 2 层：版本差在阈值内，按 `since_version` 拉增量。
    Delta { since_version: u64 },
    /// 第 3 层：版本链断裂（客户端版本超前/未知属主），全量快照兜底。
    FullResync,
    /// 第 4 层：版本差超过 [`DELTA_MAX`]，直接收口为全量（比逐段拉省往返）。
    BatchCollapse,
}

/// 增量拉取上限：超过它改走全量（第 4 层收口阈值）。
pub const DELTA_MAX: u64 = 200;

impl MessageStore {
    /// 成员变更入账（join/leave/role_change/mute…），version 单点原子。
    pub fn append_member_change(
        &self,
        owner: (OwnerType, &str),
        kind: &str,
        user_id: &str,
        payload: &str,
    ) -> Result<u64> {
        let (ot, oid) = self.owner_pair(owner)?;
        let mut c = self.c();
        let tx = c.transaction()?;
        let version = bump_change(&tx, ot, oid, user_id, kind, "system")? as u64;
        // 复用 mailbox_change 的 version 序列，但成员日志单独成表
        tx.execute(
            "INSERT INTO member_log(owner_type, owner_id, version, kind, user_id, payload, at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ot,
                oid,
                version as i64,
                kind,
                user_id,
                payload,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        tx.commit()?;
        Ok(version)
    }

    /// 成员日志头版本（摘要判断的基准）。
    pub fn member_head_version(&self, owner: (OwnerType, &str)) -> Result<u64> {
        let (ot, oid) = self.owner_pair(owner)?;
        let c = self.c();
        Ok(c.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM member_log
             WHERE owner_type = ?1 AND owner_id = ?2",
            params![ot, oid],
            |r| r.get::<_, i64>(0).map(|v| v as u64),
        )?)
    }

    /// 增量拉取（第 2 层）。
    pub fn member_changes_since(
        &self,
        owner: (OwnerType, &str),
        since_version: u64,
        limit: usize,
    ) -> Result<Vec<MemberChange>> {
        let (ot, oid) = self.owner_pair(owner)?;
        let limit = limit.clamp(1, 1000) as i64;
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT version, kind, user_id, payload FROM member_log
             WHERE owner_type = ?1 AND owner_id = ?2 AND version > ?3 ORDER BY version LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![ot, oid, since_version as i64, limit], |r| {
                Ok(MemberChange {
                    version: r.get::<_, i64>(0)? as u64,
                    kind: r.get(1)?,
                    user_id: r.get(2)?,
                    payload: r.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 五层契约的第 1/3/4 层：给定客户端摘要，决定下一步。
    pub fn sync_plan(
        &self,
        owner: (OwnerType, &str),
        client_version: u64,
    ) -> Result<SyncDirective> {
        let head = self.member_head_version(owner)?;
        if client_version > head {
            // 版本链断裂：客户端比服务端还新（换库/清库/伪造）→ 全量兜底
            return Ok(SyncDirective::FullResync);
        }
        if client_version == head {
            return Ok(SyncDirective::UpToDate);
        }
        if head - client_version > DELTA_MAX {
            return Ok(SyncDirective::BatchCollapse);
        }
        Ok(SyncDirective::Delta {
            since_version: client_version,
        })
    }

    /// 成员快照（第 3 层兜底 / 第 5 层懒加载的分页源）：按 user_id 排序分页，
    /// 页大小受控，万人群不做全量拉取。
    pub fn member_snapshot(
        &self,
        owner: (OwnerType, &str),
        after_user: &str,
        limit: usize,
    ) -> Result<(Vec<String>, Option<String>)> {
        let (ot, oid) = self.owner_pair(owner)?;
        let limit = limit.clamp(1, 1000) as i64;
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT user_id FROM member_snapshot
             WHERE owner_type = ?1 AND owner_id = ?2 AND user_id > ?3
             ORDER BY user_id LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(params![ot, oid, after_user, limit], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() < limit as usize {
            return Ok((rows, None));
        }
        let next = rows.last().cloned();
        Ok((rows, next))
    }

    /// 全量重建成员快照（member log 重放的责任在 community 层；这里只存真相）。
    /// 属主的**全部**成员（扇出用）。
    ///
    /// 与 `member_snapshot` 的区别：那个是给客户端"万人群懒加载"用的分页接口，
    /// 这里是服务端投递要的完整名单。名字分开是为了防止有人拿分页接口去扇出
    /// ——那会静默丢掉第 1000 人之后的所有成员。
    pub fn members(&self, owner: (OwnerType, &str)) -> Result<Vec<String>> {
        let (ot, oid) = self.owner_pair(owner)?;
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT user_id FROM member_snapshot
              WHERE owner_type = ?1 AND owner_id = ?2 ORDER BY user_id",
        )?;
        let rows = stmt
            .query_map(params![ot, oid], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn reset_member_snapshot(
        &self,
        owner: (OwnerType, &str),
        members: &[String],
    ) -> Result<()> {
        let (ot, oid) = self.owner_pair(owner)?;
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM member_snapshot WHERE owner_type = ?1 AND owner_id = ?2",
            params![ot, oid],
        )?;
        for user in members {
            tx.execute(
                "INSERT OR IGNORE INTO member_snapshot(owner_type, owner_id, user_id)
                 VALUES(?1, ?2, ?3)",
                params![ot, oid, user],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// read-service 门槛（蓝图第六轮 18）：已读回执只保留在 1:1 与 ≤500 人群，
/// 万人群/频道默认只做未读。这是默认关闭的开关，不是禁令——属主可显式开。
pub const READ_RECEIPT_MAX_MEMBERS: u64 = 500;

pub fn read_receipt_allowed(owner: OwnerType, member_count: u64) -> bool {
    match owner {
        OwnerType::Direct => true,
        OwnerType::Group | OwnerType::Channel => member_count <= READ_RECEIPT_MAX_MEMBERS,
    }
}
