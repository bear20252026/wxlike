//! SQLite access for the platform data plane.
//!
//! One `Connection` behind a `Mutex` (the task book's required shape): callers
//! lock, run fully-synchronous SQL, and drop the guard before any `await`, so
//! the guard is never held across a suspension point. Every statement is
//! parameterized — string-built SQL is banned by the architecture rules.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;

use crate::error::Result;

#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<Connection>>,
}

impl Db {
    /// `None` opens an in-memory database (tests, single-process demos).
    pub fn open(path: Option<&str>) -> Result<Self> {
        let mut conn = match path {
            Some(p) => {
                if let Some(dir) = std::path::Path::new(p).parent() {
                    if !dir.as_os_str().is_empty() {
                        std::fs::create_dir_all(dir)?;
                    }
                }
                Connection::open(p)
            }
            None => Connection::open_in_memory(),
        }?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            "#,
        )?;
        // WP-13 slow-query log: SQLite itself times every statement, so the
        // threshold applies to what actually ran, not to lock acquisition.
        conn.profile(Some(slow_sql_log));
        let db = Self {
            inner: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        db.migrate_friends_meta()?;
        db.migrate_groups_admin()?;
        db.migrate_security_columns()?;
        db.migrate_p4()?;
        db.migrate_communities()?;
        db.migrate_prekey_signature()?;
        db.migrate_g05()?;
        Ok(db)
    }

    /// AR-1（第四轮审计 C-1）：prekey 目录补签名列。老库的既有 bundle 无
    /// 签名，客户端验签 fail-closed 会拒绝它们——属主重新 publish 即恢复，
    /// 这是把"服务端可换身份密钥"的口子关上的代价，见 docs/threat-model.md。
    /// G05 内容/社交/收藏/检索：新增表 + 对既有 moments/favorites 的增量列。
    ///
    /// 回滚兼容：所有新表都是 `IF NOT EXISTS`，新列只增不删；旧二进制读带新列
    /// 的库时按显式列名查询，不受影响。删除本迁移的代码**不会**回收列，也不需要。
    fn migrate_g05(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(SCHEMA_G05)?;
        for (table, col, ddl) in [
            ("moments", "audience_group", "TEXT NOT NULL DEFAULT ''"),
            (
                "favorites",
                "source_kind",
                "TEXT NOT NULL DEFAULT 'message'",
            ),
            ("favorites", "source_id", "TEXT NOT NULL DEFAULT ''"),
            ("favorites", "snapshot_label", "TEXT NOT NULL DEFAULT ''"),
            ("favorites", "sort_order", "INTEGER NOT NULL DEFAULT 0"),
            ("favorites", "revoked", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !Self::has_column(&c, table, col)? {
                c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {ddl};"))?;
            }
        }
        // 必须在增量列之后建：旧库的 favorites 先补 source_kind/source_id 才能
        // 建这个部分唯一索引。
        c.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_favorites_object
                 ON favorites(user_id, source_kind, source_id) WHERE source_id <> '';",
        )?;
        Ok(())
    }

