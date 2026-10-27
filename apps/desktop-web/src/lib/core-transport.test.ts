/**
 * #294 钱包消费方用例：钱包域的每一次读写都必须落在 `CoreHandle`/action 上，
 * 而且**永不**触碰独立 localStorage 里的 bearer/base。
 *
 * 这里刻意把 `globalThis.fetch` 装成"一调用就红"的探针：旧会话那份
 * `wxlike-web-token` 会预先躺在 localStorage 里（正是线上"分裂会话"的样子），
 * 只要有任何一条钱包请求绕过核心去发 HTTP，探针就炸。这比"断言返回值正确"
 * 更接近卡面要的东西——它证明的是**没有第二条路**，而不只是这条路今天走得通。
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { ApiError, fetchTransport, isCoreOnlyPath, request, setTransport } from "./http";
import {
  BLOCKED_WALLET_PREFIXES,
  LEDGER_KINDS,
  WALLET_ROUTES,
  __resetCoreSessionForTests,
  coreSessionSnapshot,
  installCoreSession,
  isRoutedOrBlocked,
  resetCoreSession,
  uninstallCoreSession,
  walletRouteCensus,
} from "./core-transport";
import {
  ACTIONS,
  type ActionRequest,
  type ActionResponse,
  type CoreCapabilities,
  type CoreHandle,
  type ErrorCode,
} from "../shell/core-contract";
import { DOMAIN_REGISTRY } from "./domain-registry";

const HERE = path.dirname(fileURLToPath(import.meta.url));

interface Harness {
  core: CoreHandle;
  requests: ActionRequest[];
  /** 让第 N 条 dispatch 挂住，由测试决定何时放行（用来验会话切换的丢弃行为）。 */
  gate: <T>(response: ActionResponse<T>) => Promise<ActionResponse<T>>;
  release: () => void;
}

function makeCore(
  responder: (request: ActionRequest) => ActionResponse<unknown>,
  options: { delayFirst?: boolean } = {},
): Harness {
  const requests: ActionRequest[] = [];
  let release: () => void = () => undefined;
  const gate = <T,>(response: ActionResponse<T>): Promise<ActionResponse<T>> =>
    new Promise<ActionResponse<T>>((resolve) => {
      release = () => resolve(response);
    });
  const capabilities: CoreCapabilities = {
    contract_version: "test",
    transport: "fixture",
    actions: [
      ACTIONS.walletBalanceGet,
      ACTIONS.walletLedgerPage,
      ACTIONS.walletTransfer,
      ACTIONS.walletRedpacketCreate,
      ACTIONS.walletRedpacketGet,
      ACTIONS.walletRedpacketClaim,
      ACTIONS.walletRedpacketList,
    ],
    qr_login: false,
    secrets_os_backed: false,
    sync_range: false,
  };
  const core: CoreHandle = {
    async dispatch<T>(req: ActionRequest): Promise<ActionResponse<T>> {
      requests.push(req);
      const answer = responder(req) as ActionResponse<T>;
      if (options.delayFirst && requests.length === 1) return (await gate(answer)) as ActionResponse<T>;
      return answer;
    },
    subscribe: () => () => undefined,
    capabilities: async () => capabilities,
  };
  return { core, requests, gate, release: () => release() };
}

const ok = <T,>(data: T): ActionResponse<T> => ({ ok: true, data });

function failWith(code: ErrorCode, message: string): ActionResponse<never> {
  return { ok: false, error: { code, message, retryable: false, request_id: "test" } };
}

/** fetch 探针：被调用就记录，测试据此断言"一条都没发出去"。 */
let fetchCalls: string[] = [];
const realFetch = globalThis.fetch;

beforeEach(() => {
  __resetCoreSessionForTests();
  localStorage.clear();
  setTransport(null);
  fetchCalls = [];
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    fetchCalls.push(String(input));
    throw new Error(`不该发 HTTP：${String(input)}`);
  }) as typeof fetch;
});

