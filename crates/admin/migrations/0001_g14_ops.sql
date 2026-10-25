-- G14 管理后台运营域 schema（migration 0001）。
--
-- 归属：crates/admin（adminx 自己的 SQLite，`WXLIKE_ADMIN_DB`，默认 adminx.db）。
-- **不碰平台库**：平台库只以只读连接打开，理由见 crates/admin/src/lib.rs。
--
-- 回滚：见 0001_g14_ops.down.sql。本文件只建表/索引/触发器，不改已有表，
-- 因此与 D1/D2/D3 的 admin_accounts / admin_sessions / admin_oplog 完全正交，
-- 旧版本进程（不认识 g14_* 表）可以继续跑在同一份库上。
--
-- 两条不变式写进数据库而不是只写在代码里：
--   1. 审计表 append-only（触发器拒 UPDATE/DELETE）——追责链不能有"事后编辑"。
--   2. 余额表不存在——人工补账只能写 g14_journal 分录，没有任何"改余额"的入口。

CREATE TABLE IF NOT EXISTS g14_audit (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    at_ms         INTEGER NOT NULL,
    actor         TEXT    NOT NULL,
    role          TEXT    NOT NULL,
    action        TEXT    NOT NULL,
    object_kind   TEXT    NOT NULL,
    object_id     TEXT    NOT NULL DEFAULT '',
    reason        TEXT    NOT NULL DEFAULT '',
    before_digest TEXT    NOT NULL DEFAULT '',
    after_digest  TEXT    NOT NULL DEFAULT '',
    trace         TEXT    NOT NULL,
    outcome       TEXT    NOT NULL,
    detail        TEXT    NOT NULL DEFAULT '{}',
    prev_hash     TEXT    NOT NULL,
    hash          TEXT    NOT NULL UNIQUE
);
CREATE INDEX IF NOT EXISTS idx_g14_audit_actor  ON g14_audit(actor, at_ms);
CREATE INDEX IF NOT EXISTS idx_g14_audit_object ON g14_audit(object_kind, object_id, at_ms);
CREATE INDEX IF NOT EXISTS idx_g14_audit_action ON g14_audit(action, at_ms);

CREATE TRIGGER IF NOT EXISTS g14_audit_no_update
BEFORE UPDATE ON g14_audit
BEGIN
    SELECT RAISE(ABORT, 'g14_audit is append-only');
END;

CREATE TRIGGER IF NOT EXISTS g14_audit_no_delete
BEFORE DELETE ON g14_audit
BEGIN
    SELECT RAISE(ABORT, 'g14_audit is append-only');
END;

-- 敏感操作步进验证（step-up）：一次 6 位动态码只换一段很短的"可做危险操作"窗口。
CREATE TABLE IF NOT EXISTS g14_stepup (
    account_id     TEXT PRIMARY KEY,
    verified_at_ms INTEGER NOT NULL,
    method         TEXT NOT NULL DEFAULT 'totp'
);

