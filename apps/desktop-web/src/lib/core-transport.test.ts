/**
 * #294 钱包消费方用例 + 静态守卫用例（本仓里唯一一份，原来 `src/core-transport.test.ts`
 * 那份同名文件已并到这里——见文件末尾的守卫段）。
 *
 * ## 上半部分：钱包域的每一次读写都必须落在 `CoreHandle`/action 上，
 * 而且**永不**触碰独立 localStorage 里的 bearer/base。
 *
 * 这里刻意把 `globalThis.fetch` 装成"一调用就红"的探针：旧会话那份
 * `wxlike-web-token` 会预先躺在 localStorage 里（正是线上"分裂会话"的样子），
 * 只要有任何一条钱包请求绕过核心去发 HTTP，探针就炸。这比"断言返回值正确"
 * 更接近卡面要的东西——它证明的是**没有第二条路**，而不只是这条路今天走得通。
 *
 * ## 下半部分：`scripts/check-core-transport.mjs` 不是空转的
 *
 * 判定逻辑只有那一份实现，这里用 vitest 把它跑起来（口径同 `src/host-config.test.ts`
 * 跑 #158 守卫）。重点是**注入证明**：往规则引擎里喂人造违例（走脚本的 override 入口，
 * 内存里换一份文件，不改仓库——改文件会被并行用例读到半态，那种绿不作数），断言它报红，
 * 并且逐条报出注入那条的 `文件:行｜规则`。规则清单从 `--json` 的 `rules` 里读，用例必须
 * 与之一一对应——以后加一条规则却忘了配用例，这里就红。
 */

import { afterAll, afterEach, beforeEach, describe, expect, it } from "vitest";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
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

// ===========================================================================
// 静态守卫：`scripts/check-core-transport.mjs`（#294 DoD 第 5 条）
// 从 `src/core-transport.test.ts` 合并过来（那份是同名文件挪动后落下的旧位置）。
// ===========================================================================

const ROOT = path.resolve(HERE, "..", "..", "..", "..");
const CHECK = path.join(ROOT, "scripts", "check-core-transport.mjs");
const TMP = mkdtempSync(path.join(os.tmpdir(), "wxlike-294-"));

afterAll(() => rmSync(TMP, { recursive: true, force: true }));

interface Violation {
  file: string;
  path: string;
  line: number;
  rule: string;
  text: string;
}

interface CheckJson {
  rules: string[];
  domains: { id: string; featureDir: string; state: string; providerSha: string | null }[];
  routes: { method: string; pattern: string; action: string }[];
  bridgeArms: string[];
  contractActions: string[];
  coreOnlyPrefixes: string[];
  violations: Violation[];
  ledger: Record<string, { files: string[]; counts: Record<string, number> }>;
  total: number;
  problems: string[];
}

const REGISTRY_REL = "apps/desktop-web/src/lib/domain-registry.ts";
const TRANSPORT_REL = "apps/desktop-web/src/lib/core-transport.ts";
const BRIDGE_REL = "ui/src-tauri/src/core_bridge.rs";
const CALLS_API_REL = "apps/desktop-web/src/features/calls/api.ts";
/** 人造违例的落脚点：钱包（唯一已迁移域）里的一个探针文件，只存在于 override 里。 */
const PROBE_REL = "apps/desktop-web/src/features/commerce/_injected_probe.ts";

/** 读仓库里的源码作为注入底本——检出是 CRLF，先归一成 LF 再改，否则 replace 打不中。 */
function readSource(rel: string): string {
  return readFileSync(path.join(ROOT, rel), "utf8").replace(/\r\n/g, "\n");
}

function runCheck(args: string[] = [], overrides?: Record<string, string>): { status: number; stdout: string; stderr: string } {
  const env = { ...process.env };
  if (overrides) {
    const file = path.join(
      TMP,
      `overrides-${Object.keys(overrides)[0].replace(/[^\w]/g, "_")}-${Math.random().toString(36).slice(2)}.json`,
    );
    writeFileSync(file, JSON.stringify(overrides));
    env.WXLIKE_CORE_TRANSPORT_OVERRIDES = file;
  }
  const res = spawnSync(process.execPath, [CHECK, ...args], { cwd: ROOT, encoding: "utf8", env });
  return { status: res.status ?? -1, stdout: res.stdout, stderr: res.stderr };
}

