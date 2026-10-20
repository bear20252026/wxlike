/**
 * IndexedDB-backed local library and send queue for the web build — the
 * browser equivalent of the desktop's local SQLite (`ui/src-tauri`: `LocalStore`
 * + `wxlike_core::outbox`).
 *
 * Two jobs, both about surviving a reload or a dropped socket:
 *   * `messages` — the last `MSG_KEEP` rendered events per conversation, so
 *     `local_history` answers without the server;
 *   * `outbox` — envelopes that could not be sent. Retry/backoff mirrors
 *     `crates/core/src/outbox.rs` (5 attempts, doubling from 1 s, capped 30 s)
 *     because that queue's contract is the user-visible "待发送 / 发送失败".
 */

import type { Envelope, MsgBody, Receipt } from "./wire";
import { hex, hexToBytes, unhex } from "./wire";

const DB_NAME = "wxlike-web";
const DB_VERSION = 1;
const MESSAGES = "messages";
const OUTBOX = "outbox";

/** Rows kept per conversation (the desktop keeps everything; a browser tab should not). */
const MSG_KEEP = 200;

/** `wxlike_core::outbox::MAX_ATTEMPTS`. */
const MAX_ATTEMPTS = 5;
/** `backoff_ms`: 1s, 2s, 4s, 8s, then capped at 30s. */
function backoffMs(attempts: number): number {
  const raw = 1000 * 2 ** Math.max(0, attempts - 1);
  return Math.min(30_000, raw);
}

export interface StoredTask {
  task_id: string;
  conv_id: string;
  kind: string;
  /** The envelope in its JSON-safe form (see `envelopeToJson`). */
  payload: Json;
  attempts: number;
  next_retry_at: number;
  last_error: string | null;
  created_at: number;
  state: "pending" | "failed";
}

type Json = Record<string, unknown>;

let dbPromise: Promise<IDBDatabase> | null = null;
let opened = false;

export function localReady(): boolean {
  return opened;
}

/** Opens (or creates) the database. `local_open` in the command map calls this. */
export function localOpen(): Promise<void> {
  return openDb().then(() => undefined);
}

function openDb(): Promise<IDBDatabase> {
  dbPromise ??= new Promise<IDBDatabase>((resolve, reject) => {
    if (!("indexedDB" in globalThis)) {
      reject(new Error("浏览器不支持 IndexedDB"));
      return;
    }
    const req = indexedDB.open(DB_NAME, DB_VERSION);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(MESSAGES)) {
        const store = db.createObjectStore(MESSAGES, { keyPath: "key" });
        store.createIndex("by_conv", "conv_id");
      }
      if (!db.objectStoreNames.contains(OUTBOX)) {
        const store = db.createObjectStore(OUTBOX, { keyPath: "task_id" });
        store.createIndex("by_ready", "next_retry_at");
      }
    };
    req.onsuccess = () => {
      const db = req.result;
      db.onversionchange = () => db.close();
      opened = true;
      resolve(db);
    };
    req.onerror = () => reject(req.error ?? new Error("IndexedDB 打开失败"));
    req.onblocked = () => reject(new Error("IndexedDB 被其他标签页阻塞"));
  });
  return dbPromise;
}

function tx<T>(store: string, mode: IDBTransactionMode, run: (s: IDBObjectStore) => IDBRequest<T> | void): Promise<T> {
  return openDb().then(
    (db) =>
      new Promise<T>((resolve, reject) => {
        const t = db.transaction(store, mode);
        let result: T | undefined;
        const target = run(t.objectStore(store));
        if (target) {
          target.onsuccess = () => {
            result = target.result as T;
          };
          target.onerror = () => reject(target.error);
        }
        t.oncomplete = () => resolve(result as T);
        t.onerror = () => reject(t.error ?? new Error("IndexedDB 事务失败"));
        t.onabort = () => reject(t.error ?? new Error("IndexedDB 事务中止"));
      }),
  );
}

// ---------------------------------------------------------------------------
// message library
// ---------------------------------------------------------------------------

/**
 * Zero-padded seq keeps the primary key sortable: an index cursor walking
 * `prev` inside one conversation then yields newest-first, which is exactly
 * what `before_seq` paging expects.
 */
function messageKey(convId: string, seq: number, msgId: string): string {
  const safeSeq = Number.isFinite(seq) && seq >= 0 ? Math.floor(seq) : 0;
  return `${convId}\u0000${String(safeSeq).padStart(16, "0")}\u0000${msgId}`;
}

interface StoredRow extends Json {
  key: string;
  conv_id: string;
  seq: number;
}

