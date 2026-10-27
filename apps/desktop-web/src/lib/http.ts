/**
 * HTTP transport for the desktop-web client (FP-G11).
 *
 * Same wire conventions as the shipping web build (`ui/src/web/session.ts`):
 * same-origin by default, `Authorization: Bearer`, `Idempotency-Key` for write
 * retries, `{"error": "…"}` bodies from `crates/platform` — but the transport
 * itself is injected so feature code never reaches for `fetch` directly:
 *
 * - tests drive a fixture transport (upstream cards G04/G05/G06/G07/G08 are
 *   still open; the delivery book allows fixture development until they merge);
 * - every feature client takes the transport in its constructor, so pointing a
 *   suite at the real server later is a one-line change, not a rewrite.
 *
 * Deliberately absent: business rules. Every decision stays on the server.
 *
 * ## 受控迁移层（issue #294 / G10）
 *
 * 这个文件同时是"旧 localStorage 会话"与"认证 Rust 核心会话"的唯一交接点。规矩
 * 只有一条：**一个域一旦由 Rust 提供者给出 action（#293 的钱包域），它就只能走
 * 核心，回不去旧出口**。落地方式是两份东西：
 *
 * 1. `CORE_ONLY_PREFIXES` —— 已迁移域的 URL 前缀。`fetchTransport` 在**函数第一
 *    行**拒绝它们，所以哪怕核心没装上、哪怕 localStorage 里还躺着上一个用户的
 *    bearer，钱包的请求也绝不会自己发出去（这是"点进去全是空"的根因：旧出口拿的
 *    是一份没人写入的 localStorage 会话，见 `setBaseUrl`/`saveSession` 全仓零调用
 *    方）。拒绝的返回值是 `blocked` + HTTP 501，业务侧看到的是显式阻塞，不是假成功。
 * 2. `setFallbackTransport()` —— 组合根（`shell/store.tsx` 的 AppProvider）把
 *    `lib/core-transport.ts` 造出来的"按域路由"的 transport 装成默认值；未迁移的
 *    域仍由它转交给 `fetchTransport`，已迁移的域走 `CoreHandle.dispatch`。
 *
 * 测试用 `setTransport()` 装的显式 transport 优先级最高（`?? ` 短路），所以各域
 * 现存的 fixture 用例不必改动；生产路径里没人调用它，装配关系就只剩"核心 or 阻塞"。
 */

const KEY_BASE = "wxlike-server";
const KEY_TOKEN = "wxlike-web-token";
const KEY_USER = "wxlike-web-user";

/**
 * 已迁移到 Rust 核心的域（#294 第一格：钱包）。表里是这些域的 URL 前缀，
 * `fetchTransport` 见到就**如实拒绝**——它不再有权替这些域发 HTTP。
 *
 * 名单与 `lib/core-transport.ts` 的 `DOMAIN_REGISTRY` 里 `state: "core"` 的那些
 * 条目必须一致；`scripts/check-core-transport.mjs` 逐条核对，写歪就红。
 * `/api/payment-intents`、`/api/payments`、`/api/withdrawals`、`/api/topups`、
 * `/api/cards`、`/api/entitlements` 是钱包页的 G06 契约面：**服务端与桥都还没有
 * 对应 action**，所以它们同样归在本域前缀下、由迁移层返回阻塞而不是去戳旧会话。
 */
const CORE_ONLY_PREFIXES: readonly string[] = [
  "/api/wallet",
  "/api/payment-intents",
  "/api/payments",
  "/api/withdrawals",
  "/api/topups",
  "/api/cards",
  "/api/entitlements",
];

/** 这个请求路径是否属于"只能经 Rust 核心"的已迁移域。 */
export function isCoreOnlyPath(path: string): boolean {
  const bare = path.split("?")[0] ?? path;
  return CORE_ONLY_PREFIXES.some((prefix) => bare === prefix || bare.startsWith(`${prefix}/`));
}

export type Json = Record<string, unknown>;

export function baseUrl(): string {
  if (typeof localStorage === "undefined") return "http://127.0.0.1:8099";
  const stored = localStorage.getItem(KEY_BASE);
  if (stored) return stored.replace(/\/+$/, "");
  if (typeof location === "undefined") return "http://127.0.0.1:8099";
  return `${location.protocol}//${location.host}`;
}

export function setBaseUrl(raw: string): void {
  const base = raw.trim().replace(/\/+$/, "");
  if (!/^https?:\/\/.+/.test(base)) throw new Error(`服务器地址无效: ${raw}`);
  if (typeof location !== "undefined" && location.protocol === "https:" && base.startsWith("http://")) {
    throw new Error("当前页面是 https，服务器地址也必须用 https");
  }
  localStorage.setItem(KEY_BASE, base);
}

