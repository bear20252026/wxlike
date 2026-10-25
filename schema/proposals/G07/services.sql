-- G07 services 域持久化提案（无编号；由 G00 登记后进入正式迁移并统一编号）。
-- 基准：docs/full-platform-delivery/01_contracts_and_data.md §2（discovery.offer/quote/order 行）
--       与 contracts/domains/services/actions.yaml。
-- 现状：worktree g07 的 crates/service-catalog 为纯域实现（内存状态），
--       本提案是接入 PG 时的目标结构；订单/资金事实只增不删。

-- ============ services.catalog_card（G07-04）============
CREATE TABLE services_catalog_card (
    service_id           TEXT PRIMARY KEY,
    kind                 TEXT NOT NULL CHECK (kind IN ('mobile_topup','utility_bill','donation','travel','hotel','movie','medical','retail','game')),
    title                TEXT NOT NULL,
    provider             TEXT NOT NULL,
    region               TEXT NOT NULL,
    currency             CHAR(3) NOT NULL,
    display_price_minor  BIGINT NOT NULL CHECK (display_price_minor >= 0),
    terms_version        TEXT NOT NULL,
    enabled              BOOLEAN NOT NULL DEFAULT TRUE,
    revision             BIGINT NOT NULL DEFAULT 0,
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX services_catalog_card_region_kind_idx ON services_catalog_card (region, kind) WHERE enabled;

CREATE TABLE services_region_enabled (
    kind      TEXT NOT NULL,
    region    TEXT NOT NULL,
    enabled   BOOLEAN NOT NULL,
    PRIMARY KEY (kind, region)
);
-- revision 变更即触发“price changed; re-quote”：旧 quote 建单被拒。

-- ============ services.quote（G07-04）============
CREATE TABLE services_quote (
    quote_id      TEXT PRIMARY KEY,
    actor         TEXT NOT NULL,
    service_id    TEXT NOT NULL REFERENCES services_catalog_card(service_id),
    kind          TEXT NOT NULL,
    provider      TEXT NOT NULL,
    region        TEXT NOT NULL,
    params        JSONB NOT NULL,
    quantity      INTEGER NOT NULL CHECK (quantity > 0),
    amount_minor  BIGINT NOT NULL CHECK (amount_minor > 0),
    currency      CHAR(3) NOT NULL,
    terms_version TEXT NOT NULL,
    card_revision BIGINT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL
);
CREATE INDEX services_quote_actor_idx ON services_quote (actor, expires_at_ms DESC);
CREATE INDEX services_quote_expiry_idx ON services_quote (expires_at_ms);
-- 报价过期由 purge job 清理；删除过期报价不属账本事实。

-- ============ services.order（G07-05，账本性质）============
-- 必需字段对齐总册：provider,region,service_type；price,currency,expiry；provider_ref,state。
CREATE TABLE services_order (
    order_id          TEXT PRIMARY KEY,
    actor             TEXT NOT NULL,
    quote_id          TEXT NOT NULL,
    service_id        TEXT NOT NULL,
    kind              TEXT NOT NULL,
    provider          TEXT NOT NULL,
    provider_order_id TEXT NOT NULL,
    amount_minor      BIGINT NOT NULL CHECK (amount_minor > 0),
    currency          CHAR(3) NOT NULL,
    state             TEXT NOT NULL CHECK (state IN ('created','paid','fulfilling','fulfilled','cancelling','cancelled','refunding','refunded','failed','pending_reconciliation')),
    receipt           JSONB,
    payment_id        TEXT,
    refund            JSONB,
    support_ticket    TEXT,
    created_at_ms     BIGINT NOT NULL,
    updated_at_ms     BIGINT NOT NULL
);
CREATE UNIQUE INDEX services_order_provider_ref_idx ON services_order (provider, provider_order_id);
CREATE INDEX services_order_actor_idx ON services_order (actor, created_at_ms DESC);

-- 状态迁移历史：只追加，不改不删；对账依据。
CREATE TABLE services_order_event (
    order_id   TEXT NOT NULL REFERENCES services_order(order_id),
    seq        BIGINT NOT NULL,                          -- 与 Order.events 顺序一致
    state      TEXT NOT NULL,
    at_ms      BIGINT NOT NULL,
    PRIMARY KEY (order_id, seq)
);

-- 幂等表：同键换参数必须 CONFLICT（数据库唯一约束兜底，不只靠 API 先查后写）。
CREATE TABLE services_order_idempotency (
    idem_key      TEXT PRIMARY KEY,                      -- actor:idempotency_key
    params_digest CHAR(64) NOT NULL,
    order_id      TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ============ services.reconciliation（G07-05）============
CREATE TABLE services_reconciliation (
    order_id     TEXT PRIMARY KEY REFERENCES services_order(order_id),
    provider     TEXT NOT NULL,
    reason       TEXT NOT NULL CHECK (reason IN ('fulfill_failed','cancel_failed','cancel_rejected','payment_error','provider_pending','provider_failed')),
    opened_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at  TIMESTAMPTZ,
    resolution   TEXT CHECK (resolution IN ('recovered','refunded','written_off') OR resolution IS NULL)
);
-- 离开 PENDING_RECONCILIATION 只能由对账/管理台走原用例，不允许直连表改 status。

-- ============ services.vendor_audit（G07-04..07 公用）============
-- 供应商交互审计：请求摘要与错误码，不存供应商原始报文与密钥。
CREATE TABLE services_vendor_audit (
    audit_id    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    provider    TEXT NOT NULL,
    actor       TEXT NOT NULL,
    operation   TEXT NOT NULL,                           -- availability/place_order/fulfill/cancel/refund/query/bill_lookup/hold/confirm...
    outcome     TEXT NOT NULL,
    error_code  TEXT,
    at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX services_vendor_audit_provider_time_idx ON services_vendor_audit (provider, at DESC);
-- 保留 180 天；导出仅 admin 发起。

-- ============ services.reservation（G07-06）============
CREATE TABLE services_reservation_offer (
    offer_id      TEXT PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN ('travel','hotel','movie','medical')),
    region        TEXT NOT NULL,
    title         TEXT NOT NULL,
    price_minor   BIGINT NOT NULL CHECK (price_minor > 0),
    currency      CHAR(3) NOT NULL,
    starts_at_ms  BIGINT NOT NULL,
    terms_version TEXT NOT NULL,
    authorized    BOOLEAN NOT NULL DEFAULT FALSE         -- 医疗只接受 TRUE 来源
);

CREATE TABLE services_reservation_hold (
    hold_id       TEXT PRIMARY KEY,
    actor         TEXT NOT NULL,
    offer_id      TEXT NOT NULL REFERENCES services_reservation_offer(offer_id),
    expires_at_ms BIGINT NOT NULL,
    released_at   TIMESTAMPTZ,
    referral      TEXT
);
CREATE UNIQUE INDEX services_reservation_hold_active_actor_idx
    ON services_reservation_hold (actor) WHERE released_at IS NULL AND expires_at_ms > 0;

CREATE TABLE services_reservation_booking (
    booking_id   TEXT PRIMARY KEY,
    hold_id      TEXT NOT NULL,
    actor        TEXT NOT NULL,
    offer_id     TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('held','confirmed','cancelled','refunded','failed','pending_reconciliation')),
    message      TEXT NOT NULL DEFAULT '',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX services_reservation_booking_actor_idx ON services_reservation_booking (actor, created_at DESC);

-- 敏感出行人资料：密文+nonce；明文只在 open 时解密并写审计。
CREATE TABLE services_reservation_profile (
    booking_id  TEXT PRIMARY KEY REFERENCES services_reservation_booking(booking_id) ON DELETE CASCADE,
    actor       TEXT NOT NULL,
    ciphertext  BYTEA NOT NULL,                          -- ChaCha20Poly1305
    nonce       BYTEA NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE services_reservation_profile_audit (
    audit_id    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor       TEXT NOT NULL,
    booking_id  TEXT NOT NULL,
    action      TEXT NOT NULL,                           -- seal/open/delete
    at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX services_reservation_profile_audit_booking_idx ON services_reservation_profile_audit (booking_id, at DESC);

-- Saga 阶段：确认失败补偿可恢复。
CREATE TABLE services_reservation_saga (
    booking_id  TEXT NOT NULL REFERENCES services_reservation_booking(booking_id) ON DELETE CASCADE,
    stage       TEXT NOT NULL,
    done        BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (booking_id, stage)
);

-- ============ services.retail_cart（G07-07）============
CREATE TABLE services_retail_cart (
    cart_id    TEXT PRIMARY KEY,
    actor      TEXT NOT NULL,
    region     TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX services_retail_cart_actor_idx ON services_retail_cart (actor);   -- 单人单购物车

CREATE TABLE services_retail_cart_item (
    cart_id   TEXT NOT NULL REFERENCES services_retail_cart(cart_id) ON DELETE CASCADE,
    sku       TEXT NOT NULL,
    quantity  INTEGER NOT NULL CHECK (quantity > 0),
    PRIMARY KEY (cart_id, sku)
);
-- 20 SKU 上限由应用层校验；并发下以 COUNT 校验+唯一键兜底。

-- ============ services.game（G07-07）============
CREATE TABLE services_game_package (
    package_id        TEXT PRIMARY KEY,
    title             TEXT NOT NULL,
    version           TEXT NOT NULL,
    price_minor       BIGINT NOT NULL CHECK (price_minor > 0),
    currency          CHAR(3) NOT NULL,
    regions           TEXT[] NOT NULL,
    program_sha256    CHAR(64) NOT NULL,
    signature         TEXT NOT NULL,
    publisher_key_id  TEXT NOT NULL,
    published_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- 发布版本不可原地替换：改包=新 package_id/version 行。

CREATE TABLE services_game_entitlement (
    actor         TEXT NOT NULL,
    package_id    TEXT NOT NULL REFERENCES services_game_package(package_id),
    active        BOOLEAN NOT NULL DEFAULT TRUE,
    price_minor   BIGINT NOT NULL,
    order_id      TEXT NOT NULL,
    granted_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at    TIMESTAMPTZ,
    PRIMARY KEY (actor, package_id)
);

CREATE TABLE services_game_save (
    actor       TEXT NOT NULL,
    package_id  TEXT NOT NULL,
    slot        SMALLINT NOT NULL CHECK (slot BETWEEN 0 AND 7),
    data        BYTEA NOT NULL CHECK (octet_length(data) <= 1000000),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (actor, package_id, slot)
);

CREATE TABLE services_game_launch_ticket (
    ticket_id     TEXT PRIMARY KEY,
    actor         TEXT NOT NULL,
    package_id    TEXT NOT NULL,
    issued_at_ms  BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL
);
CREATE INDEX services_game_launch_ticket_expiry_idx ON services_game_launch_ticket (expires_at_ms);
-- 过期票据由清理任务删除，无账本事实。

-- ============ 回滚与迁移步骤 ============
-- 1) 全部为新表；回滚顺序：game ticket/save/entitlement/package → cart item/cart → saga/profile audit/profile/booking/hold/offer →
--    vendor_audit → reconciliation → idempotency → order event/order → quote → region_enabled → catalog card。
--    注意：services_order 是资金相关事实，回滚只删未支付（state=created 且无 payment_id）的孤儿行，
--    已支付/已退款行必须保留并转人工对账，不得删行“撤销”。
-- 2) 无存量回填；首次上线为空表。
-- 3) provider+provider_order_id 唯一约束防止重复供应商订单；idempotency 唯一键防重复扣款。
-- 4) 编号与登记归 G00；本文件不进入正式迁移序列。
