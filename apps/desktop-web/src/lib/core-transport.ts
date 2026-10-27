/**
 * 受控迁移层的核心路由（issue #294 / G10）：钱包域 → `CoreHandle`/action。
 *
 * ## 它解决的具体故障
 *
 * 迁移前钱包的调用链是 `features/commerce/api.ts` → `request()` → `http.ts` 的
 * `fetchTransport` → `baseUrl()`/`token()` 从 localStorage 里取一份**没人写过**的
 * 会话（`setBaseUrl`/`saveSession` 全仓零调用方）。后果就是卡面说的"分裂会话 +
 * 点进去全是空"：登录写进 Rust 核心，界面却拿另一份空 storage 发无鉴权 HTTP。
 *
 * ## 迁移后的形状（调用方一行不改）
 *
 * `features/commerce/api.ts` 由 G11 顺序消费，本卡不能同时改它，所以适配做在这一层：
 * 它仍然调 `request("GET", "/api/wallet/balance")`，装好之后这句话变成
 * `core.dispatch({ action_id: "wallet.balance.get", … })`，返回值由桥的形状投影回
 * `api.ts` 本来就在解析的服务端形状。base/Bearer 从此不进 webview（#293 把它们留在
 * Rust 侧的 `Http` 里），非 2xx 由桥映射成 `UNAUTHENTICATED`/`FORBIDDEN`…，本层再
 * 映射成 `ApiError`，**不折成空载荷**。
 *
 * ## 三条硬规矩
 *
 * 1. **不回落**：已迁移域的路径若没有对应 action（G06 的支付意图/提现/绑卡…），
 *    返回 `blocked`+501 的显式阻塞，绝不"先试试旧 HTTP"。
 * 2. **不编字段**：投影只搬运桥给的字段；缺字段就抛错，未知 kind 不猜方向。
 * 3. **不跨会话**：每次 dispatch 记 generation；会话一切换，旧代次的结果一律丢弃
 *    并把 `request_id` 从在途台账里摘掉（`resetCoreSession` 返回被丢了几条，
 *    用例据此断言，而不是靠注释说"这里会清"）。
 */

import {
  ApiError,
  blockedError,
  clearLegacySession,
  fetchTransport,
  isCoreOnlyPath,
  setFallbackTransport,
  type ApiErrorCode,
  type Json,
  type Transport,
} from "./http";
import { newRequest, ACTIONS, type CoreHandle, type DomainError } from "../shell/core-contract";

/**
 * 钱包 action 的名字只在 `shell/core-contract.ts` 的 `ACTIONS` 里有一份；这里只是
 * 给路由表起个短别名，避免在本文件再抄一遍字符串（#296 定的"名单只留一处"）。
 */
const WALLET_ACTIONS = {
  balanceGet: ACTIONS.walletBalanceGet,
  ledgerPage: ACTIONS.walletLedgerPage,
  transfer: ACTIONS.walletTransfer,
  redpacketCreate: ACTIONS.walletRedpacketCreate,
  redpacketList: ACTIONS.walletRedpacketList,
  redpacketGet: ACTIONS.walletRedpacketGet,
  redpacketClaim: ACTIONS.walletRedpacketClaim,
} as const;

export interface CoreSessionSnapshot {
  installed: boolean;
  generation: number;
  /** 在途（已 dispatch、还没落地）的 request_id。会话切换会清空这张台账。 */
  pending: string[];
  /** 本代次被丢弃的旧会话结果条数（可观测的"不泄露"计数）。 */
  dropped: number;
}

interface Session {
  core: CoreHandle | null;
  generation: number;
  pending: Set<string>;
  dropped: number;
}

let session: Session = { core: null, generation: 0, pending: new Set(), dropped: 0 };

/** 装上 Rust 核心：把默认出口换成"按域路由"。返回装好后的代次。 */
export function installCoreSession(core: CoreHandle): number {
  session = { ...session, core };
  setFallbackTransport(coreRoutingTransport());
  return session.generation;
}

/** 卸载（宿主没了/测试收尾）：默认出口退回遗留 fetch。 */
export function uninstallCoreSession(): void {
  session = { ...session, core: null };
  setFallbackTransport(null);
}