    fn has_column(c: &rusqlite::Connection, table: &str, col: &str) -> Result<bool> {
        let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in rows {
            if name? == col {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn migrate_prekey_signature(&self) -> Result<()> {
        let c = self.lock();
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(prekey_bundles)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        if !have.iter().any(|name| name == "signature") {
            c.execute_batch(
                "ALTER TABLE prekey_bundles ADD COLUMN signature TEXT NOT NULL DEFAULT '';",
            )?;
        }
        Ok(())
    }

    /// P4 表：人机校验、恢复码、对账、举报工单、秘密聊天设置、审核挂起队列，
    /// 以及会话的设备列（设备管理页要按设备下线）。
    fn migrate_p4(&self) -> Result<()> {
        // 建表之后还有"老库补列 + 回填"的活，所以这里要留住连接。
        let c = self.lock();
        c.execute_batch(SCHEMA_P4)?;

        // 会话表补设备列（设备管理页：按设备下线、最近活跃）
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(sessions)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        for (col, ddl) in [
            ("sid", "TEXT"),
            ("device_id", "TEXT NOT NULL DEFAULT ''"),
            ("device_name", "TEXT NOT NULL DEFAULT ''"),
            ("ip", "TEXT NOT NULL DEFAULT ''"),
            ("last_seen", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE sessions ADD COLUMN {col} {ddl};"))?;
            }
        }
        // media 补 conv_id（issue #14）：老库里的媒体没有归属会话，
        // 只能由上传者自己取——宁可严一点，也不要默认公开。
        {
            let mut have = Vec::new();
            {
                let mut stmt = c.prepare("PRAGMA table_info(media)")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
                for name in rows {
                    have.push(name?);
                }
            }
            if !have.iter().any(|name| name == "conv_id") {
                c.execute_batch("ALTER TABLE media ADD COLUMN conv_id TEXT NOT NULL DEFAULT '';")?;
            }
            if !have.iter().any(|name| name == "public") {
                c.execute_batch("ALTER TABLE media ADD COLUMN public INTEGER NOT NULL DEFAULT 0;")?;
            }
        }

        // 老会话没有 sid：补一个（12 hex），保证设备页能寻址
        let missing: Vec<String> = {
            let mut stmt = c.prepare("SELECT token FROM sessions WHERE sid IS NULL OR sid = ''")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for token in missing {
            let sid = crate::auth::random_hex(6);
            c.execute(
                "UPDATE sessions SET sid = ?1 WHERE token = ?2",
                rusqlite::params![sid, token],
            )?;
        }
        Ok(())
    }

    /// Zone C · C1/C3：频道级角色表 + 话题订阅的静音列。
    ///
    /// `channel_role` 是**新表**，直接建；`thread_subs.muted` 走"老库原地补列"
    /// （与 `migrate_groups_admin` 同一手法）：没有旧行要搬迁，缺列就是缺功能。
    ///
    /// 角色名写字符串而不是复用 `community_members` 的整数编号：社区级三档
    /// （0 成员 / 1 群主 / 2 管理员）里**没有 guest**，而频道级必须能表达
    /// "只看不能传附件"。整数字典两套并存迟早会有人拿错，字符串自带可读性。
    fn migrate_communities(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(
            r#"
            -- 频道级角色：有行才覆盖社区角色，无行则继承（见 communities.rs）
            CREATE TABLE IF NOT EXISTS channel_role (
                channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                role       TEXT NOT NULL CHECK (role IN ('owner','moderator','member','guest')),
                granted_by TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                PRIMARY KEY (channel_id, user_id)
            );
            CREATE INDEX IF NOT EXISTS idx_channel_role_user
                ON channel_role(user_id, channel_id);
            "#,
        )?;

        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(thread_subs)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        // 目前只有一个待补列。单元素 `for` 会被 clippy 拦（single_element_loop），
        // 直接展开；将来要补第二列时再改成 `vec![...]` 遍历。
        let (col, ddl) = ("muted", "INTEGER NOT NULL DEFAULT 0");
        if !have.iter().any(|name| name == col) {
            c.execute_batch(&format!("ALTER TABLE thread_subs ADD COLUMN {col} {ddl};"))?;
        }
        Ok(())
    }

    fn migrate(&self) -> Result<()> {
        self.lock().execute_batch(SCHEMA)?;
        Ok(())
    }

    /// P4 安全列（pw_algo/totp_*）：老库原地补列。
    fn migrate_security_columns(&self) -> Result<()> {
        let c = self.lock();
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(users)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        for (col, ddl) in [
            ("pw_algo", "TEXT NOT NULL DEFAULT 'pbkdf2'"),
            ("totp_secret", "TEXT"),
            ("totp_enabled", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE users ADD COLUMN {col} {ddl};"))?;
            }
        }
        Ok(())
    }

    /// P3 群管理列（公告/审批开关/全员禁言）：老库原地补列。
    fn migrate_groups_admin(&self) -> Result<()> {
        let c = self.lock();
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(groups)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        for (col, ddl) in [
            ("announcement", "TEXT NOT NULL DEFAULT ''"),
            ("approval", "INTEGER NOT NULL DEFAULT 0"),
            ("all_muted", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE groups ADD COLUMN {col} {ddl};"))?;
            }
        }
        Ok(())
    }

    /// P3 关系元数据列（星标/标签/分组）：老库原地补列，无重写。
    fn migrate_friends_meta(&self) -> Result<()> {
        let c = self.lock();
        let mut have = Vec::new();
        {
            let mut stmt = c.prepare("PRAGMA table_info(friends)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for name in rows {
                have.push(name?);
            }
        }
        for (col, ddl) in [
            ("starred", "INTEGER NOT NULL DEFAULT 0"),
            ("tags", "TEXT NOT NULL DEFAULT '[]'"),
            ("friend_group", "TEXT NOT NULL DEFAULT ''"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE friends ADD COLUMN {col} {ddl};"))?;
            }
        }
        Ok(())
    }

    /// A poisoned lock only happens if SQL panicked mid-statement; SQLite
    /// connections survive that, so recovering the guard beats crashing.
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Statements slower than this land in the log with their text (truncated).
const SLOW_SQL_MS: u64 = 100;

/// Profile callback (rusqlite `trace` feature). A plain fn, as required.
fn slow_sql_log(sql: &str, elapsed: std::time::Duration) {
    if elapsed.as_millis() >= SLOW_SQL_MS as u128 {
        let text: String = sql.chars().take(160).collect();
        tracing::warn!(elapsed_ms = elapsed.as_millis() as u64, stmt = %text, "slow sql");
    }
}

// G00-02 纯移动拆分：全部建表 SQL 移至子模块 db/schema.rs（内容与 main
// 提交态逐字节一致）。
mod schema;
use schema::{SCHEMA, SCHEMA_P4};

/// G05：内容、社交、收藏与检索。表名按域加前缀，避免与平台既有表混淆。
const SCHEMA_G05: &str = r#"
            -- G05-01 朋友圈草稿：发布前的文本/媒体/可见性可反复保存，
            -- 发布成功后客户端删除；表本身不含"已发布"状态。
            CREATE TABLE IF NOT EXISTS moment_drafts (
                id             TEXT PRIMARY KEY,
                author         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text           TEXT NOT NULL DEFAULT '',
                media          TEXT NOT NULL DEFAULT '[]',
                visibility     TEXT NOT NULL DEFAULT 'friends',
                audience_group TEXT NOT NULL DEFAULT '',
                updated_at     INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_moment_drafts_author
                ON moment_drafts(author, updated_at);

            -- 内容 -> 媒体引用。删除内容时同步删行；用于回答"附件还被谁引用"
            -- 以及未来按引用计数清理孤儿对象。唯一约束让重复发布幂等。
            CREATE TABLE IF NOT EXISTS content_media_refs (
                media_id   TEXT NOT NULL,
                ref_kind   TEXT NOT NULL,
                ref_id     TEXT NOT NULL,
                owner      TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (ref_kind, ref_id, media_id)
            );
            CREATE INDEX IF NOT EXISTS idx_cmr_media ON content_media_refs(media_id);

            -- G05-02 发布者（一个用户一个发布者；不承诺导入微信后台）
            CREATE TABLE IF NOT EXISTS publishers (
                id              TEXT PRIMARY KEY,
                owner           TEXT NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
                name            TEXT NOT NULL,
                bio             TEXT NOT NULL DEFAULT '',
                avatar_media_id TEXT NOT NULL DEFAULT '',
                created_at      INTEGER NOT NULL
            );

            -- 文章：doc 是受限文档结构的 JSON，body_html 是服务端渲染结果。
            -- seq 同时是订阅未读游标（只增不减）。
            CREATE TABLE IF NOT EXISTS articles (
                seq          INTEGER PRIMARY KEY AUTOINCREMENT,
                id           TEXT NOT NULL UNIQUE,
                publisher_id TEXT NOT NULL REFERENCES publishers(id) ON DELETE CASCADE,
                author       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                title        TEXT NOT NULL DEFAULT '',
                doc          TEXT NOT NULL DEFAULT '{}',
                body_text    TEXT NOT NULL DEFAULT '',
                body_html    TEXT NOT NULL DEFAULT '',
                state        TEXT NOT NULL DEFAULT 'draft',
                version      INTEGER NOT NULL DEFAULT 1,
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL,
                published_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_articles_pub
                ON articles(publisher_id, seq);
            CREATE INDEX IF NOT EXISTS idx_articles_state
                ON articles(state, seq);

            CREATE TABLE IF NOT EXISTS article_versions (
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                version    INTEGER NOT NULL,
                doc        TEXT NOT NULL,
                saved_at   INTEGER NOT NULL,
                PRIMARY KEY (article_id, version)
            );

            -- 订阅：read_cursor = 已读到的文章 seq。未读 = 比它大的已发布文章。
            CREATE TABLE IF NOT EXISTS publisher_follows (
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                publisher_id TEXT NOT NULL REFERENCES publishers(id) ON DELETE CASCADE,
                read_cursor  INTEGER NOT NULL DEFAULT 0,
                created_at   INTEGER NOT NULL,
                PRIMARY KEY (user_id, publisher_id)
            );
            CREATE TABLE IF NOT EXISTS article_reads (
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                read_at    INTEGER NOT NULL,
                PRIMARY KEY (user_id, article_id)
            );
            -- 在看/分享/收藏是三个独立动作，各自一行。
            CREATE TABLE IF NOT EXISTS article_reactions (
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind       TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (article_id, user_id, kind)
            );

            -- G05-03 笔记：version 是乐观锁；写入必须带 expected_version。
            CREATE TABLE IF NOT EXISTS notes (
                id         TEXT PRIMARY KEY,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                title      TEXT NOT NULL DEFAULT '',
                body       TEXT NOT NULL DEFAULT '',
                version    INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_notes_user ON notes(user_id, updated_at);
            CREATE TABLE IF NOT EXISTS note_versions (
                note_id  TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                version  INTEGER NOT NULL,
                title    TEXT NOT NULL,
                body     TEXT NOT NULL,
                saved_at INTEGER NOT NULL,
                PRIMARY KEY (note_id, version)
            );

            -- 表情包：包级许可字段用于"版权"约束；安装是每用户独立行。
            CREATE TABLE IF NOT EXISTS sticker_packs (
                id         TEXT PRIMARY KEY,
                owner      TEXT NOT NULL DEFAULT '',
                name       TEXT NOT NULL,
                license    TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stickers (
                id         TEXT PRIMARY KEY,
                pack_id    TEXT NOT NULL REFERENCES sticker_packs(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                name       TEXT NOT NULL DEFAULT '',
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_stickers_pack ON stickers(pack_id, sort_order);
            CREATE TABLE IF NOT EXISTS user_stickers (
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                sticker_id   TEXT NOT NULL REFERENCES stickers(id) ON DELETE CASCADE,
                sort_order   INTEGER NOT NULL DEFAULT 0,
                installed_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, sticker_id)
            );

            -- G05-04 视频号：发布记录只是引用；能否播以 video_refs 的
            -- state='succeeded' 为准（转码门禁）。
            CREATE TABLE IF NOT EXISTS video_posts (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                id         TEXT NOT NULL UNIQUE,
                author     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                caption    TEXT NOT NULL DEFAULT '',
                state      TEXT NOT NULL DEFAULT 'published',
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_video_posts_author
                ON video_posts(author, seq);
            CREATE INDEX IF NOT EXISTS idx_video_posts_state
                ON video_posts(state, seq);
            CREATE TABLE IF NOT EXISTS video_likes (
                video_id   TEXT NOT NULL REFERENCES video_posts(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (video_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS video_comments (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                id         TEXT NOT NULL UNIQUE,
                video_id   TEXT NOT NULL REFERENCES video_posts(id) ON DELETE CASCADE,
                author     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text       TEXT NOT NULL,
                deleted    INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_video_comments
                ON video_comments(video_id, seq);
            CREATE TABLE IF NOT EXISTS video_follows (
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                creator    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, creator)
            );

            -- G05-05 公开内容索引：只放可被"公开检索"投影的字段。
            -- 写入与内容发布同事务、删除同步，所以不存在异步落后窗口。
            CREATE TABLE IF NOT EXISTS content_index (
                doc_id     TEXT PRIMARY KEY,
                kind       TEXT NOT NULL,
                owner      TEXT NOT NULL,
                audience   TEXT NOT NULL DEFAULT 'public',
                group_name TEXT NOT NULL DEFAULT '',
                title      TEXT NOT NULL DEFAULT '',
                body       TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_content_index_recent
                ON content_index(created_at DESC, doc_id DESC);
            CREATE INDEX IF NOT EXISTS idx_content_index_kind
                ON content_index(kind, created_at DESC);

            -- 通知收件箱：read_at 在服务端，跨端共享。"事务事件生成"由调用方
            -- 与业务写同事务插入；dedupe_key 保证重试/重复点赞不刷屏。
            CREATE TABLE IF NOT EXISTS notifications (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                id          TEXT NOT NULL UNIQUE,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind        TEXT NOT NULL,
                actor       TEXT NOT NULL DEFAULT '',
                target_kind TEXT NOT NULL DEFAULT '',
                target_id   TEXT NOT NULL DEFAULT '',
                text        TEXT NOT NULL DEFAULT '',
                dedupe_key  TEXT NOT NULL DEFAULT '',
                created_at  INTEGER NOT NULL,
                read_at     INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_notifications_user
                ON notifications(user_id, seq DESC);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_dedupe
                ON notifications(user_id, dedupe_key) WHERE dedupe_key <> '';

            -- G05-06 状态：到期由服务端 now 判定；cleared_at 保留审计/事件痕迹。
            CREATE TABLE IF NOT EXISTS statuses (
                id                  TEXT PRIMARY KEY,
                owner               TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind                TEXT NOT NULL,
                text                TEXT NOT NULL DEFAULT '',
                media_id            TEXT NOT NULL DEFAULT '',
                background_media_id TEXT NOT NULL DEFAULT '',
                audience            TEXT NOT NULL DEFAULT 'friends',
                version             INTEGER NOT NULL DEFAULT 1,
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL,
                expires_at          INTEGER NOT NULL,
                cleared_at          INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_statuses_owner
                ON statuses(owner, updated_at DESC);
            CREATE TABLE IF NOT EXISTS status_background_drafts (
                owner      TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS status_reactions (
                status_id  TEXT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (status_id, user_id)
            );
            "#;