-- 举报 / 申诉工单。状态机在 identity::CaseState，库里只落结果。
CREATE TABLE IF NOT EXISTS g14_case (
    id              TEXT PRIMARY KEY,
    kind            TEXT NOT NULL,
    subject_kind    TEXT NOT NULL,
    subject_id      TEXT NOT NULL,
    state           TEXT NOT NULL,
    reporter        TEXT NOT NULL DEFAULT '',
    assignee        TEXT NOT NULL DEFAULT '',
    reason          TEXT NOT NULL DEFAULT '',
    decision_reason TEXT NOT NULL DEFAULT '',
    appeal_of       TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_g14_case_state ON g14_case(state, updated_at);

-- 内容 / 小程序 / 服务发布审核队列。idem 唯一 = 重复提交不产生第二个版本。
CREATE TABLE IF NOT EXISTS g14_review_item (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    version      TEXT NOT NULL,
    rule_version TEXT NOT NULL,
    state        TEXT NOT NULL,
    submitted_by TEXT NOT NULL DEFAULT '',
    decided_by   TEXT,
    reason       TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    decided_at   INTEGER,
    idem         TEXT NOT NULL UNIQUE
);
CREATE INDEX IF NOT EXISTS idx_g14_review_state ON g14_review_item(kind, state, created_at);

-- 服务卡片配置：只存 vault 引用（key_ref），**没有**导入密钥的列。
CREATE TABLE IF NOT EXISTS g14_service_config (
    service_id TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    category   TEXT NOT NULL,
    regions    TEXT NOT NULL DEFAULT '[]',
    enabled    INTEGER NOT NULL DEFAULT 0,
    key_ref    TEXT NOT NULL DEFAULT '',
    version    INTEGER NOT NULL DEFAULT 1,
    updated_at INTEGER NOT NULL,
    updated_by TEXT NOT NULL DEFAULT ''
);

-- 小程序发行：签名与发布是两个不同的动作、两个不同的权限。
CREATE TABLE IF NOT EXISTS g14_mini_release (
    id           TEXT PRIMARY KEY,
    app_id       TEXT NOT NULL,
    version      TEXT NOT NULL,
    state        TEXT NOT NULL,
    signed_by    TEXT,
    published_by TEXT,
    signature    TEXT,
    idem         TEXT NOT NULL UNIQUE,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE(app_id, version)
);

CREATE TABLE IF NOT EXISTS g14_merchant (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    state         TEXT NOT NULL,
    payout_masked TEXT NOT NULL DEFAULT '',
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS g14_card (
    id               TEXT PRIMARY KEY,
    merchant_id      TEXT NOT NULL REFERENCES g14_merchant(id),
    title            TEXT NOT NULL,
    face_value_cents INTEGER NOT NULL,
    state            TEXT NOT NULL,
    issued_at        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_g14_card_merchant ON g14_card(merchant_id, issued_at);

CREATE TABLE IF NOT EXISTS g14_card_redeem (
    id           TEXT PRIMARY KEY,
    card_id      TEXT NOT NULL REFERENCES g14_card(id),
    order_id     TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    at_ms        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_g14_redeem_card ON g14_card_redeem(card_id, at_ms);

-- 运营投影：成交与余额的真值在平台账本（G09），这里只放对账需要的订单号映射。
CREATE TABLE IF NOT EXISTS g14_order (
    id           TEXT PRIMARY KEY,
    merchant_id  TEXT NOT NULL REFERENCES g14_merchant(id),
    internal_no  TEXT NOT NULL UNIQUE,
    external_no  TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    currency     TEXT NOT NULL DEFAULT 'CNY',
    state        TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_g14_order_merchant ON g14_order(merchant_id, created_at);

CREATE TABLE IF NOT EXISTS g14_refund (
    id             TEXT PRIMARY KEY,
    order_id       TEXT NOT NULL REFERENCES g14_order(id),
    amount_cents   INTEGER NOT NULL,
    state          TEXT NOT NULL,
    reason         TEXT NOT NULL DEFAULT '',
    requested_by   TEXT NOT NULL,
    approved_by    TEXT,
    second_approver TEXT,
    idem           TEXT NOT NULL UNIQUE,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);

-- 人工补账分录。只有借贷两列，没有任何"余额"列可改。
CREATE TABLE IF NOT EXISTS g14_journal (
    id           TEXT PRIMARY KEY,
    entry_no     TEXT NOT NULL,
    line_no      INTEGER NOT NULL,
    account      TEXT NOT NULL,
    direction    TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    memo         TEXT NOT NULL,
    source       TEXT NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    -- 一张凭证多行分录：唯一性在 (entry_no, line_no)，不是 entry_no 本身
    UNIQUE(entry_no, line_no)
);
CREATE TRIGGER IF NOT EXISTS g14_journal_no_update
BEFORE UPDATE ON g14_journal
BEGIN
    SELECT RAISE(ABORT, 'g14_journal is append-only');
END;

CREATE TABLE IF NOT EXISTS g14_recon_diff (
    id                 TEXT PRIMARY KEY,
    batch              TEXT NOT NULL,
    external_no        TEXT NOT NULL,
    internal_no        TEXT NOT NULL DEFAULT '',
    kind               TEXT NOT NULL,
    amount_delta_cents INTEGER NOT NULL DEFAULT 0,
    state              TEXT NOT NULL,
    note               TEXT NOT NULL DEFAULT '',
    resolved_by        TEXT,
    resolved_at        INTEGER
);
CREATE INDEX IF NOT EXISTS idx_g14_recon_batch ON g14_recon_diff(batch, state);

-- 作业：转码/推送/同步/导入/隐私导出/批量处罚。retry_of 指向被重试的原作业。
CREATE TABLE IF NOT EXISTS g14_job (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    ref_id      TEXT NOT NULL DEFAULT '',
    state       TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    retry_of    TEXT,
    payload     TEXT NOT NULL DEFAULT '{}',
    last_error  TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_g14_job_state ON g14_job(kind, state, created_at);
CREATE INDEX IF NOT EXISTS idx_g14_job_retry ON g14_job(retry_of);

-- 隐私导出/删除申请。token_hash 只存哈希，明文只回给申请者一次。
CREATE TABLE IF NOT EXISTS g14_privacy_request (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    kind         TEXT NOT NULL,
    state        TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    requested_by TEXT NOT NULL,
    requested_at INTEGER NOT NULL,
    fulfilled_at INTEGER,
    expires_at   INTEGER NOT NULL,
    note         TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_g14_privacy_user ON g14_privacy_request(user_id, requested_at);

PRAGMA user_version = 1;