/**
 * 服务器切换 / 账号切换 / 退出登录时必须走这里。三件事一次做完，返回值让断言有得比：
 *
 * * `generation += 1`：旧代次在途请求落地时**一律丢弃**（`staleError`），所以
 *   上一个用户的转账/领红包结果不会写进新会话的界面；
 * * `pending` 台账清空、`dropped` 计数增加 —— 用例断言"切换前挂着的这条确实被算了"；
 * * `clearLegacySession()` 抹掉 localStorage 里残留的 bearer/base/身份（分裂会话的
 *   另一半来源），返回实际删掉的键名，断言得以落在"真的删了哪几个键"。
 */
export function resetCoreSession(reason: string): {
  generation: number;
  dropped: number;
  clearedKeys: string[];
  reason: string;
} {
  const dropped = session.pending.size;
  session = {
    core: session.core,
    generation: session.generation + 1,
    pending: new Set(),
    dropped: session.dropped + dropped,
  };
  const clearedKeys = clearLegacySession();
  return { generation: session.generation, dropped, clearedKeys, reason };
}

export function coreSessionSnapshot(): CoreSessionSnapshot {
  return {
    installed: session.core !== null,
    generation: session.generation,
    pending: [...session.pending],
    dropped: session.dropped,
  };
}

/** 只给用例读：重置全部模块级状态（代次与台账），避免跨用例串味。 */
export function __resetCoreSessionForTests(): void {
  session = { core: null, generation: 0, pending: new Set(), dropped: 0 };
  setFallbackTransport(null);
}

// ---------------------------------------------------------------------------
// 钱包路由表：URL → action →（响应投影）
// ---------------------------------------------------------------------------

interface Match {
  params: Record<string, string>;
  query: URLSearchParams;
}

interface WalletRoute {
  method: "GET" | "POST";
  /** 路径模板，`:x` 为段参数；不含 query。 */
  pattern: string;
  action: string;
  /** 从 HTTP 形状拼 action params。缺字段就少传，由桥如实报错——这里不兜默认值。 */
  toParams(match: Match, body: Json): Json;
  /** 桥的返回形状 → `features/commerce/api.ts` 本来就在解析的形状。 */
  project?(data: unknown): unknown;
}

export const WALLET_ROUTES: readonly WalletRoute[] = [
  {
    method: "GET",
    pattern: "/api/wallet/balance",
    action: WALLET_ACTIONS.balanceGet,
    toParams: () => ({}),
    // 桥：{balance_minor, currency}；api.ts：{balance}（整数分）
    project: (data) => ({ balance: needNumber(data, "balance_minor"), currency: needString(data, "currency") }),
  },
  {
    method: "GET",
    pattern: "/api/wallet/ledger",
    action: WALLET_ACTIONS.ledgerPage,
    toParams: ({ query }) => {
      const params: Json = {};
      const since = query.get("since");
      const limit = query.get("limit");
      if (since !== null) params.since = Number(since);
      if (limit !== null) params.limit = Number(limit);
      return params;
    },
    project: (data) => ({
      entries: needArray(data, "items").map((row) => ledgerRow(row)),
      cursor: optionalNumberOrNull(data, "next_cursor"),
      has_more: needBoolean(data, "has_more"),
    }),
  },
  {
    method: "POST",
    pattern: "/api/wallet/transfer",
    action: WALLET_ACTIONS.transfer,
    // api.ts 发的是服务端字段名 amount（整数分），桥的入参叫 amount_minor。
    toParams: (_match, body) => ({
      to_uid: body.to_uid,
      amount_minor: body.amount,
      memo: body.memo ?? null,
    }),
    project: (data) => ({ transfer_id: needString(data, "transfer_id") }),
  },
  {
    method: "POST",
    pattern: "/api/wallet/redpacket",
    action: WALLET_ACTIONS.redpacketCreate,
    toParams: (_match, body) => ({
      amount_minor: body.amount,
      count: body.count,
      memo: body.memo ?? null,
      conv_id: body.conv_id ?? null,
    }),
    project: (data) => ({ redpacket_id: needString(data, "redpacket_id") }),
  },
  {
    method: "GET",
    pattern: "/api/wallet/redpacket",
    action: WALLET_ACTIONS.redpacketList,
    toParams: ({ query }) => {
      const params: Json = {};
      const limit = query.get("limit");
      if (limit !== null) params.limit = Number(limit);
      return params;
    },
    // 桥对这条如实回 NOT_IMPLEMENTED（服务端没有红包列表路由），所以没有可投影的
    // 数据形状；本条存在的意义是让这个路径"登记在册"，不会偷偷回落到旧 HTTP。
  },
  {
    method: "GET",
    pattern: "/api/wallet/redpacket/:redpacket_id",
    action: WALLET_ACTIONS.redpacketGet,
    toParams: (match) => ({ redpacket_id: match.params.redpacket_id }),
    project: (data) => redpacketRecord(data),
  },
  {
    method: "POST",
    pattern: "/api/wallet/redpacket/:redpacket_id/claim",
    action: WALLET_ACTIONS.redpacketClaim,
    toParams: (match) => ({ redpacket_id: match.params.redpacket_id }),
    // 桥：{redpacket_id, amount_minor}；api.ts 读 {amount}
    project: (data) => ({ amount: needNumber(data, "amount_minor"), redpacket_id: needString(data, "redpacket_id") }),
  },
];