/** Stores one `im-in` shaped event (the same payload the desktop appends). */
export async function localAppend(payload: Json): Promise<void> {
  const convId = String(payload["conv_id"] ?? "");
  if (!convId) return;
  const seq = Number(payload["seq"] ?? 0);
  const msgId = String(payload["msg_id"] ?? "");
  const row: StoredRow = { ...payload, conv_id: convId, seq, key: messageKey(convId, seq, msgId) };
  try {
    await tx(MESSAGES, "readwrite", (s) => s.put(row));
    await trim(convId);
  } catch (err) {
    // Losing the local copy must not lose the message: the server still has it.
    console.warn("[local] append failed:", String(err));
  }
}

async function trim(convId: string): Promise<void> {
  // count first: reading 200 rows back on every incoming message is pure waste
  const count = await tx<number>(MESSAGES, "readonly", (s) =>
    s.index("by_conv").count(IDBKeyRange.only(convId)),
  ).catch(() => 0);
  if (count <= MSG_KEEP) return;
  const rows = await tx<StoredRow[]>(MESSAGES, "readonly", (s) =>
    s.index("by_conv").getAll(IDBKeyRange.only(convId)),
  ).catch(() => [] as StoredRow[]);
  const excess = rows.slice(0, rows.length - MSG_KEEP); // key-ordered: oldest first
  await tx(MESSAGES, "readwrite", (s) => {
    for (const row of excess) s.delete(row.key);
    return undefined;
  });
}

export interface LocalPage {
  messages: Json[];
  next_before_seq: number | null;
}

/** One page, newest first. `beforeSeq = 0` means "start from the newest". */
export async function localPage(convId: string, beforeSeq: number, limit: number): Promise<LocalPage> {
  const rows = await tx<StoredRow[]>(MESSAGES, "readonly", (s) =>
    s.index("by_conv").getAll(IDBKeyRange.only(convId)),
  ).catch(() => [] as StoredRow[]);
  const wanted = rows
    .filter((r) => beforeSeq <= 0 || r.seq < beforeSeq)
    .sort((a, b) => b.seq - a.seq || b.key.localeCompare(a.key))
    .slice(0, limit);
  const next = wanted.length === limit ? wanted[wanted.length - 1].seq : null;
  return { messages: wanted.map(({ key: _key, ...rest }) => rest as Json), next_before_seq: next };
}

// ---------------------------------------------------------------------------
// outbox
// ---------------------------------------------------------------------------

/** JSON-safe form of an `Envelope`: byte fields become hex, body stays tagged. */
export function envelopeToJson(env: Envelope): Json {
  return {
    version: env.version,
    conv_id: env.conv_id,
    sender: env.sender,
    to: env.to,
    seq: env.seq,
    kind: env.kind,
    body: bodyToJson(env.body),
    sent_at_ms: env.sent_at_ms,
    msg_id: hex(env.msg_id),
    quote_of: env.quote_of ? hex(env.quote_of) : null,
    receipt: env.receipt
      ? env.receipt.t === "Delivered"
        ? { t: "Delivered" }
        : { t: "Read", up_to_msg_id: hex(env.receipt.up_to_msg_id) }
      : null,
  };
}

function bodyToJson(body: MsgBody): Json {
  switch (body.t) {
    case "Text":
      return { t: "Text", value: body.value };
    case "Notice":
      return { t: "Notice", value: body.value };
    case "Bytes":
      return { t: "Bytes", hex: hex(body.value) };
    case "Recall":
      return { t: "Recall", target: hex(body.target) };
    case "Custom":
      return { t: "Custom", kind: body.kind, payload: body.payload };
  }
}

export function envelopeFromJson(v: Json): Envelope {
  const body = (v["body"] ?? {}) as Json;
  const t = String(body["t"] ?? "");
  let parsed: MsgBody;
  switch (t) {
    case "Text":
      parsed = { t: "Text", value: String(body["value"] ?? "") };
      break;
    case "Notice":
      parsed = { t: "Notice", value: String(body["value"] ?? "") };
      break;
    case "Bytes":
      // a media reference is hex of arbitrary bytes, not a 16-byte MsgId
      parsed = { t: "Bytes", value: hexToBytes(String(body["hex"] ?? "")) };
      break;
    case "Recall":
      parsed = { t: "Recall", target: unhex(String(body["target"] ?? "")) };
      break;
    case "Custom":
      parsed = { t: "Custom", kind: String(body["kind"] ?? ""), payload: body["payload"] ?? {} };
      break;
    default:
      throw new Error(`outbox: unknown body tag "${t}"`);
  }
  const receiptRaw = v["receipt"] as Json | null;
  const receipt: Receipt | null = !receiptRaw
    ? null
    : receiptRaw["t"] === "Read"
      ? { t: "Read", up_to_msg_id: unhex(String(receiptRaw["up_to_msg_id"])) }
      : { t: "Delivered" };
  const msgId = String(v["msg_id"] ?? "");
  return {
    version: Number(v["version"] ?? 2),
    conv_id: String(v["conv_id"] ?? ""),
    sender: String(v["sender"] ?? ""),
    to: Array.isArray(v["to"]) ? (v["to"] as unknown[]).map(String) : [],
    seq: Number(v["seq"] ?? 0),
    kind: String(v["kind"] ?? "Text") as Envelope["kind"],
    body: parsed,
    sent_at_ms: Number(v["sent_at_ms"] ?? 0),
    msg_id: unhex(msgId),
    quote_of: v["quote_of"] ? unhex(String(v["quote_of"])) : null,
    receipt,
  };
}

