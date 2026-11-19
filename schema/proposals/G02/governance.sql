-- G02-09 账户治理迁移草稿
--
-- 执行者：G00（迁移编号与串行合入归 G00，本文件只是草稿）。
-- 幂等：全部 IF NOT EXISTS；对既有 users / group_meta 表只做"补列"，不改既有列语义。
-- 回滚：见同目录 README.md「回滚兼容说明」与 integration-requests/G02/G02-09-governance.yaml。
--
-- 域内执行点（已随 crate migrate() 幂等落地，G00 可直接复用同逻辑）：
--   crates/features/identity/src/governance/mod.rs::migrate   （identity_* 三表）
--   crates/features/relations/src/governance.rs::migrate      （group_governance_audit + group_meta.frozen_at）

-- ---------------------------------------------------------------------------
-- G02-09 用户治理状态（identity 域自有）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS identity_user_governance (
    user_id            TEXT PRIMARY KEY,
    suspended          INTEGER NOT NULL DEFAULT 0,
    disabled           INTEGER NOT NULL DEFAULT 0,
    suspended_until_ms INTEGER,
    reason             TEXT NOT NULL DEFAULT '',
    actor              TEXT NOT NULL DEFAULT '',
    updated_at         INTEGER NOT NULL
);

-- G02-09 治理回执/审计（append-only；不写 admin 的审计表）
CREATE TABLE IF NOT EXISTS identity_governance_audit (
    seq              INTEGER PRIMARY KEY AUTOINCREMENT,
    action           TEXT NOT NULL,
    target           TEXT NOT NULL,
    actor            TEXT NOT NULL,
    reason           TEXT NOT NULL,
    trace            TEXT NOT NULL DEFAULT '',
    before_json      TEXT NOT NULL DEFAULT '{}',
    after_json       TEXT NOT NULL DEFAULT '{}',
    sessions_revoked INTEGER NOT NULL DEFAULT 0,
    already          INTEGER NOT NULL DEFAULT 0,
    at               INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_identity_gov_audit_target
    ON identity_governance_audit(target, seq);

-- G02-09 管理标签（AdminUserView.labels）
CREATE TABLE IF NOT EXISTS identity_governance_label (
    user_id    TEXT NOT NULL,
    label      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, label)
);

-- ---------------------------------------------------------------------------
-- G02-09 群治理审计（relations 域自有）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS group_governance_audit (
    seq      INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id TEXT NOT NULL,
    action   TEXT NOT NULL,
    actor    TEXT NOT NULL,
    reason   TEXT NOT NULL,
    trace    TEXT NOT NULL DEFAULT '',
    at       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_group_gov_audit_group
    ON group_governance_audit(group_id, seq);

-- ---------------------------------------------------------------------------
-- 既有表的补列（G00 执行；老库先查 PRAGMA table_info 再 ALTER）
-- ---------------------------------------------------------------------------
-- 平台 users 表治理镜像列（域侧经 PlatformGovernancePort::mirror_user_state 写入；
-- 列由 G00 补，identity crate 不直接 ALTER users）。与 sessions.audience /
-- users.deleted_at 同一手法：有列就跳过，不占独立编号亦可（由 G00 定）。
-- ALTER TABLE users ADD COLUMN suspended          INTEGER NOT NULL DEFAULT 0;
-- ALTER TABLE users ADD COLUMN disabled           INTEGER NOT NULL DEFAULT 0;
-- ALTER TABLE users ADD COLUMN suspended_until_ms INTEGER;
--
-- group_meta 冻结列（relations crate migrate() 已幂等补；新库建表自带）。
-- ALTER TABLE group_meta ADD COLUMN frozen_at INTEGER;