export function token(): string | null {
  if (typeof localStorage === "undefined") return null;
  return localStorage.getItem(KEY_TOKEN);
}

export function userId(): string {
  if (typeof localStorage === "undefined") throw new Error("未登录");
  const id = localStorage.getItem(KEY_USER);
  if (!id) throw new Error("未登录");
  return id;
}

export function userIdSafe(): string {
  if (typeof localStorage === "undefined") return "";
  return localStorage.getItem(KEY_USER) ?? "";
}

export function saveSession(reply: Json): void {
  const id = String(reply["user_id"] ?? "");
  const tok = String(reply["token"] ?? "");
  if (!id || !tok) throw new Error("登录响应缺少 user_id/token");
  localStorage.setItem(KEY_USER, id);
  localStorage.setItem(KEY_TOKEN, tok);
}

export function clearSession(): void {
  localStorage.removeItem(KEY_TOKEN);
  localStorage.removeItem(KEY_USER);
}

/** 旧会话在 localStorage 里留下的全部键（bearer / base / 身份）。 */
export const LEGACY_SESSION_KEYS: readonly string[] = [KEY_TOKEN, KEY_USER, KEY_BASE];

/**
 * 会话切换/账号切换/退出时必须调用的清理：把旧用户留下的 bearer、base 与身份
 * 一次性抹掉（`clearSession` 只抹 token/user，服务器地址也会跨账号存活）。
 *
 * 返回值是"实际删掉了几个键"，让调用方（`lib/core-transport.ts` 的
 * `resetCoreSession`）能对被清掉的东西做出**可观测断言**，而不是只在注释里
 * 说"这里清了"。jsdom/浏览器之外的环境（无 localStorage）返回空数组。
 */
