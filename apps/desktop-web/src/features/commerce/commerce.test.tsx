/**
 * G11-04 test matrix — money is integer-only (no float math), concurrent
 * clicks are one business operation, QR intents expire, unknown supplier
 * state renders 查询中, virtual/real accounts distinct, refund/withdraw
 * failure recovery, red packet claim idempotent, error cases mapped.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { setTransport } from "../../lib/http";
import { CallLog, err, fixtureTransport, loggingTransport } from "../../test/fixture";
import { formatMoney, parseToMinor, isJsonSafe } from "../../lib/money";
import { QrIntentPanel } from "./QrIntent";
import { WalletPage } from "./WalletPage";
import type { WalletPorts } from "./model";

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  cleanup();
  setTransport(null);
});

describe("unit — money (无浮点财务运算)", () => {
  it("格式化来自整数最小单位", () => {
    expect(formatMoney(1999)).toBe("¥19.99");
    expect(formatMoney(5)).toBe("¥0.05");
    expect(formatMoney(100000)).toBe("¥1000.00");
    expect(formatMoney(-250)).toBe("¥-2.50");
    expect(formatMoney("12345")).toBe("¥123.45");
  });

  it("解析拒绝浮点精度陷阱与越界小数", () => {
    expect(parseToMinor("19.99")).toBe(1999n);
    // 0.1+0.2 浮点陷阱在整数域不存在：0.1 元 = 10 分
    expect(parseToMinor("0.1")).toBe(10n);
    expect(parseToMinor("0.2")).toBe(20n);
    expect(parseToMinor("0.3")).toBe(30n);
    expect(() => parseToMinor("0.005")).toThrow("最多 2 位小数");
    expect(() => parseToMinor("1.2.3")).toThrow("格式不正确");
    expect(() => parseToMinor("-1")).toThrow("格式不正确"); // 负数直接被格式校验拦截
    expect(() => parseToMinor("0")).toThrow("必须大于零");
    expect(() => parseToMinor("abc")).toThrow("格式不正确");
  });

  it("JSON 安全范围", () => {
    expect(isJsonSafe(1999n)).toBe(true);
    expect(isJsonSafe(2n ** 53n)).toBe(true);
    expect(isJsonSafe(2n ** 53n + 1n)).toBe(false);
  });
});

const balanceRoute = { "GET /api/wallet/balance": () => ({ balance: 12345 }) };

describe("WalletPage — 零钱/账单", () => {
  it("虚拟账户与真实账户区分展示", async () => {
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
      }),
    );
    render(<WalletPage />);
    await waitFor(() => expect(screen.getByTestId("account-wallet")).toBeInTheDocument());
    expect(screen.getByTestId("account-wallet")).toHaveTextContent("虚拟账户");
    expect(screen.getByTestId("balance-wallet")).toHaveTextContent("¥123.45");
  });

  it("账单列表渲染金额与方向", async () => {
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({
          entries: [
            { id: 1, kind: "transfer", amount: 500, counterparty: "user-b", ref_id: "t1", memo: "午饭", created_at: 1_700_000_000_000 },
            { id: 2, kind: "receive", amount: -200, counterparty: "shop", ref_id: "t2", memo: "奶茶", created_at: 1_700_000_001_000 },
          ],
          // 真实服务端形状（crates/platform wallet.rs）：entries + cursor + has_more
          cursor: 2,
          has_more: false,
        }),
      }),
    );
    render(<WalletPage />);
    await waitFor(() => expect(screen.getByTestId("ledger-1")).toBeInTheDocument());
    expect(screen.getByTestId("ledger-1")).toHaveTextContent("+¥5.00");
    expect(screen.getByTestId("ledger-2")).toHaveTextContent("¥-2.00");
  });

  it("断网：错误横幅", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/wallet/balance": () => err(0, "fetch failed", "network"),
        "GET /api/wallet/ledger": () => err(0, "fetch failed", "network"),
      }),
    );
    render(<WalletPage />);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("网络不可用"));
  });

  it("未授权：提示重新登录", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/wallet/balance": () => err(401, "expired"),
        "GET /api/wallet/ledger": () => err(401, "expired"),
      }),
    );
    render(<WalletPage />);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("登录已过期"));
  });
});

describe("WalletPage — 转账（并发仅一笔）", () => {
  it("并发点击只提交一次，走确认协议展示收款人与金额", async () => {
    const log = new CallLog();
    setTransport(
      loggingTransport(
        {
          ...balanceRoute,
          "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
          "POST /api/wallet/transfer": (_m, _p, body) => {
            expect((body as Record<string, unknown>).amount).toBe(1999);
            return { transfer_id: "tr1" };
          },
        },
        log,
      ),
    );
    render(<WalletPage />);
    fireEvent.click(await screen.findByTestId("wallet-tab-transfer"));
    fireEvent.change(screen.getByTestId("transfer-to"), { target: { value: "user-b" } });
    fireEvent.change(screen.getByTestId("transfer-amount"), { target: { value: "19.99" } });
    fireEvent.click(screen.getByTitle("转账"));
    await waitFor(() => expect(screen.getByTestId("transfer-confirm")).toBeInTheDocument());
    expect(screen.getByTestId("confirm-payee")).toHaveTextContent("user-b");
    expect(screen.getByTestId("confirm-amount")).toHaveTextContent("¥19.99");
    const confirm = screen.getByRole("button", { name: "确认转账" });
    fireEvent.click(confirm);
    fireEvent.click(confirm);
    await waitFor(() => expect(screen.getByText("转账已提交")).toBeInTheDocument());
    expect(log.count({ path: "/api/wallet/transfer" })).toBe(1);
    const rows = log.all({ path: "/api/wallet/transfer" });
    expect(rows[0]?.idempotencyKey).toMatch(/^wallet-transfer-/);
  });

  it("余额不足（4xx）显示服务端原因", async () => {
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
        "POST /api/wallet/transfer": () => err(409, "insufficient balance"),
      }),
    );
    render(<WalletPage />);
    fireEvent.click(await screen.findByTestId("wallet-tab-transfer"));
    fireEvent.change(screen.getByTestId("transfer-to"), { target: { value: "user-b" } });
    fireEvent.change(screen.getByTestId("transfer-amount"), { target: { value: "999" } });
    fireEvent.click(screen.getByTitle("转账"));
    fireEvent.click(await screen.findByRole("button", { name: "确认转账" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("insufficient balance"));
  });

  it("参数错误：非法金额不过确认", async () => {
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
      }),
    );
    render(<WalletPage />);
    fireEvent.click(await screen.findByTestId("wallet-tab-transfer"));
    fireEvent.change(screen.getByTestId("transfer-to"), { target: { value: "user-b" } });
    fireEvent.change(screen.getByTestId("transfer-amount"), { target: { value: "0.005" } });
    fireEvent.click(screen.getByTitle("转账"));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("最多 2 位小数"));
    expect(screen.queryByTestId("transfer-confirm")).not.toBeInTheDocument();
  });
});

describe("QrIntentPanel — 截图二维码过期失效", () => {
  const intent = {
    intent_id: "pi-1",
    payee: "user-a",
    payee_name: "小店",
    amount_minor: 1000,
    currency: "CNY",
    expires_at: 0,
    qr_payload: "wxpay:intent:pi-1",
  };

  it("过期后展示失效状态，不再是可扫二维码", async () => {
    render(<QrIntentPanel intent={{ ...intent, expires_at: Date.now() - 1000 }} onRefresh={() => undefined} />);
    await waitFor(() => expect(screen.getByTestId("qr-expired")).toBeInTheDocument());
    expect(screen.getByTestId("qr-expired")).toHaveTextContent("二维码已过期");
    expect(screen.queryByTestId("qr-svg")).not.toBeInTheDocument();
  });

  it("有效期内渲染真实意图 payload 的 SVG", async () => {
    render(<QrIntentPanel intent={{ ...intent, expires_at: Date.now() + 60_000 }} />);
    await waitFor(() => expect(screen.getByTestId("qr-svg")).toBeInTheDocument());
    expect(screen.getByTestId("qr-amount")).toHaveTextContent("¥10.00");
    expect(screen.getByTestId("qr-payee")).toHaveTextContent("小店");
  });
});

describe("WalletPage — 收付款（G06 契约）", () => {
  it("上游未开通：明确提示，不出现假二维码", async () => {
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
        "POST /api/payment-intents": () => err(404, "not found"),
      }),
    );
    render(<WalletPage />);
    fireEvent.click(await screen.findByTestId("wallet-tab-receive"));
    fireEvent.change(screen.getByTestId("receive-amount"), { target: { value: "10" } });
    fireEvent.click(screen.getByRole("button", { name: "生成收款码" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("G06 CommercePort"));
    expect(screen.queryByTestId("qr-intent")).not.toBeInTheDocument();
  });

  it("未知支付状态显示查询中，可再次查询与退款", async () => {
    const ports: Partial<WalletPorts> = {
      createReceivingIntent: async () => ({
        intent_id: "pi-9",
        payee: "me",
        amount_minor: 500,
        currency: "CNY",
        expires_at: Date.now() + 60_000,
        qr_payload: "wxpay:intent:pi-9",
      }),
      payIntent: async () => ({
        payment_id: "pay-1",
        amount_minor: 500,
        currency: "CNY",
        state: "querying",
        account_kind: "real",
        created_at: Date.now(),
      }),
      paymentState: async () => ({
        payment_id: "pay-1",
        amount_minor: 500,
        currency: "CNY",
        state: "succeeded",
        account_kind: "real",
        created_at: Date.now(),
      }),
      refund: async () => ({
        payment_id: "pay-1",
        amount_minor: 500,
        currency: "CNY",
        state: "refunded",
        account_kind: "real",
        created_at: Date.now(),
      }),
    };
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
      }),
    );
    render(<WalletPage ports={ports as WalletPorts} />);
    fireEvent.click(await screen.findByTestId("wallet-tab-receive"));
    fireEvent.change(screen.getByTestId("pay-payload"), { target: { value: "wxpay:intent:pi-9" } });
    fireEvent.click(screen.getByRole("button", { name: "确认支付" }));
    await waitFor(() => expect(screen.getByTestId("payment-state")).toHaveTextContent("查询中…"));
    fireEvent.click(screen.getByRole("button", { name: "再次查询" }));
    await waitFor(() => expect(screen.getByTestId("payment-state")).toHaveTextContent("支付成功"));
    fireEvent.click(screen.getByRole("button", { name: "申请退款" }));
    await waitFor(() => expect(screen.getByTestId("payment-state")).toHaveTextContent("已退款"));
  });
});

describe("WalletPage — 红包（真实端点）", () => {
  it("发红包成功后提示红包 ID", async () => {
    const log = new CallLog();
    setTransport(
      loggingTransport(
        {
          ...balanceRoute,
          "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
          "POST /api/wallet/redpacket": (_m, _p, body) => {
            expect((body as Record<string, unknown>).amount).toBe(1000);
            expect((body as Record<string, unknown>).count).toBe(2);
            return { redpacket_id: "rp-1" };
          },
        },
        log,
      ),
    );
    render(<WalletPage />);
    fireEvent.click(await screen.findByTestId("wallet-tab-redpacket"));
    fireEvent.change(screen.getByTestId("redpacket-amount"), { target: { value: "10" } });
    fireEvent.change(screen.getByTestId("redpacket-count"), { target: { value: "2" } });
    fireEvent.click(screen.getByRole("button", { name: "塞钱进红包" }));
    await waitFor(() => expect(screen.getByText(/红包已创建：rp-1/)).toBeInTheDocument());
    expect(log.count({ path: "/api/wallet/redpacket" })).toBe(1);
  });

  it("领取红包显示金额；重复领取失败显示原因", async () => {
    let claims = 0;
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
        "POST /api/wallet/redpacket/:id/claim": () => {
          claims += 1;
          if (claims > 1) throw err(409, "already claimed");
          return { amount: 300 };
        },
      }),
    );
    render(<WalletPage />);
    fireEvent.click(await screen.findByTestId("wallet-tab-redpacket"));
    fireEvent.change(screen.getByTestId("redpacket-claim-id"), { target: { value: "rp-1" } });
    fireEvent.click(screen.getByRole("button", { name: "领取" }));
    await waitFor(() => expect(screen.getByTestId("redpacket-claimed")).toHaveTextContent("¥3.00"));
    fireEvent.click(screen.getByRole("button", { name: "领取" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("already claimed"));
  });
});

describe("WalletPage — 提现/绑卡/卡包（G06 契约）", () => {
  it("上游未开通：提现明确提示", async () => {
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
        "GET /api/cards": () => err(404, "not found"),
        "GET /api/entitlements": () => err(404, "not found"),
        "POST /api/withdrawals": () => err(404, "not found"),
      }),
    );
    render(<WalletPage />);
    fireEvent.click(await screen.findByTestId("wallet-tab-cards"));
    fireEvent.change(screen.getByTestId("withdraw-account"), { target: { value: "bank-1" } });
    fireEvent.change(screen.getByTestId("withdraw-amount"), { target: { value: "50" } });
    fireEvent.click(screen.getByRole("button", { name: "申请提现" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("提现不可用"));
  });

  it("提现失败恢复可操作：失败后重试成功", async () => {
    let attempts = 0;
    const ports: Partial<WalletPorts> = {
      cards: async () => [
        { card_id: "bank-1", kind: "real", label: "储蓄卡", masked_number: "0001", verified: true },
      ],
      entitlements: async () => [],
      withdraw: async () => {
        attempts += 1;
        if (attempts === 1) throw err(503, "channel busy, retry later");
        return {
          payment_id: "wd-1",
          amount_minor: 5000,
          currency: "CNY",
          state: "querying",
          account_kind: "real",
          created_at: Date.now(),
        };
      },
    };
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
      }),
    );
    render(<WalletPage ports={ports as WalletPorts} />);
    fireEvent.click(await screen.findByTestId("wallet-tab-cards"));
    fireEvent.change(screen.getByTestId("withdraw-account"), { target: { value: "bank-1" } });
    fireEvent.change(screen.getByTestId("withdraw-amount"), { target: { value: "50" } });
    fireEvent.click(screen.getByRole("button", { name: "申请提现" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("channel busy"));
    fireEvent.click(screen.getByRole("button", { name: "申请提现" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("查询中"));
    expect(attempts).toBe(2);
  });

  it("卡包区分真实账户与虚拟账户", async () => {
    const ports: Partial<WalletPorts> = {
      cards: async () => [
        { card_id: "bank-1", kind: "real", label: "储蓄卡", masked_number: "0001", verified: true },
        { card_id: "wallet", kind: "virtual", label: "零钱", masked_number: "—", verified: true },
      ],
      entitlements: async () => [
        { entitlement_id: "e1", title: "50 元券", issuer: "平台", expires_at: Date.now() + 86_400_000, status: "valid" },
      ],
    };
    setTransport(
      fixtureTransport({
        ...balanceRoute,
        "GET /api/wallet/ledger": () => ({ entries: [], cursor: 0, has_more: false }),
      }),
    );
    render(<WalletPage ports={ports as WalletPorts} />);
    fireEvent.click(await screen.findByTestId("wallet-tab-cards"));
    await waitFor(() => expect(screen.getByText("储蓄卡 ···· 0001")).toBeInTheDocument());
    expect(screen.getByText("真实账户")).toBeInTheDocument();
    expect(screen.getByText("50 元券")).toBeInTheDocument();
  });
});