function runJson(overrides?: Record<string, string>): CheckJson {
  const res = runCheck(["--json"], overrides);
  try {
    return JSON.parse(res.stdout) as CheckJson;
  } catch {
    throw new Error(`守卫 --json 输出不可解析（status=${res.status}）：\n${res.stdout}\n${res.stderr}`);
  }
}

/** 造一个"只有这一条违例"的合成模块：导入行本身不落在任何 pattern 上。 */
function probeModule(body: string): string {
  return [
    'import { ApiError, baseUrl, idemKey, request, token, userIdSafe } from "../../lib/http";',
    "",
    "export const probe = (): unknown => {",
    body,
    "};",
    "",
    "export const deps = [ApiError, baseUrl, idemKey, request, token, userIdSafe];",
    "",
  ].join("\n");
}

/**
 * 每条规则一个"人造违例"。`expect` 是这条违例必须落在**哪条判定**上（不是"随便红一下"），
 * 缺任何一条 = 规则引擎里有打死规则，用例本身也会因覆盖断言而红。
 */
const RULE_PROOFS: Record<string, { body: string; hay: string; expect: string }> = {
  direct_fetch: {
    body: '  return fetch("/api/wallet/balance");',
    hay: "fetch(",
    expect: "里仍有遗留会话用法",
  },
  legacy_transport: {
    body: '  return request("GET", "/api/legacy-probe");',
    hay: "request(",
    expect: "CORE_ONLY_PREFIXES 没拒绝它们",
  },
  legacy_token: {
    body: '  return localStorage.getItem("wxlike-web-token");',
    hay: "wxlike-web-token",
    expect: "里仍有遗留会话用法",
  },
  // 仓库里真实的取 bearer 写法就是裸 `token()`（files/video 的 xhr 分支），
  // 上一版规则只认 `.token()`，这条注入因此打不红——补成 legacy_bearer_call 之后才成立。
  legacy_bearer_call: {
    body: "  return token();",
    hay: "token()",
    expect: "里仍有遗留会话用法",
  },
  legacy_base: {
    body: "  return baseUrl();",
    hay: "baseUrl()",
    expect: "里仍有遗留会话用法",
  },
  legacy_user: {
    body: "  return userIdSafe();",
    hay: "userIdSafe()",
    expect: "里仍有遗留会话用法",
  },
};

/** 从 WALLET_ROUTES 里摘掉一整条路由（按行找块边界，免得被缩进/换行符绊倒）。 */
function dropRoute(text: string, pattern: string): string {
  const lines = text.split("\n");
  const at = lines.findIndex((line) => line.includes(`pattern: "${pattern}"`));
  if (at < 0) throw new Error(`WALLET_ROUTES 里找不到 ${pattern}：注入底本变了，这条证明要重写`);
  let start = at;
  while (start > 0 && !lines[start]!.trim().startsWith("{")) start -= 1;
  let end = at;
  while (end < lines.length && !lines[end]!.trim().startsWith("},")) end += 1;
  return [...lines.slice(0, start), ...lines.slice(end + 1)].join("\n");
}