afterEach(() => {
  globalThis.fetch = realFetch;
  __resetCoreSessionForTests();
  setTransport(null);
  localStorage.clear();
});

describe("钱包域 — 读写只经 CoreHandle", () => {
  it("余额：dispatch wallet.balance.get，并把桥形状投影成 api.ts 认的形状", async () => {
    localStorage.setItem("wxlike-web-token", "上一个用户的 bearer");
    localStorage.setItem("wxlike-server", "http://127.0.0.1:9999");
    const h = makeCore(() => ok({ balance_minor: 12345, currency: "CNY" }));
    installCoreSession(h.core);

    const body = await request<{ balance: number; currency: string }>("GET", "/api/wallet/balance");

    expect(body).toEqual({ balance: 12345, currency: "CNY" });
    expect(h.requests).toHaveLength(1);
    expect(h.requests[0]?.action_id).toBe("wallet.balance.get");
    expect(h.requests[0]?.schema_version).toBe(1);
    // 读的是核心会话，不是 localStorage：一次 HTTP 都没发，旧的 bearer 也没被动用。
    expect(fetchCalls).toEqual([]);
    expect(localStorage.getItem("wxlike-web-token")).toBe("上一个用户的 bearer");
  });

  it("流水：桥的 items 投影成 entries，金额按服务端 kind 定符号，时间转回毫秒", async () => {
    const h = makeCore(() =>
      ok({
        items: [
          {
            entry_id: 1,
            kind: "transfer_out",
            amount_minor: 500,
            counterparty: "user-b",
            ref_id: "t1",
            memo: "午饭",
            created_at_ms: "2026-10-27T00:00:00.000Z",
          },
          {
            entry_id: 2,
            kind: "redpacket_in",
            amount_minor: 300,
            counterparty: "user-c",
            ref_id: "rp1",
            memo: "",
            created_at_ms: "2026-10-27T01:00:00.000Z",
          },
        ],
        next_cursor: 42,
        has_more: true,
      }),
    );
    installCoreSession(h.core);

    const body = await request<{ entries: { id: number; amount: number; created_at: number }[]; cursor: number | null; has_more: boolean }>(
      "GET",
      "/api/wallet/ledger?since=0&limit=50",
    );

    expect(h.requests[0]?.action_id).toBe("wallet.ledger.page");
    expect(h.requests[0]?.params).toEqual({ since: 0, limit: 50 });
    expect(body.entries.map((row) => row.amount)).toEqual([-500, 300]);
    expect(body.entries[0]?.created_at).toBe(Date.parse("2026-10-27T00:00:00.000Z"));
    expect(body.cursor).toBe(42);
    expect(body.has_more).toBe(true);
    expect(fetchCalls).toEqual([]);
  });

  it("未知 kind 不猜方向：如实报错，不返回一条算错符号的账", async () => {
    const h = makeCore(() =>
      ok({
        items: [
          { entry_id: 1, kind: "mystery", amount_minor: 500, counterparty: "x", ref_id: "r", memo: "", created_at_ms: "2026-10-27T00:00:00.000Z" },
        ],
        next_cursor: null,
        has_more: false,
      }),
    );
    installCoreSession(h.core);
    const error = await request("GET", "/api/wallet/ledger").catch((err) => err);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).message).toContain("未知 kind");
    expect((error as ApiError).code).toBe("blocked");
  });

  it("桥缺字段就抛：不替 balance_minor 兜 0", async () => {
    const h = makeCore(() => ok({ currency: "CNY" }));
    installCoreSession(h.core);
    const error = await request("GET", "/api/wallet/balance").catch((err) => err);
    expect((error as ApiError).message).toContain("balance_minor");
    expect(fetchCalls).toEqual([]);
  });

  it("转账：调用方给的幂等键原样带上；不给就不发（桥不代编）", async () => {
    const h = makeCore(() => ok({ transfer_id: "tr1" }));
    installCoreSession(h.core);

    await request("POST", "/api/wallet/transfer", {
      body: { to_uid: "user-b", amount: 1999, memo: null },
      idempotencyKey: "wallet-transfer:fixed",
    });
    expect(h.requests[0]?.action_id).toBe("wallet.transfer");
    expect(h.requests[0]?.params).toEqual({ to_uid: "user-b", amount_minor: 1999, memo: null });
    expect(h.requests[0]?.idempotency_key).toBe("wallet-transfer:fixed");

    await request("POST", "/api/wallet/transfer", { body: { to_uid: "user-b", amount: 1 } });
    expect(h.requests[1]?.idempotency_key).toBeUndefined();
  });

  it("红包：创建/读取/领取各走自己的 action，领取金额按 amount_minor 回填", async () => {
    const h = makeCore((req) => {
      if (req.action_id === "wallet.redpacket.create") return ok({ redpacket_id: "rp-1" });
      if (req.action_id === "wallet.redpacket.get")
        return ok({
          redpacket_id: "rp-1",
          sender_id: "user-a",
          conv_id: "conv-1",
          total_minor: 1000,
          count: 2,
          claimed: 1,
          remaining: 1,
          claimed_total_minor: 400,
          memo: "恭喜",
          status: "active",
          created_at_ms: "2026-10-27T00:00:00.000Z",
          expire_at_ms: "2026-10-28T00:00:00.000Z",
        });
      return ok({ redpacket_id: "rp-1", amount_minor: 600 });
    });
    installCoreSession(h.core);

    expect(await request("POST", "/api/wallet/redpacket", { body: { amount: 1000, count: 2 } })).toEqual({ redpacket_id: "rp-1" });
    const record = await request<{ redpacket_id: string; remaining: number; expires_at: number; memo: string | null }>(
      "GET",
      "/api/wallet/redpacket/rp-1",
    );
    expect(record.redpacket_id).toBe("rp-1");
    expect(record.remaining).toBe(1);
    expect(record.expires_at).toBe(Date.parse("2026-10-28T00:00:00.000Z"));
    // 桥没给"我是否领过"，界面就不凭空补 claimed_by_me
    expect(record).not.toHaveProperty("claimed_by_me");
    expect(await request("POST", "/api/wallet/redpacket/rp-1/claim", { idempotencyKey: "claim:1" })).toEqual({
      amount: 600,
      redpacket_id: "rp-1",
    });
    expect(h.requests.map((r) => r.action_id)).toEqual([
      "wallet.redpacket.create",
      "wallet.redpacket.get",
      "wallet.redpacket.claim",
    ]);
    expect(h.requests[1]?.params).toEqual({ redpacket_id: "rp-1" });
  });

  it("服务端没有红包列表：桥回 NOT_IMPLEMENTED → 界面拿到 501 阻塞，而不是空数组假成功", async () => {
    const h = makeCore(() => failWith("NOT_IMPLEMENTED", "服务端没有红包列表路由"));
    installCoreSession(h.core);
    const error = await request("GET", "/api/wallet/redpacket").catch((err) => err);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(501);
    expect((error as ApiError).code).toBe("blocked");
    expect((error as ApiError).message).toContain("服务端没有红包列表路由");
    expect(fetchCalls).toEqual([]);
  });

  it("401/403 如实映射：核心说不许，就是不许，不折成空载荷", async () => {
    for (const [code, status, expected] of [
      ["UNAUTHENTICATED", 401, "unauthorized"],
      ["FORBIDDEN", 403, "forbidden"],
      ["VALIDATION", 400, "bad_request"],
      ["CONFLICT", 409, "conflict"],
      ["RATE_LIMITED", 429, "rate_limited"],
      ["OFFLINE", 0, "network"],
    ] as const) {
      const h = makeCore(() => failWith(code, `server said ${code}`));
      installCoreSession(h.core);
      const error = (await request("GET", "/api/wallet/balance").catch((err) => err)) as ApiError;
      expect(error, code).toBeInstanceOf(ApiError);
      expect(error.status, code).toBe(status);
      expect(error.code, code).toBe(expected);
      expect(error.message, code).toContain(`server said ${code}`);
    }
  });
});