export function clearLegacySession(): string[] {
  if (typeof localStorage === "undefined") return [];
  const removed: string[] = [];
  for (const key of LEGACY_SESSION_KEYS) {
    if (localStorage.getItem(key) !== null) removed.push(key);
    localStorage.removeItem(key);
  }
  return removed;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly path: string,
    readonly code: ApiErrorCode = classify(status),
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** Coarse classes the UI branches on instead of guessing from numbers. */
export type ApiErrorCode =
  | "bad_request"
  | "unauthorized"
  | "forbidden"
  | "not_found"
  | "conflict"
  | "payload_too_large"
  | "rate_limited"
  | "server"
  | "network"
  | "timeout"
  | "cancelled"
  /** 域没有 Rust 提供者／核心 action 未注册：显式阻塞，绝不折成空数据或假成功。 */
  | "blocked";

function classify(status: number): ApiErrorCode {
  switch (status) {
    case 400: return "bad_request";
    case 401: return "unauthorized";
    case 403: return "forbidden";
    case 404: return "not_found";
    case 409: return "conflict";
    case 413: return "payload_too_large";
    case 429: return "rate_limited";
    default: return "server";
  }
}

function describe(status: number, body: unknown): string {
  if (body && typeof body === "object" && "error" in body) {
    return String((body as Json)["error"]);
  }
  if (typeof body === "string" && body.trim()) return body.trim().slice(0, 200);
  return `请求失败 (${status})`;
}

export interface RequestOptions {
  body?: unknown;
  /** Sent as `Idempotency-Key`; the server replays the first result on retry. */
  idempotencyKey?: string;
  /** Hard wall-clock cap; the abort reason becomes an `ApiError` with code `timeout`. */
  timeoutMs?: number;
  /** Caller-driven cancellation (user pressed cancel) — surfaces as `cancelled`. */
  signal?: AbortSignal;
  token?: string | null;
}

export interface Transport {
  (method: string, path: string, opts?: RequestOptions): Promise<unknown>;
}

function mergeSignals(external: AbortSignal | undefined, timeoutMs: number): AbortSignal {
  const controller = new AbortController();
  const timer = setTimeout(
    () => controller.abort(new DOMException("timeout", "TimeoutError")),
    timeoutMs,
  );
  const onExternalAbort = () => controller.abort(external?.reason);
  const clear = () => clearTimeout(timer);
  controller.signal.addEventListener("abort", clear, { once: true });
  if (external) {
    if (external.aborted) onExternalAbort();
    else external.addEventListener("abort", onExternalAbort, { once: true });
  }
  return controller.signal;
}

/**
 * 「没有核心可用」的统一说法。状态用 501：`features/commerce/api.ts` 的
 * `isUpstreamMissing()` 认 404/501，业务侧因此落到它本来就有的"上游未开通"横幅，
 * 而不是一句看起来像数据的空载荷。
 */
export function blockedError(path: string, detail = ""): ApiError {
  const why = "该域必须经认证 Rust 核心会话（#294），旧 HTTP 出口已停用";
  return new ApiError(501, `${why}${detail ? `：${detail}` : ""}`, path, "blocked");
}

/**
 * Default transport: browser fetch against the platform HTTP API.
 *
 * 这是**遗留出口**，只服务还没拿到 Rust 提供者的域。已迁移域（`isCoreOnlyPath`）
 * 在这里被无条件拒绝：不查 base、不读 bearer、不发 fetch。为什么把拒绝放在函数
 * 第一行而不是"装配好之后"——装配是组合根做的事，装配之前/失败之前也总有人调用
 * `request()`；把守门写在出口自己身上，"钱包偷偷又用 localStorage 会话发请求"这条
 * 路在结构上就不存在了（`lib/core-transport.test.ts` 与
 * `scripts/check-core-transport.mjs` 一边跑行为、一边跑静态）。
 */
export const fetchTransport: Transport = async (method, path, opts = {}) => {
  if (isCoreOnlyPath(path)) {
    throw blockedError(path, `${method} ${path}`);
  }
  const timeoutMs = opts.timeoutMs ?? 15_000;
  const signal = mergeSignals(opts.signal, timeoutMs);
  try {
    const tok = opts.token === undefined ? token() : opts.token;
    const res = await fetch(baseUrl() + path, {
      method,
      headers: {
        ...(opts.body !== undefined ? { "content-type": "application/json" } : {}),
        ...(tok ? { authorization: `Bearer ${tok}` } : {}),
        ...(opts.idempotencyKey ? { "idempotency-key": opts.idempotencyKey } : {}),
      },
      body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
      signal,
    });
    const text = await res.text();
    let body: unknown = null;
    if (text) {
      try { body = JSON.parse(text) as unknown; } catch { body = text; }
    }
    if (!res.ok) throw new ApiError(res.status, describe(res.status, body), path);
    return body;
  } catch (err) {
    if (err instanceof ApiError) throw err;
    if (err instanceof DOMException && err.name === "TimeoutError") {
      throw new ApiError(0, "请求超时", path, "timeout");
    }
    if (err instanceof DOMException && err.name === "AbortError") {
      throw new ApiError(0, "已取消", path, "cancelled");
    }
    throw new ApiError(0, `网络错误: ${err instanceof Error ? err.message : String(err)}`, path, "network");
  }
};

/**
 * Transport installed app-wide; features read it through their own module.
 *
 * 两层：`explicit` 只给测试/夹具用（优先级最高，`setTransport(null)` 让开），
 * `fallback` 是生产默认值 —— 默认就是遗留的 `fetchTransport`，组合根装上核心会话
 * 后由 `lib/core-transport.ts` 换成"按域路由"的那一份。已迁移域在两层里都出不去
 * （见 `fetchTransport` 开头的拒绝），所以"没装配成功"只会表现成阻塞，不会表现成
 * 拿旧 localStorage 会话发的请求。
 */
let explicit: Transport | null = null;
let fallback: Transport = fetchTransport;

export function setTransport(t: Transport | null): void {
  explicit = t;
}

/** 组合根专用：把默认出口换成"核心会话路由"。传 null 退回遗留出口。 */
export function setFallbackTransport(t: Transport | null): void {
  fallback = t ?? fetchTransport;
}

/** 当前默认出口是不是核心会话路由（供装配断言与守卫用例读）。 */
export function isCoreSessionInstalled(): boolean {
  return fallback !== fetchTransport;
}

export function transport(): Transport {
  return (method, path, opts) => (explicit ?? fallback)(method, path, opts);
}

/** JSON request through the installed transport. */
export async function request<T = unknown>(method: string, path: string, opts?: RequestOptions): Promise<T> {
  const t = explicit ?? fallback;
  return (await t(method, path, opts)) as T;
}

/** A fresh idempotency key: retries land on the same record, not a second one. */
export function idemKey(prefix: string): string {
  const uuid = typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random()}`;
  return `${prefix}-${uuid}`;
}

/** Human-readable line for banners; never leaks SQL or internals (server side already redacts). */
export function errorText(err: unknown): string {
  if (err instanceof ApiError) {
    switch (err.code) {
      case "unauthorized": return `登录已过期，请重新登录（${err.message}）`;
      case "forbidden": return `没有权限执行该操作（${err.message}）`;
      case "network": return "网络不可用，请检查连接后重试";
      case "timeout": return "请求超时，请重试";
      case "cancelled": return "已取消";
      case "blocked": return err.message;
      default: return err.message;
    }
  }
  return err instanceof Error ? err.message : String(err);
}