// 每条注入证明都要真的起一个 node 进程跑守卫（spawnSync 是同步的，20 个 worker 并行时
// 单条可能跑到几秒），所以这里的超时比默认 5s 宽——放宽的是等待时间，不是断言强度。
describe("静态守卫：绕过 Core 的 fetch / localStorage 会话（#294 DoD 第 5 条）", { timeout: 60_000 }, () => {
  const disk = runJson();

  it("今天判定为绿，并把现存违例的条数如实打印出来（条数来自扫描，不是写死的常量）", () => {
    const res = runCheck();
    expect(res.stdout + res.stderr).toContain("OK: 已迁移域（wallet）零遗留 HTTP");
    expect(res.status).toBe(0);
    const count = /现存违例：(\d+) 条，分布在 (\d+) 个未迁移域/.exec(res.stdout);
    expect(count, `输出里必须自带现存违例计数，实际：${res.stdout}`).toBeTruthy();
    expect(Number(count?.[1]), "违例条数为 0 就说明这条检查在空转").toBeGreaterThan(0);
    expect(Number(count?.[1])).toBe(disk.total);
    expect(Number(count?.[2])).toBe(6);
  });

  it("六个未迁移域逐域都有现存违例（规则抓到的是真的，不是永远为真）", () => {
    expect(disk.problems).toEqual([]);
    expect(disk.total).toBeGreaterThan(0);
    const blocked = disk.domains.filter((d) => d.state === "blocked").map((d) => d.featureDir).sort();
    expect(blocked).toEqual(["calls", "content", "discover", "files", "mini", "video"]);
    for (const dir of blocked) {
      const entry = disk.ledger[dir];
      expect(entry, `债台账里应当有 features/${dir} 这一格`).toBeTruthy();
      expect(Object.keys(entry?.counts ?? {}).length, `features/${dir} 应该至少有一条现存违例`).toBeGreaterThan(0);
    }
    // 钱包是唯一的已迁移域，它的违例只许是"经公共 request() 走核心"这一种
    const commerceHits = disk.violations.filter((v) => v.file === "features/commerce");
    expect(commerceHits.length).toBeGreaterThan(0);
    expect(commerceHits.filter((v) => v.rule !== "legacy_transport")).toEqual([]);
  });

  it("四份 action 名单同源：路由表 ↔ core-contract ↔ Rust 桥（七条，不多不少）", () => {
    const routed = [...new Set(disk.routes.map((r) => r.action))].sort();
    expect(routed).toEqual([...new Set(disk.contractActions)].sort());
    expect(routed).toEqual([...new Set(disk.bridgeArms)].sort());
    expect(routed).toHaveLength(7);
  });

  it("规则清单与注入用例一一对应（往 RULES 里加一条打死规则就会在这里红）", () => {
    expect([...disk.rules].sort()).toEqual(Object.keys(RULE_PROOFS).sort());
  });

  it.each(Object.keys(RULE_PROOFS))("注入证明｜规则 %s：喂一条人造违例 → 报红并逐条报出位置", (rule) => {
    const proof = RULE_PROOFS[rule]!;
    const content = probeModule(proof.body);
    const injectedLine = content.split("\n").findIndex((line) => line.includes(proof.hay)) + 1;
    expect(injectedLine, "注入那行必须真的在合成模块里").toBeGreaterThan(0);

    const res = runCheck([], { [PROBE_REL]: content });
    expect(res.status, `规则 ${rule} 的人造违例没有让它报红`).toBe(1);
    expect(res.stderr).toContain(proof.expect);

    const json = runJson({ [PROBE_REL]: content });
    const hit = json.violations.find((v) => v.path === "features/commerce/_injected_probe.ts");
    expect(hit, `违例清单里没有注入的那条（${rule}）`).toBeTruthy();
    expect(hit?.rule).toBe(rule);
    expect(hit?.line).toBe(injectedLine);
    expect(hit?.text).toContain(proof.hay);
    // 只红在注入这一条上：其它文件的现存违例一条不多一条不少（override 不会整片失效）
    const others = json.violations.filter((v) => v.path !== "features/commerce/_injected_probe.ts");
    expect(others.length).toBe(disk.violations.length);
  });

  it("注入通道本身是活的：只加一行注释 → 依旧判绿，违例条数一字不变", () => {
    const api = readSource("apps/desktop-web/src/features/commerce/api.ts");
    const commented = `${api}\n// 注入证明用：这一行没有任何 HTTP 出口，不该被算成违例。\n`;
    const res = runCheck([], { "apps/desktop-web/src/features/commerce/api.ts": commented });
    expect(res.status).toBe(0);
    expect(runJson({ "apps/desktop-web/src/features/commerce/api.ts": commented }).violations.length).toBe(
      disk.violations.length,
    );
  });

  it("注入证明：允许名单写成包含全部七域 → 判红（红在「这些域没有 provider」，不是红在「名单长了」）", () => {
    const registry = readSource(REGISTRY_REL);
    const claimAllSeven = registry.replace(/state: "blocked"/g, 'state: "core"');
    expect(claimAllSeven).not.toBe(registry);
    const res = runCheck([], { [REGISTRY_REL]: claimAllSeven });
    expect(res.status).toBe(1);
    expect(res.stderr).toContain("FAIL");
    expect(res.stderr).toContain("里仍有遗留会话用法");
    expect(res.stderr).toContain("标成 core 却没有可解析的 providerSha");
    expect(runJson({ [REGISTRY_REL]: claimAllSeven }).problems.length).toBeGreaterThan(10);
  });

  it("注入证明：桥少注册一条钱包臂（UI 声称接了但桥没有）→ 判红", () => {
    const bridge = readSource(BRIDGE_REL);
    const missing = bridge.replace(/^\s*"wallet\.redpacket\.list" =>.*\r?\n/m, "");
    expect(missing).not.toBe(bridge);
    const res = runCheck([], { [BRIDGE_REL]: missing });
    expect(res.status).toBe(1);
    expect(res.stderr).toContain("与 core_bridge.rs 的 wallet_router 不一致");
  });

  it("注入证明：把一条已交付的路由从表里摘掉（声称迁完、其实出口少一条）→ 判红", () => {
    const transport = readSource(TRANSPORT_REL);
    const dropped = dropRoute(transport, "/api/wallet/balance");
    expect(dropped).not.toBe(transport);
    const res = runCheck([], { [TRANSPORT_REL]: dropped });
    expect(res.status).toBe(1);
    expect(res.stderr).toContain("与 core-contract 的 ACTIONS.wallet* 不一致");
  });

  it("注入证明：未迁移域悄悄加一条新违例 → 判红（债台账不是只往下走的摆设）", () => {
    const calls = readSource(CALLS_API_REL);
    const worse = `${calls}\n/** 注入证明用：通话域自己发了一次 HTTP。 */\nexport const _extraFetch = (url: string) => fetch(url);\n`;
    const res = runCheck([], { [CALLS_API_REL]: worse });
    expect(res.status).toBe(1);
    expect(res.stderr).toContain("债台账漂移：features/calls");
  });

  it("注入证明：某个域迁走了却不改基线/注册表 → 同样判红（台账只许「两边一起动」）", () => {
    const migratedAway = 'export const calls = { migrated: true };\n';
    const res = runCheck([], { [CALLS_API_REL]: migratedAway });
    expect(res.status).toBe(1);
    expect(res.stderr).toContain("登记为 blocked");
    expect(res.stderr).toContain("债台账漂移：features/calls");
  });

  it("注入证明：名单被清空/解析不出条目 → fail-closed，不会当成「零违例所以绿」", () => {
    const emptyRegistry = "export const DOMAIN_REGISTRY = [\n];\n";
    const registryRun = runCheck([], { [REGISTRY_REL]: emptyRegistry });
    expect(registryRun.status).toBe(1);
    expect(registryRun.stderr).toContain("解析出 0 个域");

    const transport = readSource(TRANSPORT_REL);
    const noRoutes = transport.replace(
      /export const WALLET_ROUTES: readonly WalletRoute\[\] = \[[\s\S]*?\n\];/,
      "export const WALLET_ROUTES: readonly WalletRoute[] = [\n];",
    );
    expect(noRoutes).not.toBe(transport);
    const routesRun = runCheck([], { [TRANSPORT_REL]: noRoutes });
    expect(routesRun.status).toBe(1);
    expect(routesRun.stderr).toContain("WALLET_ROUTES 解析出 0 条路由");
  });

  it("未知参数不再被静默忽略（打错 flag 不能等于把门关掉）", () => {
    const res = runCheck(["--config", "whatever.json"]);
    expect(res.status).toBe(2);
    expect(`${res.stderr}${res.stdout}`).toContain("未知参数");
  });

  it("override 入口指到不存在的文件时报错而不是静默按默认跑", () => {
    const res = spawnSync(process.execPath, [CHECK], {
      cwd: ROOT,
      encoding: "utf8",
      env: { ...process.env, WXLIKE_CORE_TRANSPORT_OVERRIDES: path.join(TMP, "nope.json") },
    });
    expect(res.status).not.toBe(0);
    expect(`${res.stderr}${res.stdout}`).toContain("WXLIKE_CORE_TRANSPORT_OVERRIDES");
  });
});