/** Queues an envelope for retry. Same id twice is a no-op (`enqueue_with_id`). */
export async function outboxEnqueue(env: Envelope): Promise<void> {
  const taskId = hex(env.msg_id);
  const existing = await tx<StoredTask | undefined>(OUTBOX, "readonly", (s) =>
    s.get(taskId),
  ).catch(() => undefined);
  if (existing) return;
  const task: StoredTask = {
    task_id: taskId,
    conv_id: env.conv_id,
    kind: "envelope",
    payload: envelopeToJson(env),
    attempts: 0,
    next_retry_at: 0,
    last_error: null,
    created_at: Date.now(),
    state: "pending",
  };
  await tx(OUTBOX, "readwrite", (s) => s.put(task));
}

export async function outboxAll(): Promise<StoredTask[]> {
  return tx<StoredTask[]>(OUTBOX, "readonly", (s) => s.getAll()).catch(() => []);
}

/** Tasks the pump may attempt right now, oldest first. */
export async function outboxReady(now: number): Promise<StoredTask[]> {
  const tasks = await outboxAll();
  return tasks
    .filter((t) => t.state === "pending" && t.next_retry_at <= now)
    .sort((a, b) => a.next_retry_at - b.next_retry_at || a.created_at - b.created_at);
}

/** One failed attempt: back off, or mark the task dead after `MAX_ATTEMPTS`. */
export async function outboxFailed(taskId: string, error: string, now: number): Promise<void> {
  const task = await tx<StoredTask | undefined>(OUTBOX, "readonly", (s) => s.get(taskId)).catch(
    () => undefined,
  );
  if (!task) return;
  const attempts = task.attempts + 1;
  const dead = attempts >= MAX_ATTEMPTS;
  await tx(OUTBOX, "readwrite", (s) =>
    s.put({
      ...task,
      attempts,
      last_error: error,
      state: dead ? "failed" : "pending",
      next_retry_at: dead ? 0 : now + backoffMs(attempts),
    }),
  );
}

export async function outboxDone(taskId: string): Promise<void> {
  await tx(OUTBOX, "readwrite", (s) => s.delete(taskId));
}

/** "重发": zero the counter and let the pump try again. */
export async function outboxRetry(taskId: string): Promise<void> {
  const task = await tx<StoredTask | undefined>(OUTBOX, "readonly", (s) => s.get(taskId)).catch(
    () => undefined,
  );
  if (!task) return;
  await tx(OUTBOX, "readwrite", (s) =>
    s.put({ ...task, attempts: 0, state: "pending", next_retry_at: 0, last_error: null }),
  );
}

export async function outboxDrop(taskId: string): Promise<void> {
  await tx(OUTBOX, "readwrite", (s) => s.delete(taskId));
}

export interface OutboxStatus {
  pending: number;
  failed: Array<{
    task_id: string;
    conv_id: string;
    attempts: number;
    error: string | null;
    payload: unknown;
  }>;
}

/** Mirrors the desktop's `outbox_status` reply shape. */
export async function outboxStatus(): Promise<OutboxStatus> {
  const tasks = await outboxAll();
  return {
    pending: tasks.filter((t) => t.state === "pending").length,
    failed: tasks
      .filter((t) => t.state === "failed")
      .slice(0, 50)
      .map((t) => ({
        task_id: t.task_id,
        conv_id: t.conv_id,
        attempts: t.attempts,
        error: t.last_error,
        payload: t.payload,
      })),
  };
}

/** Rebuilds the queued envelope so the pump can resend it verbatim. */
export function envelopeFromTask(task: StoredTask): Envelope {
  return envelopeFromJson(task.payload);
}
