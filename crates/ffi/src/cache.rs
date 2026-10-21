//! 本地渲染缓存：断网时应用还能显示什么。
//!
//! 它**不是**第二套消息库。排序、seq、未读、去重的真相在服务端信箱
//! （`crates/message`）；这张表只存"上一次在线时看到过的行"，好让飞行模式下
//! 打开应用不是一片空白。也因此它的写入永远来自服务端回复或已经服务端
//! 确认过的发送，不在这里做业务判断。
//!
//! 键是 `(conv_id, msg_id)` 而不是 `(conv_id, seq)`：发送成功时服务端已经
//! 返回了 `msg_id`，此时 seq 还不知道，先按"最大 seq"占位写一行；
//! 之后这一条从历史里回来时，同一个 `msg_id` 被就地更新成真实 seq。
//! 用 seq 做键的话这两行会同时存在，UI 就出现重复气泡。

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use rusqlite::{params, Connection, OptionalExtension};

use crate::types::{ChatMessage, ConversationRow};

/// 乐观写入的占位 seq。
///
/// 两个约束夹出了这个值：它必须**严格小于**"给我最新一页"的游标（`i64::MAX`），
/// 否则刚发出去的那条会被 `seq < cursor` 过滤掉、在离线页里凭空消失；
/// 同时又必须远大于任何真实 seq，才能排在最下面，等真实 seq 回来时被就地更新。
const LOCAL_TENTATIVE_SEQ: i64 = i64::MAX / 2;

pub struct Cache {
    conn: Mutex<Connection>,
}

