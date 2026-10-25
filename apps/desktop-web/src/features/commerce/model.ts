/**
 * Types for the commerce feature (G11-04). Mirrors crates/platform/wallet.rs
 * plus the CommercePort contract (§3 of the data contract) for the parts the
 * server does not have yet.
 *
 * All amounts are **integer minor units** (分). No float ever touches a
 * money value in this feature.
 */

export type AccountKind = "virtual" | "real";

/** 严格区分虚拟账户（站内零钱）与真实账户（银行/供应商通道）。 */
export interface Account {
  account_id: string;
  kind: AccountKind;
  label: string;
  currency: string;
  /** Minor units; integers only. */
  balance_minor: number;
}

export interface LedgerRow {
  id: number;
  kind: string;
  amount: number;
  counterparty: string;
  ref_id: string;
  memo: string;
  created_at: number;
}

export interface LedgerPage {
  items: LedgerRow[];
  next_cursor: number | null;
}

export interface TransferInput {
  to_uid: string;
  amount_minor: number;
  memo?: string;
}

export interface TransferReply {
  transfer_id: string;
}

export interface RedpacketCreateInput {
  amount_minor: number;
  count: number;
  memo?: string;
  conv_id?: string;
}

export interface RedpacketRecord {
  redpacket_id: string;
  total_minor: number;
  count: number;
  remaining?: number;
  memo?: string;
  expires_at: number;
  claimed_by_me?: boolean;
}

/**
 * G06 CommercePort contract: a short-lived payment intent backs every QR.
 * 截图二维码过期失效 — the QR carries intent_id + expiry and is worthless
 * after `expires_at`. `66.66` 样例或随机串永远不是合法收款码。
 */
export interface PaymentIntent {
  intent_id: string;
  payee: string;
  payee_name?: string;
  amount_minor: number;
  currency: string;
  memo?: string;
  expires_at: number;
  /** Encoded payload scanned by the payer's client. */
  qr_payload: string;
}

export type PaymentState =
  | "pending"
  | "querying"
  | "succeeded"
  | "failed"
  | "refunded"
  | "refund_pending";

export interface PaymentRecord {
  payment_id: string;
  intent_id?: string;
  amount_minor: number;
  currency: string;
  /** 未知供应商状态必须是 "querying"（查询中），绝不能显示成功。 */
  state: PaymentState;
  account_kind: AccountKind;
  created_at: number;
}

export interface CardRecord {
  card_id: string;
  kind: AccountKind;
  label: string;
  masked_number: string;
  verified: boolean;
}

export interface EntitlementRecord {
  entitlement_id: string;
  title: string;
  issuer: string;
  expires_at: number;
  status: "valid" | "used" | "expired";
  /** 票面/卡券图片或核销码；核销 nonce 一次性。 */
  redemption_code?: string;
}

export interface RequestOpts {
  signal?: AbortSignal;
  idempotencyKey?: string;
}

export interface WalletPorts {
  accounts(opts?: RequestOpts): Promise<Account[]>;
  balance(opts?: RequestOpts): Promise<{ balance: number }>;
  ledger(since?: number, limit?: number, opts?: RequestOpts): Promise<LedgerPage>;
  transfer(input: TransferInput, opts?: RequestOpts): Promise<TransferReply>;
  createRedpacket(input: RedpacketCreateInput, opts?: RequestOpts): Promise<RedpacketRecord>;
  redpacket(id: string, opts?: RequestOpts): Promise<RedpacketRecord>;
  claimRedpacket(id: string, opts?: RequestOpts): Promise<{ amount: number }>;
  /** G06 contract: short-term receiving intent → QR. */
  createReceivingIntent(amount_minor: number, memo?: string, opts?: RequestOpts): Promise<PaymentIntent>;
  payIntent(qrPayload: string, opts?: RequestOpts): Promise<PaymentRecord>;
  paymentState(paymentId: string, opts?: RequestOpts): Promise<PaymentRecord>;
  refund(paymentId: string, opts?: RequestOpts): Promise<PaymentRecord>;
  withdraw(accountId: string, amount_minor: number, opts?: RequestOpts): Promise<PaymentRecord>;
  topUp(accountId: string, amount_minor: number, opts?: RequestOpts): Promise<PaymentRecord>;
  cards(opts?: RequestOpts): Promise<CardRecord[]>;
  entitlements(opts?: RequestOpts): Promise<EntitlementRecord[]>;
}
