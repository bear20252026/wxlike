/**
 * Commerce client (G11-04): commerce.* / confirmation protocol.
 *
 * Real today (`crates/platform/src/wallet.rs`): balance, ledger, transfer,
 * redpacket create/claim — every write carries an Idempotency-Key so a
 * double click or a network retry lands on ONE business operation.
 * Contract-gated (G06 CommercePort open): receiving intents, intent payment,
 * refund, withdraw/top-up, cards, entitlements — the page shows the explicit
 * upstream state for those instead of faking success.
 */

import { ApiError, idemKey, request } from "../../lib/http";
import type {
  Account,
  CardRecord,
  EntitlementRecord,
  LedgerPage,
  LedgerRow,
  PaymentIntent,
  PaymentRecord,
  RedpacketCreateInput,
  RedpacketRecord,
  RequestOpts,
  TransferInput,
  TransferReply,
  WalletPorts,
} from "./model";

export class WalletApi implements WalletPorts {
  async accounts(_opts?: RequestOpts): Promise<Account[]> {
    // The server has exactly one virtual wallet account today.
    const { balance } = await request<{ balance: number }>("GET", "/api/wallet/balance");
    return [
      {
        account_id: "wallet",
        kind: "virtual",
        label: "零钱",
        currency: "CNY",
        balance_minor: balance,
      },
    ];
  }

  async balance(opts?: RequestOpts): Promise<{ balance: number }> {
    return request<{ balance: number }>("GET", "/api/wallet/balance", { signal: opts?.signal });
  }

  async ledger(since?: number, limit?: number, opts?: RequestOpts): Promise<LedgerPage> {
    const usp = new URLSearchParams();
    if (since !== undefined) usp.set("since", String(since));
    if (limit !== undefined) usp.set("limit", String(limit));
    const text = usp.toString();
    // Server shape (crates/platform wallet.rs ledger): {entries, cursor, has_more}.
    // `cursor` is the ledger head after this page; a bare array is tolerated for
    // older transports but is NOT the contract the server ships.
    const body = await request<{ entries?: LedgerRow[]; cursor?: number | null; has_more?: boolean } | LedgerRow[]>(
      "GET",
      `/api/wallet/ledger${text ? `?${text}` : ""}`,
      { signal: opts?.signal },
    );
    const items = Array.isArray(body) ? body : (body.entries ?? []);
    const next = Array.isArray(body) ? null : (body.cursor ?? null);
    return { items, next_cursor: next };
  }

  async transfer(input: TransferInput, opts?: RequestOpts): Promise<TransferReply> {
    return request<TransferReply>("POST", "/api/wallet/transfer", {
      body: { to_uid: input.to_uid, amount: input.amount_minor, memo: input.memo ?? null },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("wallet-transfer"),
      signal: opts?.signal,
    });
  }

  async createRedpacket(input: RedpacketCreateInput, opts?: RequestOpts): Promise<RedpacketRecord> {
    const reply = await request<{ redpacket_id: string }>("POST", "/api/wallet/redpacket", {
      body: {
        amount: input.amount_minor,
        count: input.count,
        memo: input.memo ?? null,
        conv_id: input.conv_id ?? null,
      },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("redpacket-create"),
      signal: opts?.signal,
    });
    return {
      redpacket_id: reply.redpacket_id,
      total_minor: input.amount_minor,
      count: input.count,
      memo: input.memo,
      expires_at: Date.now() + 24 * 3600 * 1000,
    };
  }

  async redpacket(id: string, opts?: RequestOpts): Promise<RedpacketRecord> {
    return request<RedpacketRecord>("GET", `/api/wallet/redpacket/${encodeURIComponent(id)}`, {
      signal: opts?.signal,
    });
  }

  async claimRedpacket(id: string, opts?: RequestOpts): Promise<{ amount: number }> {
    return request<{ amount: number }>(
      "POST",
      `/api/wallet/redpacket/${encodeURIComponent(id)}/claim`,
      {
        idempotencyKey: opts?.idempotencyKey ?? idemKey("redpacket-claim"),
        signal: opts?.signal,
      },
    );
  }

  // --- G06 CommercePort contract surface (upstream pending) -----------------

  async createReceivingIntent(
    _amount_minor: number,
    _memo?: string,
    _opts?: RequestOpts,
  ): Promise<PaymentIntent> {
    return request<PaymentIntent>("POST", "/api/payment-intents", {
      body: { amount: _amount_minor, memo: _memo ?? null, direction: "receiving" },
      idempotencyKey: idemKey("intent-receiving"),
    });
  }

  async payIntent(qrPayload: string, opts?: RequestOpts): Promise<PaymentRecord> {
    return request<PaymentRecord>("POST", "/api/payment-intents/pay", {
      body: { qr_payload: qrPayload },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("intent-pay"),
      signal: opts?.signal,
    });
  }

  async paymentState(paymentId: string, opts?: RequestOpts): Promise<PaymentRecord> {
    return request<PaymentRecord>("GET", `/api/payments/${encodeURIComponent(paymentId)}`, {
      signal: opts?.signal,
    });
  }

  async refund(paymentId: string, opts?: RequestOpts): Promise<PaymentRecord> {
    return request<PaymentRecord>("POST", `/api/payments/${encodeURIComponent(paymentId)}/refund`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("payment-refund"),
      signal: opts?.signal,
    });
  }

  async withdraw(accountId: string, amount_minor: number, opts?: RequestOpts): Promise<PaymentRecord> {
    return request<PaymentRecord>("POST", "/api/withdrawals", {
      body: { account_id: accountId, amount: amount_minor },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("withdraw"),
      signal: opts?.signal,
    });
  }

  async topUp(accountId: string, amount_minor: number, opts?: RequestOpts): Promise<PaymentRecord> {
    return request<PaymentRecord>("POST", "/api/topups", {
      body: { account_id: accountId, amount: amount_minor },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("topup"),
      signal: opts?.signal,
    });
  }

  async cards(_opts?: RequestOpts): Promise<CardRecord[]> {
    return request<CardRecord[]>("GET", "/api/cards");
  }

  async entitlements(_opts?: RequestOpts): Promise<EntitlementRecord[]> {
    return request<EntitlementRecord[]>("GET", "/api/entitlements");
  }
}

export function isUpstreamMissing(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 404 || err.status === 501);
}