describe("钱包域 — 遗留出口已经被拆掉", () => {
  it("CORE_ONLY 路径覆盖钱包与 G06 契约面，且每条都有归属", () => {
    for (const prefix of ["/api/wallet/balance", "/api/wallet/ledger", "/api/payment-intents", "/api/entitlements"]) {
      expect(isCoreOnlyPath(prefix), prefix).toBe(true);
      expect(isRoutedOrBlocked(prefix), prefix).toBe(true);
    }
    expect(isCoreOnlyPath("/api/media/m1")).toBe(false);
    expect(walletRouteCensus()).toHaveLength(WALLET_ROUTES.length);
    expect(BLOCKED_WALLET_PREFIXES.length).toBeGreaterThan(0);
  });

  it("直接调用遗留 fetchTransport 打钱包：拒绝，且一次 HTTP 都不发", async () => {
    localStorage.setItem("wxlike-web-token", "上一个用户的 bearer");
    localStorage.setItem("wxlike-server", "http://127.0.0.1:9999");
    const error = await fetchTransport("GET", "/api/wallet/balance").catch((err) => err);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).code).toBe("blocked");
    expect((error as ApiError).status).toBe(501);
    expect(fetchCalls).toEqual([]);
  });

  it("没装核心时打钱包：显式阻塞，绝不回落到 localStorage 会话", async () => {
    localStorage.setItem("wxlike-web-token", "旧 bearer");
    const error = await request("GET", "/api/wallet/balance").catch((err) => err);
    expect((error as ApiError).code).toBe("blocked");
    expect((error as ApiError).message).toContain("旧 HTTP 出口已停用");
    expect(fetchCalls).toEqual([]);
  });

  it("装上核心但会话还没就绪：同样是阻塞，不是空数据", async () => {
    const h = makeCore(() => ok({ balance_minor: 1, currency: "CNY" }));
    installCoreSession(h.core);
    uninstallCoreSession();
    // 卸载把默认出口换回遗留 fetch —— 它对钱包路径本身就是拒绝的，所以这里仍然
    // 不可能出现"拿旧 bearer 发一次请求"的行为。
    const error = (await request("GET", "/api/wallet/balance").catch((err) => err)) as ApiError;
    expect(error.code).toBe("blocked");
    expect(h.requests).toEqual([]);
    expect(fetchCalls).toEqual([]);
  });

  it("G06 契约面（支付意图/提现/绑卡…）没有 provider：阻塞，不发 HTTP", async () => {
    const h = makeCore(() => ok({}));
    installCoreSession(h.core);
    for (const [method, pathName] of [
      ["POST", "/api/payment-intents"],
      ["POST", "/api/withdrawals"],
      ["GET", "/api/cards"],
      ["GET", "/api/entitlements"],
    ] as const) {
      const error = (await request(method, pathName).catch((err) => err)) as ApiError;
      expect(error, pathName).toBeInstanceOf(ApiError);
      expect(error.code, pathName).toBe("blocked");
      expect(error.status, pathName).toBe(501);
    }
    expect(h.requests).toEqual([]);
    expect(fetchCalls).toEqual([]);
  });

  it("未迁移的域仍然走遗留出口（本卡只动钱包这一格）", async () => {
    const h = makeCore(() => ok({}));
    installCoreSession(h.core);
    globalThis.fetch = (async () => ({
      ok: true,
      status: 200,
      text: async () => JSON.stringify({ ok: true }),
    })) as unknown as typeof fetch;
    try {
      const body = await request<{ ok: boolean }>("GET", "/api/moments/list");
      expect(body).toEqual({ ok: true });
      expect(h.requests).toEqual([]);
    } finally {
      globalThis.fetch = (async (input: RequestInfo | URL) => {
        fetchCalls.push(String(input));
        throw new Error(`不该发 HTTP：${String(input)}`);
      }) as typeof fetch;
    }
  });
});

