//! `SqliteStorage` 的 schema 迁移链：建表 + 老库原地补列。
//!
//! 本文件是纯移动：从 `storage.rs` 的 `impl SqliteStorage` 里搬出来，方法体与
//! 注释逐字未改。顺序集中在 [`SqliteStorage::run_all`]，存储端口只调一次；
//! 以后接 PostgreSQL 时，迁移链与端口生命周期不会继续缠在同一个文件里。

use super::schema::{SCHEMA, SCHEMA_G05, SCHEMA_P4};
use super::SqliteStorage;
use crate::error::Result;

impl SqliteStorage {
    /// 按既有顺序跑完全部迁移。顺序本身是契约：先建基表，再补列/回填。
    pub(super) fn run_all(&self) -> Result<()> {
        self.migrate()?;
        self.migrate_friends_meta()?;
        self.migrate_groups_admin()?;
        self.migrate_security_columns()?;
        self.migrate_p4()?;
        self.migrate_communities()?;
        self.migrate_prekey_signature()?;
        self.migrate_identity()?;
        self.migrate_relations()?;
        self.migrate_g05()?;
        self.migrate_round5()?;
        self.migrate_account()?;
        self.migrate_governance_columns()?;
        Ok(())
    }

    /// G02-05（#114）注销：**排队标记**。
    ///
    /// `deleted_at` 只回答一件事——"这个账号已经不再属于活人"。所有发凭证的
    /// 入口（登录口令校验、会话签发）都按它 fail-closed，因此**不等 worker 跑完
    /// 清理，账号的数据就已经取不到了**；行本身保留到清理完成，`groups.owner_uid`
    /// 这类没有 CASCADE 的外键才不会因为一次 DELETE 把整条链顶回来。
    ///
    /// 原地补列（与 `migrate_security_columns` 同一手法）：老行 `NULL` = 未注销。
    fn migrate_account(&self) -> Result<()> {
        let c = self.lock();
        if !Self::has_column(&c, "users", "deleted_at")? {
            c.execute_batch("ALTER TABLE users ADD COLUMN deleted_at INTEGER;")?;
        }
        Ok(())
    }

    /// G02-09 治理镜像列（schema/proposals/G02/governance.sql，集成请求
    /// REQ-G00-governance-users-migration）：users 表归平台，治理状态要在
    /// 平台登录/会话路径可读。域自有表（identity_user_governance 等）已随
    /// `wxlike_identity::migrate` 幂等落地；这三个补列是**镜像**，权威在域表。
    fn migrate_governance_columns(&self) -> Result<()> {
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
            ("suspended", "INTEGER NOT NULL DEFAULT 0"),
            ("disabled", "INTEGER NOT NULL DEFAULT 0"),
            ("suspended_until_ms", "INTEGER"),
        ] {
            if !have.iter().any(|name| name == col) {
                c.execute_batch(&format!("ALTER TABLE users ADD COLUMN {col} {ddl};"))?;
            }
        }
        Ok(())
    }

    /// 审计第 5 轮：两个小表——
    /// * `qr_ticket_creator`：扫码登录 HTTP 适配层的归属表（哪张票是谁建的，
    ///   以及服务端持有的 PKCE verifier——见 `qr_login` 模块头）。规则本体在
    ///   `identity_qr`（features crate 建），这里只补 HTTP 面需要的映射；
    /// * `wallet_idem`：钱包写接口的幂等键（`Idempotency-Key` → 24h 内重放
    ///   同一结果），见 `wallet` 模块。
    fn migrate_round5(&self) -> Result<()> {
        let c = self.lock();
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS qr_ticket_creator (
                ticket_id   TEXT PRIMARY KEY,
                creator     TEXT NOT NULL,
                verifier    TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS wallet_idem (
                user_id      TEXT NOT NULL,
                key          TEXT NOT NULL,
                request_hash TEXT NOT NULL,
                response     TEXT NOT NULL,
                created_at   INTEGER NOT NULL,
                PRIMARY KEY (user_id, key)
            );",
        )?;
        Ok(())
    }

    /// G02-02：关系域补充表（群可见性 / 解散、好友标签、关系审计）。
    /// 与身份域同理：DDL 在 features crate 里，这里只执行。
    fn migrate_relations(&self) -> Result<()> {
        let c = self.lock();
        wxlike_relations::migrate(&c)?;
        Ok(())
    }

    /// G02-01：身份域的刷新链 / step-up / 设备表。建表语句在 features crate 里
    /// （`crates/features/identity`），这里只负责在平台库上执行——规则只有一份。
    fn migrate_identity(&self) -> Result<()> {
        let c = self.lock();
        wxlike_identity::migrate(&c)?;
        Ok(())
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
}
