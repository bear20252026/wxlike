/**
 * A1 — the browser implementation of the Tauri bridge.
 *
 * `ui/src/tauri.ts` reaches for `window.__TAURI__.core.invoke` / `.event.listen`.
 * Installing a same-shaped object here means `api.ts`, `ui.ts` and `main.ts` run
 * unmodified in a browser: only the transport changes, which is the whole point
 * of Zone A. Every command is answered by HTTP (`crates/platform`,
 * `server/src/lib.rs`) or by a bincode frame on `/ws` — never by a rule
 * re-implemented in TypeScript.
 *
 * Deliberately **not** implemented here: E2EE secret chats and the QUIC-based
 * RTC engine. Both live in Rust (`crates/core/src/e2ee.rs`, `crates/rtc`); a
 * browser cannot reuse them, and a JS stand-in would produce messages that look
 * fine and read as garbage. Those commands reject with a named reason instead.
 */

import { appStore, friendsAtom, viewAtom } from "../state";
import { commands, type WebCtx } from "./commands";
import { hub } from "./hub";
import {
  envelopeFromTask,
  localAppend,
  localOpen,
  outboxDone,
  outboxEnqueue,
  outboxFailed,
  outboxReady,
} from "./local";
import { showNotification } from "./notify";
import { envelopeToEvent, hex, makeEnvelope, type Envelope, type MsgBody, type WireFrame } from "./wire";
import * as session from "./session";

type Json = Record<string, unknown>;
type Handler = (payload: unknown) => void;

const listeners = new Map<string, Set<Handler>>();

/** Process-local sequence counter (`AppState::next_seq`); the hub assigns the real one. */
let seqCounter = 0;

function subscribe(event: string, handler: Handler): () => void {
  let set = listeners.get(event);
  if (!set) {
    set = new Set();
    listeners.set(event, set);
  }
  set.add(handler);
  return () => set?.delete(handler);
}

function emit(event: string, payload: unknown): void {
  for (const handler of listeners.get(event) ?? []) {
    try {
      handler(payload);
    } catch (err) {
      console.error(`[web] listener for ${event} threw:`, err);
    }
  }
}

// ---------------------------------------------------------------------------
// inbound
// ---------------------------------------------------------------------------

const BARE_KINDS: Record<string, string> = {
  image: "[图片]",
  "wallet.redpacket": "[红包]",
  "wallet.transfer": "[转账]",
  "msg.voice": "[语音]",
  "msg.file": "[文件]",
  "msg.video": "[视频]",
  "msg.sticker": "[贴纸]",
  "msg.forward": "[转发]",
  "msg.merge": "[合并转发]",
  e2ee: "[加密消息]",
};

function bannerText(payload: Json): string | null {
  const kind = String(payload["kind"] ?? "");
  // transient or self-originated traffic never belongs in a system banner
  if (kind === "typing" || kind === "receipt" || kind === "recall") return null;
  if (payload["sender"] === session.userIdSafe()) return null;
  const text = payload["text"];
  if (typeof text === "string" && text.trim()) return text.trim().slice(0, 120);
  return BARE_KINDS[kind] ?? "[新消息]";
}

function bannerTitle(env: Envelope): string {
  const friend = appStore.get(friendsAtom).find((f) => f.friend_id === env.sender);
  return friend?.remark || friend?.friend_id || env.sender;
}

/**
 * One inbound message: banner, persist, hand to the UI. Mirrors the desktop
 * pump (`start_recv_pump` → `render_event` → `local_append`).
 */
function deliverInbound(env: Envelope): void {
  const payload = envelopeToEvent(env);
  const view = appStore.get(viewAtom);
  const readingThisChat = view.screen === "chat" && view.convId === env.conv_id;
  const text = bannerText(payload);
  if (text !== null && (document.hidden || !readingThisChat)) {
    showNotification(bannerTitle(env), text);
  }
  void localAppend(payload);
  emit("im-in", payload);
}

/** Echo of my own send: the hub never hands a sender their own message back. */
function echoOwn(payload: Json): void {
  void localAppend(payload);
  emit("im-in", payload);
}

// ---------------------------------------------------------------------------
// sending
// ---------------------------------------------------------------------------

const ctx: WebCtx = {
  hub,
  emit,
  nextSeq(): number {
    return ++seqCounter;
  },

  async sendBody(convId, to, body, opts = {}) {
    const env = makeEnvelope(convId, session.userId(), to, ctx.nextSeq(), body, {
      quoteOf: opts.quoteOf ?? null,
      receipt: opts.receipt ?? null,
    });
    try {
      await hub.send({ t: "Msg", env });
    } catch (err) {
      // Connection trouble is not the user's fault: queue it (`queue_for_retry`)
      // so the command still resolves and the outbox pump can retry.
      await outboxEnqueue(env);
      console.warn(`[web] 发送已排队: ${String(err)}`);
      emit("im-outbox", { sent: 0, failed: 0, retrying: 1, pending: 1 });
      return env;
    }
    if (!opts.noEcho && !isTransient(body)) void ctx.echoFromServer(convId, hex(env.msg_id), env);
    return env;
  },

  async echoFromServer(convId, msgIdHex, fallback) {
    const row = await fetchRow(convId, msgIdHex);
    if (row) {
      echoOwn(row);
      return;
    }
    // The send did go out; only the authoritative seq is missing. Falling back
    // to the local envelope keeps the bubble on screen instead of losing it.
    console.warn(`[web] echo fallback for ${msgIdHex}: 服务端暂时查不到这条消息`);
    echoOwn(envelopeToEvent(fallback));
  },
};