describe("会话切换 — 真清，不是注释", () => {
  it("切换前挂着的钱包请求：结果落地时被丢弃，并计入 dropped 台账", async () => {
    const h = makeCore(() => ok({ balance_minor: 999999, currency: "CNY" }), { delayFirst: true });
    installCoreSession(h.core);
    const before = coreSessionSnapshot();
    const inflight = request("GET", "/api/wallet/balance");
    await Promise.resolve();
    expect(coreSessionSnapshot().pending).toHaveLength(1);

    const report = resetCoreSession("account-switch");
    expect(report.dropped).toBe(1);
    expect(coreSessionSnapshot().pending).toEqual([]);
    expect(coreSessionSnapshot().generation).toBe(before.generation + 1);

    h.release();
    const error = await inflight.catch((err) => err);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).code).toBe("cancelled");
    expect((error as ApiError).message).toContain("会话已切换");
    // 上一个用户的余额一个字都没落到新会话里
    expect(String((error as ApiError).message)).not.toContain("999999");
  });

  it("切换会抹掉 localStorage 里残留的 bearer/base/身份，并报告删了哪几个键", () => {
    localStorage.setItem("wxlike-web-token", "旧 bearer");
    localStorage.setItem("wxlike-web-user", "old-user");
    localStorage.setItem("wxlike-server", "http://127.0.0.1:8099");
    const report = resetCoreSession("logout");
    expect(report.clearedKeys.sort()).toEqual(["wxlike-server", "wxlike-web-token", "wxlike-web-user"]);
    expect(localStorage.length).toBe(0);
  });

  it("调用方取消（signal）：如实已取消，不把半截数据交回去", async () => {
    const h = makeCore(() => ok({ balance_minor: 1, currency: "CNY" }), { delayFirst: true });
    installCoreSession(h.core);
    const controller = new AbortController();
    const inflight = request("GET", "/api/wallet/balance", { signal: controller.signal });
    await Promise.resolve();
    controller.abort();
    const error = await inflight.catch((err) => err);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).code).toBe("cancelled");
    // 在途台账随取消一起收掉，不留一条"谁还在等旧会话"的挂账
    expect(coreSessionSnapshot().pending).toEqual([]);
    h.release();
  });
});

