/**
 * Browser session + HTTP transport for the web build.
 *
 * The desktop reaches the server through `crates/client`'s `Http`; this is the
 * browser-shaped equivalent of the same thing (base URL, bearer token, error
 * text). It carries no rules — every decision stays on the server.
 *
 * Error shapes differ by route family and both must be readable by the user:
 * `crates/platform` handlers return `{"error": "…"}`, while `server/src/lib.rs`
 * (history/send/recall/forward/search) and `crates/video` return **plain text**.
 */

const KEY_BASE = "wxlike-server";
const KEY_TOKEN = "wxlike-web-token";
const KEY_USER = "wxlike-web-user";

export type Json = Record<string, unknown>;

/** Same-origin by default: the server hosts the bundle at `/app` (A2). */
function defaultBase(): string {
  if (typeof location === "undefined") return "http://127.0.0.1:8099";
  return `${location.protocol}//${location.host}`;
}

export function baseUrl(): string {
  return (localStorage.getItem(KEY_BASE) ?? defaultBase()).replace(/\/+$/, "");
}

export function setBaseUrl(raw: string): void {
  const base = raw.trim().replace(/\/+$/, "");
  if (!/^https?:\/\/.+/.test(base)) throw new Error(`服务器地址无效: ${raw}`);
  // https pages must not downgrade their API calls to http
  if (location.protocol === "https:" && base.startsWith("http://")) {
    throw new Error("当前页面是 https，服务器地址也必须用 https");
  }
  localStorage.setItem(KEY_BASE, base);
}

export function token(): string | null {
  return localStorage.getItem(KEY_TOKEN);
}

export function userId(): string {
  const id = localStorage.getItem(KEY_USER);
  if (!id) throw new Error("未登录");
  return id;
}

/** Same, but empty instead of throwing: banners and reconnects run pre-login. */
export function userIdSafe(): string {
  return localStorage.getItem(KEY_USER) ?? "";
}

export function hasSession(): boolean {
  return Boolean(token() && localStorage.getItem(KEY_USER));
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

/** `/ws` shares the API origin; `connect_hub` still hands us the QUIC address, which a browser cannot use. */
export function wsUrl(): string {
  const base = baseUrl();
  return `${base.startsWith("https://") ? "wss" : "ws"}://${base.slice(
    base.indexOf("//") + 2,
  )}/ws`;
}

export class ApiError extends Error {
  constructor(readonly status: number, message: string, readonly path: string) {
    super(message);
    this.name = "ApiError";
  }
}

/**
 * A body is either JSON (`{"error":…}` / payload) or text (`text/plain` from
 * the server's own routes). Never `res.json()` blindly: two routes answer
 * `200` with an empty body.
 */
async function readBody(res: Response): Promise<unknown> {
  const text = await res.text();
  if (!text) return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
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
  /** Sent as `Idempotency-Key`; the server replays the first result on a retry. */
  idempotencyKey?: string;
  token?: string | null;
}

/** JSON request that must answer with JSON (or 2xx empty). */
export async function request<T = unknown>(
  method: string,
  path: string,
  opts: RequestOptions = {},
): Promise<T> {
  const tok = opts.token === undefined ? token() : opts.token;
  const res = await fetch(baseUrl() + path, {
    method,
    headers: {
      ...(opts.body !== undefined ? { "content-type": "application/json" } : {}),
      ...(tok ? { authorization: `Bearer ${tok}` } : {}),
      ...(opts.idempotencyKey ? { "idempotency-key": opts.idempotencyKey } : {}),
    },
    body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
  });
  const body = await readBody(res);
  if (!res.ok) throw new ApiError(res.status, describe(res.status, body), path);
  return body as T;
}

/** A write that answers `204` / no body. */
export async function requestEmpty(method: string, path: string): Promise<void> {
  await request(method, path);
}

/** POSTs a prepared form; the media/video endpoints reject unknown field names. */
export async function postForm<T = unknown>(path: string, form: FormData): Promise<T> {
  const tok = token();
  const res = await fetch(baseUrl() + path, {
    method: "POST",
    headers: { ...(tok ? { authorization: `Bearer ${tok}` } : {}) },
    body: form,
  });
  const body = await readBody(res);
  if (!res.ok) throw new ApiError(res.status, describe(res.status, body), path);
  return body as T;
}

/** Raw bytes out of a GET (media, video artifacts). */
export async function fetchBytes(path: string): Promise<Blob> {
  const res = await fetch(baseUrl() + path);
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new ApiError(res.status, text.trim() || `下载失败 (${res.status})`, path);
  }
  return await res.blob();
}

/**
 * TS 5.9's DOM types reject a `SharedArrayBuffer`-backed view as a `BlobPart` /
 * `BufferSource`. Everything here comes from `fetch`, `TextEncoder` or `crypto`
 * — always an `ArrayBuffer` — so the view is narrowed instead of copied.
 */
export function asView(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return bytes as Uint8Array<ArrayBuffer>;
}

/** `sha256` hex, so uploads can declare a digest the server verifies. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", asView(bytes));
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/** A fresh idempotency key: retries land on the same message, not a second one. */
export function idemKey(prefix: string): string {
  return `${prefix}-${crypto.randomUUID()}`;
}
