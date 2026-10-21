/**
 * The command map: every `invoke` name in `ui/src/api.ts` → HTTP route or
 * `/ws` frame. This table is the contract between the UI and the server; the
 * server side of it is `crates/platform` + `server/src/lib.rs::history_routes`.
 *
 * Two send paths, and the split is the server's, not a style choice:
 *   * text and recall go over **HTTP** (`POST /api/conv/{c}/send`,
 *     `POST /api/conv/{c}/recall`) because those routes exist and they carry
 *     the idempotency key and the authorship check;
 *   * everything else (image / voice / file / sticker / video / typing /
 *     forward / merge-forward / read receipt) goes over **`/ws` as a bincode
 *     `Envelope`**, because no HTTP route accepts a non-text body —
 *     `SendBody` has exactly one content field (`server/src/lib.rs:569-575`).
 *
 * Anything that would need a rule here instead has one on the server: the
 * envelope is serialized, not decided.
 */

import type { Hub } from "./hub";
import { downloadMedia, downloadVideo, toBytes, uploadMedia, uploadVideo } from "./media";
import {
  localOpen,
  localPage,
  localReady,
  outboxDrop,
  outboxRetry,
  outboxStatus,
} from "./local";
import { requestNotifyPermission } from "./notify";
import * as session from "./session";
import { makeEnvelope, unhex, type Envelope, type MsgBody, type Receipt } from "./wire";

export interface SendOptions {
  /** Suppress the local echo (typing, receipts, recalls have no bubble). */
  noEcho?: boolean;
  quoteOf?: Uint8Array | null;
  receipt?: Receipt | null;
}

export interface WebCtx {
  hub: Hub;
  emit(event: string, payload: unknown): void;
  nextSeq(): number;
  /** Builds an envelope, sends it on `/ws`, queues it when the socket is down. */
  sendBody(
    convId: string,
    to: string[],
    body: MsgBody,
    opts?: SendOptions & { envelope?: Partial<Envelope> },
  ): Promise<Envelope>;
  /** Echoes a sent message from the server's own row (authoritative seq). */
  echoFromServer(convId: string, msgIdHex: string, fallback: Envelope): Promise<void>;
}

type Json = Record<string, unknown>;

// ---------------------------------------------------------------------------
// argument readers — a missing argument is a bug, so it says which one
// ---------------------------------------------------------------------------

function str(args: Json, key: string): string {
  const v = args[key];
  if (typeof v !== "string") throw new Error(`缺少参数 ${key}`);
  return v;
}

function optStr(args: Json, key: string): string | null {
  const v = args[key];
  if (v === undefined || v === null) return null;
  if (typeof v !== "string") throw new Error(`参数 ${key} 必须是字符串`);
  return v;
}

function num(args: Json, key: string): number {
  const v = args[key];
  if (typeof v !== "number" || !Number.isFinite(v)) throw new Error(`缺少参数 ${key}`);
  return v;
}

function strArray(args: Json, key: string): string[] {
  const v = args[key];
  if (v === undefined || v === null) return [];
  if (!Array.isArray(v)) throw new Error(`参数 ${key} 必须是数组`);
  return v.map((item) => String(item));
}

function obj(args: Json, key: string): Json {
  const v = args[key];
  if (!v || typeof v !== "object") throw new Error(`缺少参数 ${key}`);
  return v as Json;
}

const RTC_UNAVAILABLE =
  "浏览器端没有通话引擎：语音/视频走的是 Rust 的 QUIC 媒体栈（crates/rtc），" +
  "要接进浏览器需要 WebRTC 网关，属于 Zone A 之外的工程量";

const E2EE_UNAVAILABLE =
  "浏览器端没有端到端加密：X3DH/XDoubleRatchet 在 Rust 侧（crates/core/src/e2ee.rs），" +
  "按项目铁律不在 TypeScript 里重写密码学";

// ---------------------------------------------------------------------------
// the table
// ---------------------------------------------------------------------------