export function walletRouteCensus(): string[] {
  return WALLET_ROUTES.map((route) => `${route.method} ${route.pattern} -> ${route.action}`);
}

/**
 * 已迁移域里**明确没有 provider** 的出口：登记在此，运行时一律 `blocked`（501），
 * 静态检查据此确认"钱包域的每条路径都有归属"——要么进 `WALLET_ROUTES` 走核心，
 * 要么进这张表显式阻塞。两条都不在就是"偷偷回落到旧 HTTP"，判红。
 *
 * 这些都是 G06 CommercePort 的契约面（`features/commerce/api.ts` 还在按服务端 REST
 * 形状调用它们）。#293 没有交付这些 action，所以这里不猜、不代打、不返回空数组。
 */
export const BLOCKED_WALLET_PREFIXES: readonly string[] = [
  "/api/payment-intents",
  "/api/payments",
  "/api/withdrawals",
  "/api/topups",
  "/api/cards",
  "/api/entitlements",
];

/** 这个已迁移域路径是否有明确归属（路由 or 显式阻塞）。 */
export function isRoutedOrBlocked(path: string): boolean {
  const bare = path.split("?")[0] ?? path;
  if (BLOCKED_WALLET_PREFIXES.some((prefix) => bare === prefix || bare.startsWith(`${prefix}/`))) return true;
  return WALLET_ROUTES.some((route) => bare === route.pattern || bare.startsWith(`${route.pattern}/`));
}

/** 路径（去掉 query）+ 方法 → 路由条目；不匹配返回 null。 */
function matchWalletRoute(method: string, path: string): { route: WalletRoute; match: Match } | null {
  const [bare = "", text = ""] = path.split("?");
  const parts = bare.split("/").filter(Boolean);
  for (const route of WALLET_ROUTES) {
    if (route.method !== method) continue;
    const template = route.pattern.split("/").filter(Boolean);
    if (template.length !== parts.length) continue;
    const params: Record<string, string> = {};
    let ok = true;
    for (let i = 0; i < template.length; i += 1) {
      const token = template[i] as string;
      const actual = parts[i] as string;
      if (token.startsWith(":")) params[token.slice(1)] = decodeURIComponent(actual);
      else if (token !== actual) {
        ok = false;
        break;
      }
    }
    if (ok) return { route, match: { params, query: new URLSearchParams(text) } };
  }
  return null;
}

// ---------------------------------------------------------------------------
// 投影用到的严格读数（缺字段就抛，不兜默认值）
// ---------------------------------------------------------------------------

function asRecord(value: unknown, field: string): Json {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new ApiError(502, `核心响应里 ${field} 不是对象（不猜形状）`, field, "server");
  }
  return value as Json;
}

function needNumber(value: unknown, key: string): number {
  const row = asRecord(value, key);
  const raw = row[key];
  if (typeof raw !== "number" || !Number.isSafeInteger(raw)) {
    throw new ApiError(502, `核心响应缺 ${key}（整数分），界面不编造金额`, key, "server");
  }
  return raw;
}

