-- G07 discovery 域持久化提案（无编号；由 G00 登记后进入正式迁移并统一编号）。
-- 基准：docs/full-platform-delivery/01_contracts_and_data.md §2 与本组 actions.yaml/events.yaml。
-- 现状：worktree g07 的 crates/features/discovery 为纯域实现（内存状态），
--       本提案是接入 PG 时的目标结构；字段与公开类型一一对应，不引入域外语义。

-- ============ discovery.qr_scan（G07-01）============
-- 扫码审计：只记 actor/kind/摘要/结果，绝不落原始图片、token、完整 URL。
CREATE TABLE discovery_qr_scan (
    scan_id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor          TEXT        NOT NULL,
    kind           TEXT        NOT NULL CHECK (kind IN ('friend','group','payment','miniapp','external','text')),
    outcome        TEXT        NOT NULL CHECK (outcome IN ('decoded','rejected','blocked','expired')),
    payload_digest CHAR(64)    NOT NULL,              -- 原始 wire 的 SHA-256
    reject_reason  TEXT,
    scanned_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX discovery_qr_scan_actor_time_idx ON discovery_qr_scan (actor, scanned_at DESC);
-- 保留政策：审计 90 天后由清理任务删除；导出走 shared 导出框架，不含 payload 原文。

-- ============ discovery.payment_nonce（G07-01）============
-- 一次性 nonce：数据库唯一约束是并发重放的最后防线（API 层先查后写不算数）。
CREATE TABLE discovery_payment_nonce (
    nonce         TEXT PRIMARY KEY,
    expires_at_ms BIGINT    NOT NULL,
    consumed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX discovery_payment_nonce_expiry_idx ON discovery_payment_nonce (expires_at_ms);
-- 清理：expires_at_ms 过期即可物理删除；回滚=整表 drop（无账本事实）。

-- ============ discovery.vision_job（G07-02）============
-- 模型任务：只存结果摘要与 TTL；输入字节从不落库。
CREATE TABLE discovery_vision_job (
    job_id        TEXT PRIMARY KEY,
    actor         TEXT        NOT NULL,
    kind          TEXT        NOT NULL CHECK (kind IN ('identify','ocr','transcribe')),
    provider      TEXT        NOT NULL,
    result        JSONB       NOT NULL,              -- labels/ocr/transcript，与 ports.rs 同构
    byte_len      INTEGER     NOT NULL,              -- 审计用输入字节数
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ NOT NULL
);
CREATE INDEX discovery_vision_job_actor_idx ON discovery_vision_job (actor, expires_at);
CREATE INDEX discovery_vision_job_expiry_idx ON discovery_vision_job (expires_at);
-- 删除/导出/保留：用户可一键删除（delete_actor_data）；结果默认 1 小时 TTL；导出仅 actor 本人发起。

-- ============ discovery.vision_consent（G07-02）============
CREATE TABLE discovery_vision_consent (
    actor         TEXT PRIMARY KEY,
    expires_at_ms BIGINT NOT NULL,
    granted_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at    TIMESTAMPTZ
);
-- 撤回授权=置 revoked_at 并删除行内 expires 授权语义；行保留供审计，不含内容。

-- ============ discovery.geo_presence（G07-03）============
-- 必需字段对齐总册：user, coarse_cell, expiry, visibility；精确坐标不落库。
CREATE TABLE discovery_geo_presence (
    actor         TEXT PRIMARY KEY,
    lat_idx       INTEGER NOT NULL,
    lng_idx       INTEGER NOT NULL,
    visibility    TEXT    NOT NULL CHECK (visibility IN ('friends','everyone')),
    expires_at_ms BIGINT  NOT NULL,
    updated_at_ms BIGINT  NOT NULL
);
CREATE INDEX discovery_geo_presence_cell_idx ON discovery_geo_presence (lat_idx, lng_idx) WHERE expires_at_ms > 0;
CREATE INDEX discovery_geo_presence_expiry_idx ON discovery_geo_presence (expires_at_ms);
-- 撤回授权=DELETE 行（同一事务内删除投影后确认 inbox）；过期行由 TTL 清理任务删除。

-- ============ discovery.geo_block / discovery.geo_report（G07-03）============
CREATE TABLE discovery_geo_block (
    actor       TEXT NOT NULL,
    target      TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (actor, target)
);

CREATE TABLE discovery_geo_report (
    report_id    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    reporter     TEXT NOT NULL,
    target       TEXT NOT NULL,
    reason       TEXT NOT NULL CHECK (char_length(reason) <= 200),
    reported_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX discovery_geo_report_target_idx ON discovery_geo_report (target, reported_at DESC);
-- 举报原因可能含用户自由文本：导出仅本人发起；保留 180 天。

-- ============ discovery.shake_match（G07-03）============
CREATE TABLE discovery_shake_ticket (
    ticket_id     TEXT PRIMARY KEY,
    actor         TEXT        NOT NULL,
    proposal_id   TEXT,
    expires_at_ms BIGINT      NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX discovery_shake_ticket_active_actor_idx
    ON discovery_shake_ticket (actor)
    WHERE proposal_id IS NULL AND expires_at_ms > 0;   -- 单活跃窗口

CREATE TABLE discovery_shake_proposal (
    proposal_id    TEXT PRIMARY KEY,
    ticket_id      TEXT NOT NULL UNIQUE,               -- 一窗一客
    host           TEXT NOT NULL,
    guest          TEXT NOT NULL,
    host_confirmed BOOLEAN NOT NULL DEFAULT FALSE,
    guest_confirmed BOOLEAN NOT NULL DEFAULT FALSE,
    expires_at_ms  BIGINT NOT NULL
);
CREATE UNIQUE INDEX discovery_shake_proposal_guest_idx ON discovery_shake_proposal (guest) WHERE guest_confirmed = FALSE;

CREATE TABLE discovery_match (
    match_id      TEXT PRIMARY KEY,
    user_a        TEXT NOT NULL,
    user_b        TEXT NOT NULL,
    matched_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (user_a < user_b)
);
CREATE UNIQUE INDEX discovery_match_pair_idx ON discovery_match (user_a, user_b);
-- match 落库与 G02 关系边创建同事务（消费 discovery.match.matched 事件）。

-- ============ discovery.share_session / share_position（G07-08）============
CREATE TABLE discovery_share_session (
    share_id           TEXT PRIMARY KEY,
    conversation_id    TEXT NOT NULL,
    host               TEXT NOT NULL,
    members            TEXT[] NOT NULL,                -- G03 会话授权集快照
    expires_at_ms      BIGINT NOT NULL,
    interval_ms        BIGINT NOT NULL,
    precision_mode     TEXT NOT NULL CHECK (precision_mode IN ('exact','approx_100m')),
    background_limited BOOLEAN NOT NULL DEFAULT FALSE,
    state              TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active','stopped')),
    seq                BIGINT NOT NULL DEFAULT 0,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX discovery_share_session_host_active_idx ON discovery_share_session (host) WHERE state = 'active';
CREATE UNIQUE INDEX discovery_share_session_host_one_active_idx ON discovery_share_session (host) WHERE state = 'active';

CREATE TABLE discovery_share_member (
    share_id  TEXT NOT NULL REFERENCES discovery_share_session(share_id) ON DELETE CASCADE,
    user_id   TEXT NOT NULL,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (share_id, user_id)
);

CREATE TABLE discovery_share_position (
    share_id      TEXT NOT NULL,
    user_id       TEXT NOT NULL,
    lat           DOUBLE PRECISION NOT NULL,
    lng           DOUBLE PRECISION NOT NULL,
    coord_system  TEXT NOT NULL CHECK (coord_system IN ('wgs84','gcj02','bd09')),
    accuracy_m    REAL NOT NULL,
    seq           BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (share_id, user_id)
);
-- 删除/导出：停止共享/成员移除/会话过期时删除对应行（同事务）；导出仅本人位置轨迹。
-- 保留：会话结束后位置行立即删除，不保留历史轨迹。

-- ============ discovery.contact_card（G07-09）============
-- 服务端只存 capability 哈希；wire/capability 明文不出现在任何表。
CREATE TABLE discovery_contact_card (
    card_id        TEXT PRIMARY KEY,
    owner          TEXT NOT NULL,
    capability_hash CHAR(64) NOT NULL UNIQUE,          -- SHA-256(capability)
    expires_at_ms  BIGINT NOT NULL,
    revoked_at     TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX discovery_contact_card_owner_idx ON discovery_contact_card (owner, created_at DESC);
-- 轮换/撤销：同事务置 revoked_at 并发 discovery.contact.card_revoked 事件。

-- ============ 回滚与迁移步骤 ============
-- 1) 全部为新表，回滚 = 按依赖逆序 DROP（position→member→session→match→proposal→ticket→report→block→presence→consent→job→nonce→scan→card）。
-- 2) 无存量数据回填；上线后由 TTL 清理任务维护。
-- 3) 不得用删除行的方式“撤销”任何状态；纠错走新事件。
-- 4) 编号与登记归 G00；本文件不进入正式迁移序列。