export const commands: Record<string, (args: Json, ctx: WebCtx) => Promise<unknown>> = {
  // ---- session / auth ------------------------------------------------------

  async set_server(args) {
    // 之后 `connect_hub` 会用新 origin 重开 socket（Hub.connect 比对 url/用户）
    session.setBaseUrl(str(args, "base_url"));
    return null;
  },

  async register(args) {
    return await authenticate("POST", "/api/register", args);
  },

  async login(args) {
    return await authenticate("POST", "/api/login", args);
  },

  async me() {
    return await session.request<Json>("GET", "/api/me");
  },

  async update_profile(args) {
    const body: Json = {};
    for (const key of ["name", "avatar", "signature"] as const) {
      const value = optStr(args, key);
      if (value !== null) body[key] = value;
    }
    return await session.request<Json>("PUT", "/api/profile", { body });
  },

  // ---- realtime ------------------------------------------------------------

  /**
   * `connect_hub` hands over the QUIC address from the auth screen. A browser
   * cannot use it, so the socket targets `/ws` on the API origin instead and
   * the hub registers this device with `Hello{user_id}`.
   */
  async connect_hub(_args, ctx) {
    await ctx.hub.connect(session.wsUrl(), session.token() ?? "");
    return `ws ${session.wsUrl()}`;
  },

  /** The desktop starts its pump here; in the browser the socket *is* the pump. */
  async start_recv_pump() {
    return null;
  },

  // ---- friends / groups ----------------------------------------------------

  async friends() {
    return await session.request<Json[]>("GET", "/api/friends");
  },

  async friend_requests_incoming() {
    return await session.request<Json[]>("GET", "/api/friends/requests/incoming");
  },

  async friend_accept(args) {
    return await session.request(
      "POST",
      `/api/friends/requests/${encodeURIComponent(str(args, "req_id"))}/accept`,
    );
  },

  async friend_decline(args) {
    return await session.request(
      "POST",
      `/api/friends/requests/${encodeURIComponent(str(args, "req_id"))}/decline`,
    );
  },

  async friend_request(args) {
    return await session.request("POST", "/api/friends/requests", {
      body: { to_user_id: str(args, "to_user_id"), message: optStr(args, "message") },
    });
  },

  async groups_create(args) {
    const key = session.idemKey("group");
    const reply = await session.request<Json>("POST", "/api/groups", {
      body: { name: str(args, "name"), member_ids: strArray(args, "member_ids") },
      idempotencyKey: key,
    });
    return String(reply["group_id"] ?? "");
  },

  // ---- wallet --------------------------------------------------------------

  async wallet_balance() {
    const reply = await session.request<Json>("GET", "/api/wallet/balance");
    return Number(reply["balance"] ?? 0);
  },

  async wallet_redpacket_cmd(args) {
    return await session.request<Json>("POST", "/api/wallet/redpacket", {
      body: { amount: num(args, "amount"), count: num(args, "count"), memo: optStr(args, "memo") },
    });
  },

  async wallet_claim(args) {
    const reply = await session.request<Json>(
      "POST",
      `/api/wallet/redpacket/${encodeURIComponent(str(args, "redpacket_id"))}/claim`,
    );
    return Number(reply["amount"] ?? 0);
  },

  // ---- media ---------------------------------------------------------------

  async upload_media(args) {
    return await uploadMedia(
      toBytes(args["bytes"]),
      optStr(args, "content_type") ?? "application/octet-stream",
      optStr(args, "sha256"),
    );
  },

  async download_media(args) {
    return await downloadMedia(str(args, "media_id"), args["thumb"] === true);
  },

  async upload_video(args) {
    return await uploadVideo(toBytes(args["bytes"]));
  },

  async download_video(args) {
    return await downloadVideo(str(args, "path"));
  },

  /** Avatar = upload media, then point the profile at the id (same as the shell). */
  async upload_avatar(args) {
    const up = await uploadMedia(
      toBytes(args["bytes"]),
      optStr(args, "content_type") ?? "image/png",
      null,
    );
    const mediaId = up.media_id;
    await session.request("PUT", "/api/profile", { body: { avatar: mediaId } });
    return mediaId;
  },

  // ---- sending -------------------------------------------------------------

  /** Text goes over HTTP: the route exists and turns `idempotency_key` into a stable msg_id. */
  async send_text(args, ctx) {
    const convId = str(args, "conv_id");
    const text = str(args, "text");
    const key = session.idemKey("send");
    const reply = await session.request<Json>("POST", `/api/conv/${encodeURIComponent(convId)}/send`, {
      body: { text, idempotency_key: key },
      idempotencyKey: key,
    });
    const msgIdHex = String(reply["msg_id"] ?? "");
    if (msgIdHex) {
      // fallback keeps the server's id, so a failed echo lookup still renders
      const fallback = makeEnvelope(convId, session.userId(), strArray(args, "to"), ctx.nextSeq(), {
        t: "Text",
        value: text,
      }, { msgId: unhex(msgIdHex) });
      void ctx.echoFromServer(convId, msgIdHex, fallback);
    }
    return reply;
  },

  async send_image(args, ctx) {
    // the reference, never the bytes (`ui/src-tauri`: `send_image`)
    await ctx.sendBody(str(args, "conv_id"), strArray(args, "to"), {
      t: "Bytes",
      value: new TextEncoder().encode(str(args, "media_id")),
    });
    return null;
  },

  async send_typing(args, ctx) {
    await ctx.sendBody(str(args, "conv_id"), strArray(args, "to"), {
      t: "Custom",
      kind: "typing",
      payload: { conv_id: str(args, "conv_id") },
    }, { noEcho: true });
    return null;
  },

  /** Recall has its own route — it checks authorship before submitting. */
  async recall_msg(args) {
    return await session.request("POST", `/api/conv/${encodeURIComponent(str(args, "conv_id"))}/recall`, {
      body: { msg_id: str(args, "target") },
    });
  },

  /**
   * Read receipt: an envelope whose body is an empty notice, `quote_of` and
   * `receipt.Read` pointing at the message, addressed to nobody (conversation
   * broadcast) — exactly `AppState::send_read_receipt`.
   */
  async send_read_receipt(args, ctx) {
    const upTo = unhex(str(args, "up_to"));
    await ctx.sendBody(str(args, "conv_id"), [], { t: "Notice", value: "" }, {
      noEcho: true,
      quoteOf: upTo,
      receipt: { t: "Read", up_to_msg_id: upTo },
    });
    return null;
  },

  async forward_message(args, ctx) {
    await ctx.sendBody(str(args, "to_conv"), strArray(args, "to"), {
      t: "Custom",
      kind: "msg.forward",
      payload: obj(args, "item"),
    });
    return null;
  },

  async merge_forward(args, ctx) {
    const items = Array.isArray(args["items"]) ? (args["items"] as unknown[]) : [];
    if (items.length === 0) throw new Error("没有选中要合并转发的消息");
    await ctx.sendBody(str(args, "to_conv"), strArray(args, "to"), {
      t: "Custom",
      kind: "msg.merge",
      payload: { title: `${items.length} 条聊天记录`, items },
    });
    return null;
  },

  async send_voice(args, ctx) {
    await ctx.sendBody(str(args, "conv_id"), strArray(args, "to"), {
      t: "Custom",
      kind: "msg.voice",
      payload: {
        media_id: str(args, "media_id"),
        duration_ms: num(args, "duration_ms"),
        waveform: Array.isArray(args["waveform"]) ? (args["waveform"] as number[]) : [],
        asr: null,
      },
    });
    return null;
  },

  async send_file(args, ctx) {
    await ctx.sendBody(str(args, "conv_id"), strArray(args, "to"), {
      t: "Custom",
      kind: "msg.file",
      payload: {
        media_id: str(args, "media_id"),
        name: str(args, "name"),
        size: num(args, "size"),
        sha256: optStr(args, "sha256") ?? "",
      },
    });
    return null;
  },

  async send_sticker(args, ctx) {
    await ctx.sendBody(str(args, "conv_id"), strArray(args, "to"), {
      t: "Custom",
      kind: "msg.sticker",
      payload: { media_id: str(args, "media_id"), pack: str(args, "pack"), id: str(args, "id") },
    });
    return null;
  },

  async send_video(args, ctx) {
    await ctx.sendBody(str(args, "conv_id"), strArray(args, "to"), {
      t: "Custom",
      kind: "msg.video",
      payload: obj(args, "payload"),
    });
    return null;
  },

  async send_redpacket_msg(args, ctx) {
    await ctx.sendBody(str(args, "conv_id"), strArray(args, "to"), {
      t: "Custom",
      kind: "wallet.redpacket",
      payload: {
        redpacket_id: str(args, "redpacket_id"),
        amount: num(args, "amount"),
        count: num(args, "count"),
        memo: optStr(args, "memo") ?? "",
      },
    });
    return null;
  },

  // ---- history / unread / search ------------------------------------------

  async load_history(args) {
    const conv = encodeURIComponent(str(args, "conv_id"));
    const before = Number(args["before_seq"] ?? 0);
    const limit = Number(args["limit"] ?? 30);
    return await session.request<Json>("GET", `/api/history/${conv}?before_seq=${before}&limit=${limit}`);
  },

  async conv_unread(args) {
    return await session.request<Json>(
      "GET",
      `/api/conv/${encodeURIComponent(str(args, "conv"))}/unread`,
    );
  },

  async conv_mark_read(args) {
    return await session.request(
      "POST",
      `/api/conv/${encodeURIComponent(str(args, "conv"))}/read`,
      { body: {} },
    );
  },

  async search_messages(args) {
    const q = encodeURIComponent(str(args, "q"));
    const limit = Number(args["limit"] ?? 20);
    return await session.request<Json>("GET", `/api/search/messages?q=${q}&limit=${limit}`);
  },

  // ---- favorites -----------------------------------------------------------

  /**
   * `api.ts`'s snapshot uses `origin_conv` / `origin_msg_id`; the platform's
   * `FavoriteInput` requires `source_conv` / `source_msg_id` (no default), so
   * the keys are renamed here. The desktop posts the snapshot verbatim and
   * gets a 422 — reported as a separate bug.
   */
  async favorite_add(args) {
    const item = obj(args, "item");
    return await session.request<Json>("POST", "/api/favorites", {
      body: {
        source_conv: String(item["origin_conv"] ?? item["source_conv"] ?? ""),
        source_msg_id: String(item["origin_msg_id"] ?? item["source_msg_id"] ?? ""),
        kind: String(item["kind"] ?? "text"),
        text: String(item["text"] ?? ""),
        media_id: String(item["media_id"] ?? ""),
        sender: String(item["sender"] ?? ""),
        sent_at_ms: Number(item["sent_at_ms"] ?? 0),
      },
    });
  },

  async favorites_list(args) {
    const limit = Number(args["limit"] ?? 20);
    return await session.request<Json>("GET", `/api/favorites?limit=${limit}`);
  },

  async favorite_remove(args) {
    return await session.request(
      "DELETE",
      `/api/favorites/${encodeURIComponent(str(args, "id"))}`,
    );
  },

  // ---- devices / sessions --------------------------------------------------

  async sessions_list() {
    return await session.request<Json>("GET", "/api/sessions");
  },

  async session_revoke(args) {
    return await session.request(
      "POST",
      `/api/sessions/${encodeURIComponent(str(args, "sid"))}/revoke`,
    );
  },

  async sessions_revoke_others() {
    return await session.request<Json>("POST", "/api/sessions/revoke-others");
  },

  // ---- local library + outbox (IndexedDB) ---------------------------------

  async local_open() {
    await localOpen();
    return null;
  },

  async local_autopen() {
    await localOpen();
    return null;
  },

  async local_ready() {
    return localReady();
  },

  async local_history(args) {
    return await localPage(
      str(args, "conv_id"),
      Number(args["before_seq"] ?? 0),
      Number(args["limit"] ?? 50),
    );
  },

  async outbox_status() {
    return await outboxStatus();
  },

  async outbox_retry(args) {
    await outboxRetry(str(args, "task_id"));
    return null;
  },

  async outbox_drop(args) {
    await outboxDrop(str(args, "task_id"));
    return null;
  },

  // ---- secret chat (Rust-only, refused rather than faked) ------------------

  async secret_enable() {
    throw new Error(E2EE_UNAVAILABLE);
  },

  /** False = *this device* has no E2EE state; the conversation may be secret elsewhere. */
  async secret_enabled() {
    return false;
  },

  async send_secret_text() {
    throw new Error(E2EE_UNAVAILABLE);
  },

  async safety_number(args) {
    return await session.request<Json>(
      "GET",
      `/api/secret/${encodeURIComponent(str(args, "peer"))}/safety-number`,
    );
  },

  async verify_peer(args) {
    return await session.request(
      "PUT",
      `/api/secret/${encodeURIComponent(str(args, "peer"))}/verify`,
      { body: { peer_identity: str(args, "peer_identity") } },
    );
  },

  // ---- call control plane (room state is HTTP; media is not) ---------------

  async call_state(args) {
    return await session.request<Json>("GET", `/api/call/${encodeURIComponent(str(args, "conv"))}`);
  },

  async call_action(args) {
    return await callAction(args);
  },

  async call_rtc_start() {
    throw new Error(RTC_UNAVAILABLE);
  },

  /**
   * 信令的 HTTP 路由确实存在（`/api/call/{conv}/signal`），但载荷是 Rust RTC
   * 引擎产生的 `Signal`（v/call_id/from/to/content）。浏览器里没有这个引擎，
   * 拼一个空壳发出去只会污染房间队列，所以和媒体命令一起拒绝。
   */
  async call_signal_post() {
    throw new Error(RTC_UNAVAILABLE);
  },

  async call_signal_take() {
    throw new Error(RTC_UNAVAILABLE);
  },

  async call_rtc_accept() {
    throw new Error(RTC_UNAVAILABLE);
  },

  async call_rtc_hangup() {
    throw new Error(RTC_UNAVAILABLE);
  },

  /** Truthful "no call on this device" — the browser has no local RTC session. */
  async call_rtc_status() {
    return {
      active: false,
      conv: "",
      call_id: "",
      ringing: false,
      connected: false,
      phase: "浏览器端无通话引擎",
      audio_frames: 0,
      mic_frames: 0,
      peers: 0,
      connected_peers: 0,
    };
  },

  // ---- diagnostics ---------------------------------------------------------

  /**
   * `net_diag` walks DNS → TCP → HTTP → QUIC on the desktop. A browser cannot
   * resolve or open raw sockets, so those steps are reported as `skipped`
   * rather than as success — only the HTTP leg is actually measured.
   */
  async net_diag() {
    const base = session.baseUrl();
    const host = base.replace(/^https?:\/\//, "");
    const steps = [
      step("dns", 0, "skipped", "浏览器无法查询 DNS"),
      step("tcp", 0, "skipped", "浏览器无法建立原始 TCP 连接"),
    ];
    const started = performance.now();
    let httpStatus: { status: "ok" | "failed" | "skipped"; detail: string };
    try {
      const res = await fetch(`${base}/health`);
      const ms = Math.round(performance.now() - started);
      httpStatus = res.ok
        ? { status: "ok", detail: `HTTP ${res.status} · ${ms}ms` }
        : { status: "failed", detail: `HTTP ${res.status}` };
    } catch (err) {
      httpStatus = { status: "failed", detail: String(err) };
    }
    steps.push(step("http", Math.round(performance.now() - started), httpStatus.status, httpStatus.detail));
    steps.push(step("quic", 0, "skipped", "浏览器没有 QUIC 栈，实时通道走 /ws"));
    const ok = httpStatus.status === "ok";
    return {
      host,
      http_port: portOf(base, 8099),
      quic_port: 0,
      steps,
      summary: ok
        ? `HTTP 可用（${httpStatus.detail}）；QUIC/DNS/TCP 由浏览器代持，未检测`
        : `HTTP 不通：${httpStatus.detail}`,
    };
  },
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

async function authenticate(method: string, path: string, args: Json): Promise<Json> {
  const reply = await session.request<Json>(method, path, {
    body: { name: str(args, "name"), password: str(args, "password") },
    token: null,
  });
  session.saveSession(reply);
  // the auth-screen click is the user gesture a permission prompt needs
  await requestNotifyPermission();
  return reply;
}

function step(
  name: string,
  elapsed: number,
  status: "ok" | "failed" | "skipped",
  detail: string,
): Json {
  return { step: name, elapsed_ms: elapsed, status: { status, detail } };
}

function portOf(base: string, fallback: number): number {
  const match = /:(\d+)/.exec(base.replace(/^https?:\/\//, "").split("/")[0] ?? "");
  if (match) return Number(match[1]);
  return base.startsWith("https://") ? 443 : fallback;
}

/** action → room route. `value` becomes `muted` where the route asks for it. */
const CALL_ROUTES: Record<string, { method: "GET" | "POST"; suffix: string; body?: (a: Json) => Json }> = {
  start: { method: "POST", suffix: "/start", body: (a) => ({ capacity: a["capacity"] ?? null }) },
  join: { method: "POST", suffix: "/join" },
  leave: { method: "POST", suffix: "/leave" },
  // the room's mute route takes `muted`; the UI's action carries `value`
  mute: {
    method: "POST",
    suffix: "/mute",
    body: (a) => ({ target: a["target"] ?? "", track: a["track"] ?? "audio", muted: a["value"] === true }),
  },
  "mute-all": {
    method: "POST",
    suffix: "/mute-all",
    body: (a) => ({ target: "", track: a["track"] ?? "audio", muted: a["value"] === true }),
  },
  kick: { method: "POST", suffix: "/kick", body: (a) => ({ target: a["target"] ?? "" }) },
  lock: { method: "POST", suffix: "/lock", body: (a) => ({ value: a["value"] === true }) },
  record: { method: "POST", suffix: "/record", body: (a) => ({ value: a["value"] === true }) },
  hand: { method: "POST", suffix: "/hand", body: (a) => ({ value: a["value"] === true }) },
  transfer: { method: "POST", suffix: "/transfer", body: (a) => ({ target: a["target"] ?? "" }) },
  egress: { method: "GET", suffix: "/egress" },
};

async function callAction(args: Json): Promise<Json> {
  const conv = encodeURIComponent(str(args, "conv"));
  const action = str(args, "action");
  const route = CALL_ROUTES[action];
  if (!route) throw new Error(`通话动作「${action}」不存在`);
  const reply = await session.request<Json | null>(route.method, `/api/call/${conv}${route.suffix}`, {
    body: route.body ? route.body(args) : undefined,
  });
  return (reply ?? {}) as Json;
}