describe("台账漂移守卫 — 与 Rust 服务端同源", () => {
  it("LEDGER_KINDS 与 crates/platform/src/wallet.rs 的 mod kind 完全一致", () => {
    const rust = readFileSync(path.resolve(HERE, "../../../../crates/platform/src/wallet.rs"), "utf8").replace(/\r\n/g, "\n");
    const block = rust.match(/pub mod kind\s*\{([\s\S]*?)\n\}/);
    expect(block, "wallet.rs 里找不到 pub mod kind").toBeTruthy();
    const consts = [...(block?.[1] ?? "").matchAll(/pub const \w+: &str = "([^"]+)";/g)].map((m) => m[1]);
    expect(consts.sort()).toEqual(LEDGER_KINDS);
  });

  it("DOMAIN_REGISTRY 里 core 域的 action 名单与路由表一致（七条，不多不少）", () => {
    const wallet = DOMAIN_REGISTRY.find((domain) => domain.id === "wallet");
    expect(wallet?.state).toBe("core");
    expect(wallet?.providerSha).toBe("71cc90c5");
    expect([...(wallet?.actions ?? [])].sort()).toEqual(WALLET_ROUTES.map((r) => r.action).sort());
    expect([...new Set(WALLET_ROUTES.map((r) => r.action))]).toHaveLength(WALLET_ROUTES.length);
    expect(DOMAIN_REGISTRY.filter((d) => d.state === "blocked")).toHaveLength(6);
  });
});