/** `GET /api/conv/{c}/msg/{id}` — the row already is the event shape. */
async function fetchRow(convId: string, msgIdHex: string): Promise<Json | null> {
  const path = `/api/conv/${encodeURIComponent(convId)}/msg/${encodeURIComponent(msgIdHex)}`;
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      return normalizeRow(await session.request<Json>("GET", path));
    } catch (err) {
      if (err instanceof session.ApiError && err.status !== 404) {
        // 鉴权/授权出问题不是"还没落库"，说清楚：气泡照样渲染，原因留在控制台
        console.error(`[web] 取回自己的消息失败 (${err.status}): ${err.message}`);
        return null;
      }
      await new Promise((r) => setTimeout(r, 60));
    }
  }
  return null;
}

function normalizeRow(row: Json): Json {
  const out = { ...row };
  if (out["withdrawn"] === true) {
    out["kind"] = "system";
    out["text"] = "消息已撤回";
  }
  // keys the event payload does not carry
  delete out["withdrawn"];
  delete out["to"];
  return out;
}

function isTransient(body: MsgBody): boolean {
  return body.t === "Custom" && body.kind === "typing";
}

/** 1 s pump, same cadence as the desktop's `start_outbox_pump`. */
function startOutboxPump(): void {
  window.setInterval(async () => {
    if (!hub.online) return;
    const tasks = await outboxReady(Date.now());
    let sent = 0;
    let failed = 0;
    for (const task of tasks) {
      // 解不出来的任务也要有个结论（标记失败 → 用户能看到/删掉），
      // 不能让它永远停在 pending 里把队列卡死
      let env: Envelope;
      try {
        env = envelopeFromTask(task);
      } catch (err) {
        await outboxFailed(task.task_id, `队列记录无法解析: ${String(err)}`, Date.now());
        failed += 1;
        continue;
      }
      try {
        await hub.send({ t: "Msg", env });
        await outboxDone(task.task_id);
        sent += 1;
        void ctx.echoFromServer(task.conv_id, task.task_id, env);
      } catch (err) {
        await outboxFailed(task.task_id, String(err), Date.now());
        failed += 1;
      }
    }
    if (sent > 0 || failed > 0) emit("im-outbox", { sent, failed, retrying: 0, pending: 0 });
  }, 1_000);
}

// ---------------------------------------------------------------------------
// hub wiring
// ---------------------------------------------------------------------------

let hubWired = false;

function wireHub(): void {
  if (hubWired) return;
  hubWired = true;
  hub.onFrame((frame: WireFrame) => {
    if (frame.t === "Msg") deliverInbound(frame.env);
    // Hello/Ping frames carry nothing to render — and the protocol has no Pong.
  });
  let wasOnline = false;
  hub.onStatus((online) => {
    if (wasOnline && !online) emit("im-down", null);
    wasOnline = online;
  });
}

// ---------------------------------------------------------------------------
// bridge
// ---------------------------------------------------------------------------

async function invoke<T = unknown>(cmd: string, args?: Json): Promise<T> {
  const handler = commands[cmd];
  if (!handler) throw new Error(`命令「${cmd}」在网页端没有实现`);
  return (await handler(args ?? {}, ctx)) as T;
}

async function listen(
  event: string,
  handler: (e: { payload: unknown }) => void,
): Promise<() => void> {
  return subscribe(event, (payload) => handler({ payload }));
}

let installed = false;

/** Installs the bridge; must run before `ui/src/main.ts` is imported. */
export function installWebBridge(): void {
  if (installed) return;
  installed = true;
  wireHub();
  (window as unknown as Record<string, unknown>)["__TAURI__"] = {
    core: { invoke: (cmd: string, args?: Json) => invoke(cmd, args) },
    event: { listen },
  };
}

/**
 * Web defaults before the auth screen renders: the API origin is the page
 * origin (the server hosts the bundle itself — A2), the local library is
 * IndexedDB rather than a path on disk, and a reload keeps its bearer token.
 */
export async function prepareWeb(): Promise<void> {
  if (!localStorage.getItem("wxlike-server")) {
    localStorage.setItem("wxlike-server", session.baseUrl());
  }
  try {
    await localOpen();
  } catch (err) {
    console.warn("[web] IndexedDB unavailable:", String(err));
  }
  startOutboxPump();
  if (session.hasSession()) await resumeSession();
}

/** Brings the socket back after a reload, or drops a token the server rejects. */
async function resumeSession(): Promise<void> {
  try {
    await session.request("GET", "/api/me");
  } catch (err) {
    session.clearSession();
    console.warn("[web] 会话已失效，需要重新登录:", String(err));
    return;
  }
  try {
    await hub.connect(session.wsUrl(), session.token() ?? "");
  } catch (err) {
    console.warn("[web] 自动重连失败:", String(err));
  }
}