impl Cache {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| anyhow::anyhow!("open mobile cache db {}: {e}", path.display()))?;
        // outbox / 本缓存 / 将来的桌面 LocalStore 共用一个文件，写锁竞争靠这两个
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS messages (
                conv_id     TEXT    NOT NULL,
                msg_id      TEXT    NOT NULL,
                seq         INTEGER NOT NULL,
                sender      TEXT    NOT NULL,
                kind        TEXT    NOT NULL,
                text        TEXT    NOT NULL DEFAULT '',
                media_id    TEXT    NOT NULL DEFAULT '',
                sent_at_ms  INTEGER NOT NULL,
                withdrawn   INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (conv_id, msg_id)
            );
            CREATE INDEX IF NOT EXISTS messages_page ON messages(conv_id, seq DESC);

            CREATE TABLE IF NOT EXISTS conversations (
                conv_id   TEXT PRIMARY KEY,
                kind      TEXT NOT NULL,
                title     TEXT NOT NULL,
                members   INTEGER NOT NULL,
                unread    INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );

            -- token 与 user_id：应用重启后免登录用（见 WxlikeClient::create）
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )
        .map_err(|e| anyhow::anyhow!("mobile cache schema: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn upsert(&self, m: &ChatMessage) -> rusqlite::Result<usize> {
        self.c().execute(
            "INSERT INTO messages(conv_id, msg_id, seq, sender, kind, text, media_id, sent_at_ms, withdrawn)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(conv_id, msg_id) DO UPDATE SET
               seq = excluded.seq,
               sender = excluded.sender,
               kind = excluded.kind,
               text = excluded.text,
               media_id = excluded.media_id,
               sent_at_ms = excluded.sent_at_ms,
               withdrawn = excluded.withdrawn",
            params![
                m.conv,
                m.msg_id,
                m.seq,
                m.sender,
                m.kind,
                m.text,
                m.media_id,
                m.sent_at_ms,
                m.withdrawn
            ],
        )
    }

    /// 把服务端回复的一批行写进本地库。返回写入条数。
    ///
    /// 没有 `msg_id` 的行**不写**：那是服务端还没确认的东西，落进缓存就成了
    /// 一条永远对不上号的孤儿。
    pub fn put_messages(&self, rows: &[ChatMessage]) -> usize {
        let mut written = 0;
        for m in rows {
            if m.msg_id.is_empty() {
                continue;
            }
            // 单行写失败（磁盘满、锁不住）不该让整页历史消失
            if self.upsert(m).is_ok() {
                written += 1;
            }
        }
        written
    }

    /// 一条服务端已接收的消息：先按占位 seq 落库，历史回来时会被真实 seq 覆盖。
    pub fn put_sent(&self, m: &ChatMessage) -> rusqlite::Result<usize> {
        let mut row = m.clone();
        if row.seq <= 0 {
            row.seq = LOCAL_TENTATIVE_SEQ;
        }
        self.upsert(&row)
    }

    /// 本地的一页历史，按 `seq` 升序返回（UI 直接画）。
    pub fn page_messages(
        &self,
        conv: &str,
        before_seq: i64,
        limit: i64,
        my_id: &str,
    ) -> rusqlite::Result<Vec<ChatMessage>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT conv_id, msg_id, seq, sender, kind, text, media_id, sent_at_ms, withdrawn
             FROM messages WHERE conv_id = ?1 AND seq < ?2
             ORDER BY seq DESC LIMIT ?3",
        )?;
        let limit = limit.clamp(1, 200);
        let rows = stmt.query_map(params![conv, before_seq, limit], |r| {
            Ok(ChatMessage {
                conv: r.get(0)?,
                msg_id: r.get(1)?,
                seq: r.get(2)?,
                sender: r.get(3)?,
                kind: r.get(4)?,
                text: r.get(5)?,
                media_id: r.get(6)?,
                sent_at_ms: r.get(7)?,
                withdrawn: r.get::<_, i64>(8)? != 0,
                // `mine` 不落库：换账号登录时它必须按新的我重算
                mine: false,
            })
        })?;
        let mut out: Vec<ChatMessage> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        out.reverse();
        for m in &mut out {
            m.mine = !my_id.is_empty() && m.sender == my_id;
        }
        Ok(out)
    }

    /// 整份替换会话列表：`conv.list` 是"当前我有哪些会话"的完整答案，
    /// 留着已经退出的群会显示一个不该存在的入口。
    pub fn put_conversations(&self, rows: &[ConversationRow]) -> rusqlite::Result<()> {
        let c = self.c();
        c.execute("DELETE FROM conversations", [])?;
        let now = wxlike_core::types::now_ms() as i64;
        for row in rows {
            c.execute(
                "INSERT INTO conversations(conv_id, kind, title, members, unread, updated_at_ms)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(conv_id) DO UPDATE SET
                   kind = excluded.kind, title = excluded.title,
                   members = excluded.members, unread = excluded.unread,
                   updated_at_ms = excluded.updated_at_ms",
                params![row.conv, row.kind, row.title, row.members, row.unread, now],
            )?;
        }
        Ok(())
    }

    pub fn conversations(&self, my_id: &str) -> rusqlite::Result<Vec<ConversationRow>> {
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT conv_id, kind, title, members, unread FROM conversations
             ORDER BY updated_at_ms DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ConversationRow {
                conv: r.get(0)?,
                kind: r.get(1)?,
                title: r.get(2)?,
                members: r.get(3)?,
                unread: r.get(4)?,
                pending: 0,
            })
        })?;
        let mut out = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        // 直聊的标题是对方 user id；自己的那一行不该出现在列表里
        if !my_id.is_empty() {
            out.retain(|row| !(row.kind == "direct" && row.title == my_id));
        }
        Ok(out)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.c().execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.c()
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get::<_, String>(0)
            })
            .optional()
    }

    /// 退出登录：连消息一起清掉。
    ///
    /// 本地正文是明文，只清 token 的话下一个人装同一个 APK 就能读到上一个人的
    /// 聊天记录——所以凭据和数据一起走。
    pub fn wipe(&self) -> rusqlite::Result<()> {
        let c = self.c();
        c.execute("DELETE FROM messages", [])?;
        c.execute("DELETE FROM conversations", [])?;
        c.execute("DELETE FROM meta", [])?;
        Ok(())
    }

    /// 只在测试里用：确认某一行现在是不是占位状态。
    #[cfg(test)]
    pub fn seq_of(&self, conv: &str, msg_id: &str) -> rusqlite::Result<Option<i64>> {
        self.c()
            .query_row(
                "SELECT seq FROM messages WHERE conv_id = ?1 AND msg_id = ?2",
                params![conv, msg_id],
                |r| r.get(0),
            )
            .optional()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(conv: &str, msg_id: &str, seq: i64, sender: &str, text: &str) -> ChatMessage {
        ChatMessage {
            conv: conv.to_string(),
            msg_id: msg_id.to_string(),
            seq,
            sender: sender.to_string(),
            kind: "text".to_string(),
            text: text.to_string(),
            media_id: String::new(),
            sent_at_ms: 1_700_000_000_000,
            withdrawn: false,
            mine: false,
        }
    }

    /// 没有 msg_id 的行不落地：它对应一条服务端还没接收的消息，
    /// 落进缓存就是一条永远对不上号的孤儿。
    #[test]
    fn rows_without_msg_id_are_not_cached() {
        let cache = Cache::open(Path::new(":memory:")).expect("open");
        let mut ghost = msg("d-a~b", "", 7, "a", "幽灵");
        ghost.msg_id.clear();
        assert_eq!(cache.put_messages(&[ghost]), 0);
        assert_eq!(cache.put_messages(&[msg("d-a~b", "m1", 7, "a", "真")]), 1);
        assert_eq!(
            cache
                .page_messages("d-a~b", i64::MAX, 50, "a")
                .unwrap()
                .len(),
            1
        );
    }

    /// 发送成功先写占位行，历史回来后按同一个 msg_id 就地更新——
    /// 这是"气泡不重复"的唯一保证。
    #[test]
    fn optimistic_row_is_replaced_by_the_real_one_not_duplicated() {
        let cache = Cache::open(Path::new(":memory:")).expect("open");
        cache
            .put_sent(&msg("d-a~b", "m9", 0, "alice", "hi"))
            .expect("put_sent");
        assert_eq!(
            cache.seq_of("d-a~b", "m9").unwrap(),
            Some(LOCAL_TENTATIVE_SEQ),
            "未确认 seq 时应该占在最下面"
        );
        assert_eq!(
            cache.put_messages(&[msg("d-a~b", "m9", 42, "alice", "hi")]),
            1,
            "同一条消息按 msg_id 就地更新"
        );
        assert_eq!(cache.seq_of("d-a~b", "m9").unwrap(), Some(42));
        let page = cache.page_messages("d-a~b", i64::MAX, 50, "alice").unwrap();
        assert_eq!(page.len(), 1, "同一条消息不该出现两次");
        assert_eq!(page[0].seq, 42);
    }

    /// `mine` 是每次读取按"当前的我"重算的，不落库：
    /// 换账号登录后同一条消息的归属会变。
    #[test]
    fn mine_is_recomputed_per_reader_not_stored() {
        let cache = Cache::open(Path::new(":memory:")).expect("open");
        cache.put_messages(&[msg("d-a~b", "m1", 1, "alice", "hi")]);
        let as_alice = cache.page_messages("d-a~b", i64::MAX, 50, "alice").unwrap();
        let as_bob = cache.page_messages("d-a~b", i64::MAX, 50, "bob").unwrap();
        assert!(as_alice[0].mine);
        assert!(!as_bob[0].mine);
    }

    #[test]
    fn pages_are_returned_ascending_and_stop_before_the_cursor() {
        let cache = Cache::open(Path::new(":memory:")).expect("open");
        cache.put_messages(&[
            msg("g-1", "m1", 1, "a", "一"),
            msg("g-1", "m2", 2, "a", "二"),
            msg("g-1", "m3", 3, "a", "三"),
        ]);
        let page = cache.page_messages("g-1", i64::MAX, 2, "me").unwrap();
        assert_eq!(
            page.iter().map(|m| m.seq).collect::<Vec<_>>(),
            vec![2, 3],
            "取最新两页应是 seq 升序（3 在最下面）"
        );
        let older = cache.page_messages("g-1", 2, 10, "me").unwrap();
        assert_eq!(older.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn conversation_list_is_replaced_wholesale() {
        let cache = Cache::open(Path::new(":memory:")).expect("open");
        let row = |conv: &str| ConversationRow {
            conv: conv.to_string(),
            kind: "group".to_string(),
            title: "t".to_string(),
            members: 2,
            unread: 0,
            pending: 0,
        };
        cache.put_conversations(&[row("g-1"), row("g-2")]).unwrap();
        cache.put_conversations(&[row("g-3")]).unwrap();
        let ids: Vec<String> = cache
            .conversations("me")
            .unwrap()
            .into_iter()
            .map(|c| c.conv)
            .collect();
        assert_eq!(ids, vec!["g-3".to_string()], "已经退出的群不该留在列表里");
    }

    #[test]
    fn logout_wipes_messages_too() {
        let cache = Cache::open(Path::new(":memory:")).expect("open");
        cache.put_messages(&[msg("d-a~b", "m1", 1, "a", "私密")]);
        cache.set_meta("token", "t").unwrap();
        cache.wipe().unwrap();
        assert!(cache
            .page_messages("d-a~b", i64::MAX, 50, "a")
            .unwrap()
            .is_empty());
        assert_eq!(cache.meta("token").unwrap(), None);
    }

    #[test]
    fn meta_round_trips() {
        let cache = Cache::open(Path::new(":memory:")).expect("open");
        assert_eq!(cache.meta("user_id").unwrap(), None);
        cache.set_meta("user_id", "u-1").unwrap();
        cache.set_meta("user_id", "u-2").unwrap();
        assert_eq!(cache.meta("user_id").unwrap().as_deref(), Some("u-2"));
    }
}
