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
 */

const KEY_BASE = "wxlike-server";
const KEY_TOKEN = "wxlike-web-token";
const KEY_USER = "wxlike-web-user";

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
  | "cancelled";

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

/** Default transport: browser fetch against the platform HTTP API. */
export const fetchTransport: Transport = async (method, path, opts = {}) => {
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

/** Transport installed app-wide; features read it through their own module. */
let active: Transport = fetchTransport;

export function setTransport(t: Transport | null): void {
  active = t ?? fetchTransport;
}

export function transport(): Transport {
  return active;
}

/** JSON request through the installed transport. */
export async function request<T = unknown>(method: string, path: string, opts?: RequestOptions): Promise<T> {
  return (await active(method, path, opts)) as T;
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
      default: return err.message;
    }
  }
  return err instanceof Error ? err.message : String(err);
}