function optionalNumberOrNull(value: unknown, key: string): number | null {
  const raw = asRecord(value, key)[key];
  if (raw === null || raw === undefined) return null;
  if (typeof raw !== "number" || !Number.isSafeInteger(raw)) {
    throw new ApiError(502, `核心响应的 ${key} 不是整数游标，界面不编造分页位置`, key, "server");
  }
  return raw;
}

function needString(value: unknown, key: string): string {
  const raw = asRecord(value, key)[key];
  if (typeof raw !== "string") {
    throw new ApiError(502, `核心响应缺 ${key}，界面不编造标识`, key, "server");
  }
  return raw;
}

function needBoolean(value: unknown, key: string): boolean {
  const raw = asRecord(value, key)[key];
  if (typeof raw !== "boolean") {
    throw new ApiError(502, `核心响应缺 ${key}，界面不猜分页状态`, key, "server");
  }
  return raw;
}

function needOptStringOrNull(value: unknown, key: string): string | null {
  const raw = asRecord(value, key)[key];
  if (raw === null || raw === undefined) return null;
  if (typeof raw !== "string") {
    throw new ApiError(502, `核心响应的 ${key} 不是字符串`, key, "server");
  }
  return raw;
}

function needArray(value: unknown, key: string): unknown[] {
  const raw = asRecord(value, key)[key];
  if (!Array.isArray(raw)) {
    throw new ApiError(502, `核心响应缺 ${key} 数组`, key, "server");
  }
  return raw;
}

function needIsoMs(value: unknown, key: string): number {
  const text = needString(value, key);
  const ms = Date.parse(text);
  if (Number.isNaN(ms)) {
    throw new ApiError(502, `核心响应的 ${key} 不是可解析的时间（${text}）`, key, "server");
  }
  return ms;
}

/**
 * kind → 记账方向。**这份名单必须与 `crates/platform/src/wallet.rs` 的 `mod kind`
 * 一致**（`wallet-core.test.ts` 直接读那个 Rust 源文件比对，防止两边各写一套）。
 * 桥刻意不给金额补符号（方向是服务端口径），界面要显示 `+/-` 就只能按 kind 判，
 * 判不出来就报错 —— 认不出的 kind 兜成"收入"是把账做错。
 */
const LEDGER_CREDIT = new Set(["transfer_in", "redpacket_in", "redpacket_refund", "fund_in"]);
const LEDGER_DEBIT = new Set(["transfer_out", "redpacket_out"]);

export const LEDGER_KINDS = [...LEDGER_CREDIT, ...LEDGER_DEBIT].sort();

function ledgerRow(row: unknown): Json {
  const source = asRecord(row, "entry");
  const kind = needString(source, "kind");
  const amount = needNumber(source, "amount_minor");
  const signed = LEDGER_CREDIT.has(kind) ? amount : LEDGER_DEBIT.has(kind) ? -amount : null;
  if (signed === null) {
    throw new ApiError(502, `账本出现未知 kind="${kind}"：方向由服务端决定，界面不猜`, "wallet.ledger", "blocked");
  }
  return {
    id: needNumber(source, "entry_id"),
    kind,
    amount: signed,
    counterparty: needString(source, "counterparty"),
    ref_id: needString(source, "ref_id"),
    memo: needString(source, "memo"),
    created_at: needIsoMs(source, "created_at_ms"),
  };
}

function redpacketRecord(data: unknown): Json {
  const row = asRecord(data, "redpacket");
  const record: Json = {
    redpacket_id: needString(row, "redpacket_id"),
    total_minor: needNumber(row, "total_minor"),
    count: needNumber(row, "count"),
    remaining: needNumber(row, "remaining"),
    memo: needOptStringOrNull(row, "memo"),
    expires_at: needIsoMs(row, "expire_at_ms"),
    status: needString(row, "status"),
  };
  // claimed_by_me 需要"当前身份"，桥没给身份口径，界面就不自己判（不编造）。
  return record;
}

// ---------------------------------------------------------------------------
// 错误映射与 transport
// ---------------------------------------------------------------------------

/** 桥的 ErrorCode → 遗留 ApiError：一一对应，绝不把失败折成"空但成功"。 */
export function toApiError(error: DomainError, path: string): ApiError {
  const table: Record<DomainError["code"], { status: number; code: ApiErrorCode }> = {
    UNAUTHENTICATED: { status: 401, code: "unauthorized" },
    FORBIDDEN: { status: 403, code: "forbidden" },
    VALIDATION: { status: 400, code: "bad_request" },
    CONFLICT: { status: 409, code: "conflict" },
    RATE_LIMITED: { status: 429, code: "rate_limited" },
    REQUIRES_CONFIRMATION: { status: 409, code: "conflict" },
    DEPENDENCY_UNAVAILABLE: { status: 503, code: "server" },
    // NOT_IMPLEMENTED = 服务端/桥确实没有这条能力：501 且 code=blocked，
    // 业务侧 isUpstreamMissing() 认 501，于是显示"上游未开通"而不是假数据。
    NOT_IMPLEMENTED: { status: 501, code: "blocked" },
    OFFLINE: { status: 0, code: "network" },
    CANCELLED: { status: 0, code: "cancelled" },
    INTERNAL: { status: 0, code: "server" },
  };
  const mapped = table[error.code] ?? { status: 0, code: "server" as ApiErrorCode };
  return new ApiError(mapped.status, error.message || `核心返回 ${error.code}`, path, mapped.code);
}

function staleError(path: string, generation: number): ApiError {
  return new ApiError(
    0,
    `会话已切换（请求属于第 ${generation} 代，当前第 ${session.generation} 代）：结果按旧用户任务丢弃，不写入新会话`,
    path,
    "cancelled",
  );
}

/**
 * 装上后的默认出口。未迁移的域原样转给遗留 `fetchTransport`（它们的阻塞状态由
 * `app.tsx` 的组合根横幅表达，迁移由各自 provider 卡负责），已迁移域只有核心一条路。
 */
function coreRoutingTransport(): Transport {
  return async (method, path, opts = {}) => {
    if (!isCoreOnlyPath(path)) return fetchTransport(method, path, opts);

    const core = session.core;
    if (!core) {
      throw blockedError(path, "组合根尚未装上 Rust 核心会话");
    }
    const hit = matchWalletRoute(method, path);
    if (!hit) {
      throw blockedError(
        path,
        `${method} ${path} 在 #293 的桥里没有对应 action（该能力没有 provider，不允许回落到遗留 HTTP）`,
      );
    }
    const { route, match } = hit;
    const body = (opts.body ?? {}) as Json;
    const request = newRequest(route.action, route.toParams(match, body), opts.idempotencyKey);
    const generation = session.generation;
    session.pending.add(request.request_id);

    if (opts.signal?.aborted) {
      session.pending.delete(request.request_id);
      throw new ApiError(0, "已取消", path, "cancelled");
    }

    try {
      // 调用方给了 signal 就一定要尊重：桥/IPC 这层没有撤销语义（ActionRequest 不带
      // signal），所以取消表现成"结果被丢弃"，而不是"服务端那笔没发生"。绝不把取消
      // 折成一次成功，也不把半截数据交回去。
      const dispatch = core.dispatch<unknown>(request);
      const response = opts.signal ? await raceAbort(dispatch, opts.signal, path) : await dispatch;
      if (generation !== session.generation) throw staleError(path, generation);
      if (!response.ok) throw toApiError(response.error, path);
      return route.project ? route.project(response.data) : response.data;
    } catch (err) {
      if (err instanceof ApiError) throw err;
      throw new ApiError(0, `核心不可达: ${err instanceof Error ? err.message : String(err)}`, path, "network");
    } finally {
      session.pending.delete(request.request_id);
    }
  };
}

/** signal 触发时让调用方的 Promise 先失败；核心那条 dispatch 继续跑完但不落地。 */
function raceAbort<T>(promise: Promise<T>, signal: AbortSignal, path: string): Promise<T> {
  if (signal.aborted) return Promise.reject(new ApiError(0, "已取消", path, "cancelled"));
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(new ApiError(0, "已取消（核心结果稍后到达，按旧请求丢弃）", path, "cancelled"));
    signal.addEventListener("abort", onAbort, { once: true });
    promise
      .then((value) => {
        signal.removeEventListener("abort", onAbort);
        resolve(value);
      })
      .catch((err) => {
        signal.removeEventListener("abort", onAbort);
        reject(err instanceof Error ? err : new Error(String(err)));
      });
  });
}
