/**
 * bincode codec for the hub wire protocol (`WireFrame` + `Envelope`).
 *
 * Browser has no QUIC, so it speaks `/ws` — and `/ws` is **binary only**: the
 * server has exactly one wire format, bincode 1.3 `WireFrame` (`server/src/lib.rs::ws_routes`,
 * `crates/net/src/wire.rs`). This module is the JS half of `crates/core/src/types.rs`;
 * it mirrors that struct field-for-field, so a schema drift shows up here as a
 * decode error rather than as a silently wrong message.
 *
 * bincode 1.3 layout (`bincode::serialize` = fixint + little-endian):
 *   u8 1 byte · u32 4 bytes LE · u64 8 bytes LE · enum variant index u32 LE ·
 *   string / bytes / Vec length **u64 LE** · `Option<T>` one tag byte (0/1) ·
 *   struct fields and `[u8; 16]` positional with no length prefix.
 *
 * Nothing here is a business rule: it is positional serialization only. Sorting,
 * unread counts and visibility stay on the server (`crates/message`, `crates/net`).
 */

/// `wxlike_core::types::PROTOCOL_VERSION`.
export const PROTOCOL_VERSION = 2;

/// `MsgId` is `[u8; 16]` — written positional, no length prefix.
export const MSG_ID_LEN = 16;

const MAX_STR_BYTES = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Envelope mirror of `crates/core/src/types.rs`
// ---------------------------------------------------------------------------

/** `MsgKind`: serialized as its own field, but always derived from the body. */
export type MsgKind = "Text" | "Blob" | "System";

/** `MsgBody` — variant order is part of the protocol (indices 0..=4). */
export type MsgBody =
  | { readonly t: "Text"; readonly value: string }
  | { readonly t: "Bytes"; readonly value: Uint8Array }
  | { readonly t: "Notice"; readonly value: string }
  | { readonly t: "Recall"; readonly target: Uint8Array }
  | { readonly t: "Custom"; readonly kind: string; readonly payload: unknown };

/** `Receipt`. */
export type Receipt =
  | { readonly t: "Delivered" }
  | { readonly t: "Read"; readonly up_to_msg_id: Uint8Array };

export interface Envelope {
  readonly version: number;
  readonly conv_id: string;
  readonly sender: string;
  readonly to: string[];
  readonly seq: number;
  readonly kind: MsgKind;
  readonly body: MsgBody;
  readonly sent_at_ms: number;
  readonly msg_id: Uint8Array;
  readonly quote_of: Uint8Array | null;
  readonly receipt: Receipt | null;
}

/**
 * `WireFrame` (`crates/net/src/wire.rs`) — the server has no `Pong` variant.
 *
 * **变体序号必须与 Rust 的声明顺序一致**（bincode 写的是 index）：
 * 0 Hello / 1 HelloAck / 2 HelloReject / 3 Msg / 4 Ping。
 *
 * `Hello` 只带 `token`，不带 `user_id`：身份由服务端解析（issue #3）。
 * 客户端从 `HelloAck` 得知自己是谁。
 */
export type WireFrame =
  | { readonly t: "Hello"; readonly token: string }
  | { readonly t: "HelloAck"; readonly user_id: string; readonly session_id: string }
  | { readonly t: "HelloReject"; readonly reason: string }
  | { readonly t: "Msg"; readonly env: Envelope }
  | { readonly t: "Ping" };

const MSG_KIND: Record<MsgKind, number> = { Text: 0, Blob: 1, System: 2 };
const BODY_INDEX: Record<MsgBody["t"], number> = {
  Text: 0,
  Bytes: 1,
  Notice: 2,
  Recall: 3,
  Custom: 4,
};
const KIND_BY_BODY: Record<MsgBody["t"], MsgKind> = {
  Text: "Text",
  Bytes: "Blob",
  Notice: "System",
  Recall: "Blob",
  Custom: "System",
};

/**
 * `crates_core::types::kind_of` — "`kind` stays derivable from the body so the
 * two cannot disagree". Exposed so callers build envelopes without guessing.
 */
export function kindOf(body: MsgBody): MsgKind {
  return KIND_BY_BODY[body.t];
}

/** `Envelope::new_msg_id()`: 16 random bytes. */
export function newMsgId(): Uint8Array {
  const id = new Uint8Array(MSG_ID_LEN);
  crypto.getRandomValues(id);
  return id;
}

/** `Envelope::with_body`, minus the parts the hub fills in. */
export function makeEnvelope(
  convId: string,
  sender: string,
  to: string[],
  seq: number,
  body: MsgBody,
  opts: { msgId?: Uint8Array; quoteOf?: Uint8Array | null; receipt?: Receipt | null } = {},
): Envelope {
  const msgId = opts.msgId ?? newMsgId();
  if (msgId.length !== MSG_ID_LEN) throw new Error(`msg_id must be ${MSG_ID_LEN} bytes`);
  return {
    version: PROTOCOL_VERSION,
    conv_id: convId,
    sender,
    to,
    seq,
    kind: kindOf(body),
    body,
    sent_at_ms: Date.now(),
    msg_id: msgId,
    quote_of: opts.quoteOf ?? null,
    receipt: opts.receipt ?? null,
  };
}

// ---------------------------------------------------------------------------
// writer / reader primitives
// ---------------------------------------------------------------------------

class BinWriter {
  private readonly view: DataView;
  private off = 0;

  private constructor(readonly buf: ArrayBuffer, view: DataView) {
    this.view = view;
  }

  static withCapacity(bytes: number): BinWriter {
    const buf = new ArrayBuffer(bytes);
    return new BinWriter(buf, new DataView(buf));
  }

  /** Size of `value` once encoded — used to size the buffer exactly. */
  static sizeOfFrame(frame: WireFrame): number {
    let n = 4; // variant index
    if (frame.t === "Hello") return n + strBytes(frame.token);
    if (frame.t === "HelloAck") return n + strBytes(frame.user_id) + strBytes(frame.session_id);
    if (frame.t === "HelloReject") return n + strBytes(frame.reason);
    if (frame.t === "Ping") return n;
    const e = frame.env;
    n += 1; // version
    n += strBytes(e.conv_id);
    n += strBytes(e.sender);
    n += 8 + e.to.reduce((acc, s) => acc + strBytes(s), 0); // Vec<String>
    n += 8; // seq
    n += 4; // kind index
    n += 4; // body index
    switch (e.body.t) {
      case "Text":
      case "Notice":
        n += strBytes(e.body.value);
        break;
      case "Bytes":
        n += 8 + e.body.value.length;
        break;
      case "Recall":
        n += MSG_ID_LEN;
        break;
      case "Custom":
        n += strBytes(e.body.kind) + strBytes(jsonText(e.body.payload));
        break;
    }
    n += 8; // sent_at_ms
    n += MSG_ID_LEN;
    n += 1 + (e.quote_of ? MSG_ID_LEN : 0);
    // Option tag, then the Receipt's own u32 variant index + its fields
    n += 1 + (e.receipt ? 4 + (e.receipt.t === "Read" ? MSG_ID_LEN : 0) : 0);
    return n;
  }

  u8(v: number): void {
    this.view.setUint8(this.off, v);
    this.off += 1;
  }

  u32(v: number): void {
    this.view.setUint32(this.off, v, true);
    this.off += 4;
  }

  u64(v: number | bigint): void {
    this.view.setBigUint64(this.off, BigInt(v), true);
    this.off += 8;
  }

  len(v: number): void {
    this.u64(BigInt(v));
  }

  raw(b: Uint8Array): void {
    new Uint8Array(this.buf, this.off, b.length).set(b);
    this.off += b.length;
  }

  str(s: string): void {
    const bytes = new TextEncoder().encode(s);
    this.len(bytes.length);
    this.raw(bytes);
  }

  fixed(b: Uint8Array, name: string): void {
    if (b.length !== MSG_ID_LEN) throw new Error(`${name} must be ${MSG_ID_LEN} bytes`);
    this.raw(b);
  }

  optional(b: Uint8Array | null): void {
    this.u8(b ? 1 : 0);
    if (b) this.fixed(b, "optional msg id");
  }

  done(): Uint8Array {
    if (this.off !== this.buf.byteLength) {
      // sizing bug: never ship a frame with padding, the server would misread it
      throw new Error(`encoder reserved ${this.buf.byteLength} bytes, wrote ${this.off}`);
    }
    return new Uint8Array(this.buf);
  }
}

class BinReader {
  private off = 0;
  private readonly view: DataView;

  constructor(readonly bytes: Uint8Array) {
    if (typeof DataView === "undefined") throw new Error("DataView unavailable");
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  private take(n: number): number {
    if (this.off + n > this.bytes.length) {
      throw new Error(`bincode: truncated frame (need ${n} more bytes at ${this.off})`);
    }
    const at = this.off;
    this.off += n;
    return at;
  }

  u8(): number {
    return this.view.getUint8(this.take(1));
  }

  u32(): number {
    return this.view.getUint32(this.take(4), true);
  }

  /**
   * `u64`. Hub seqs and millisecond timestamps stay far below 2^53, so a Number
   * is exact for everything the protocol actually carries here; anything larger
   * is refused rather than rounded.
   */
  u64(): number {
    const at = this.take(8);
    const big = this.view.getBigUint64(at, true);
    if (big > 9007199254740991n) {
      throw new Error(`bincode: u64 ${big} exceeds safe integer range`);
    }
    return Number(big);
  }

  len(): number {
    return this.u64();
  }

  raw(n: number): Uint8Array {
    return this.bytes.subarray(this.take(n), this.off);
  }

  str(): string {
    const n = this.len();
    if (n > MAX_STR_BYTES) throw new Error(`bincode: string length ${n} implausible`);
    return new TextDecoder("utf-8", { fatal: false }).decode(this.raw(n));
  }

  bytesVec(): Uint8Array {
    const n = this.len();
    if (n > MAX_STR_BYTES) throw new Error(`bincode: bytes length ${n} implausible`);
    return this.raw(n);
  }

  fixed(): Uint8Array {
    return this.raw(MSG_ID_LEN);
  }

  optional(): Uint8Array | null {
    return this.u8() === 1 ? this.fixed() : null;
  }

  eof(): boolean {
    return this.off >= this.bytes.length;
  }

  leftover(): number {
    return this.bytes.length - this.off;
  }
}

function strBytes(s: string): number {
  return 8 + new TextEncoder().encode(s).length;
}

/**
 * `json_bytes` in `types.rs`: bincode is not self-describing, so a
 * `serde_json::Value` payload rides as its JSON **text**.
 */
function jsonText(payload: unknown): string {
  return JSON.stringify(payload ?? {});
}

// ---------------------------------------------------------------------------
// encode
// ---------------------------------------------------------------------------

export function encodeFrame(frame: WireFrame): Uint8Array {
  const w = BinWriter.withCapacity(BinWriter.sizeOfFrame(frame));
  writeFrame(w, frame);
  return w.done();
}

function writeFrame(w: BinWriter, frame: WireFrame): void {
  switch (frame.t) {
    case "Hello":
      w.u32(0);
      w.str(frame.token);
      return;
    case "HelloAck":
      w.u32(1);
      w.str(frame.user_id);
      w.str(frame.session_id);
      return;
    case "HelloReject":
      w.u32(2);
      w.str(frame.reason);
      return;
    case "Msg":
      w.u32(3);
      writeEnvelope(w, frame.env);
      return;
    case "Ping":
      w.u32(4);
      return;
  }
}

function writeEnvelope(w: BinWriter, e: Envelope): void {
  if (e.version !== PROTOCOL_VERSION) {
    throw new Error(`unsupported envelope version ${e.version}`);
  }
  w.u8(e.version);
  w.str(e.conv_id);
  w.str(e.sender);
  w.len(e.to.length);
  for (const t of e.to) w.str(t);
  w.u64(e.seq);
  w.u32(MSG_KIND[e.kind]);
  w.u32(BODY_INDEX[e.body.t]);
  switch (e.body.t) {
    case "Text":
    case "Notice":
      w.str(e.body.value);
      break;
    case "Bytes":
      w.len(e.body.value.length);
      w.raw(e.body.value);
      break;
    case "Recall":
      w.fixed(e.body.target, "recall target");
      break;
    case "Custom":
      w.str(e.body.kind);
      w.str(jsonText(e.body.payload));
      break;
  }
  w.u64(e.sent_at_ms);
  w.fixed(e.msg_id, "msg_id");
  w.optional(e.quote_of);
  w.u8(e.receipt ? 1 : 0);
  if (e.receipt) {
    if (e.receipt.t === "Delivered") w.u32(0);
    else {
      w.u32(1);
      w.fixed(e.receipt.up_to_msg_id, "receipt.up_to_msg_id");
    }
  }
}

// ---------------------------------------------------------------------------
// decode
// ---------------------------------------------------------------------------

export function decodeFrame(bytes: Uint8Array): WireFrame {
  const r = new BinReader(bytes);
  const frame = readFrame(r);
  // `bincode::serialize` on the hub side writes exactly one frame per message,
  // so leftovers mean we misparsed a field — surface it instead of shrugging.
  if (!r.eof()) throw new Error(`bincode: ${r.leftover()} trailing bytes unconsumed`);
  return frame;
}

function readFrame(r: BinReader): WireFrame {
  switch (r.u32()) {
    case 0:
      return { t: "Hello", token: r.str() };
    case 1:
      return { t: "HelloAck", user_id: r.str(), session_id: r.str() };
    case 2:
      return { t: "HelloReject", reason: r.str() };
    case 3:
      return { t: "Msg", env: readEnvelope(r) };
    case 4:
      return { t: "Ping" };
    default:
      throw new Error("bincode: unknown WireFrame variant");
  }
}

function readEnvelope(r: BinReader): Envelope {
  const version = r.u8();
  if (version !== PROTOCOL_VERSION) {
    throw new Error(`bincode: unsupported envelope version ${version}`);
  }
  const conv_id = r.str();
  const sender = r.str();
  const toLen = r.len();
  if (toLen > 100_000) throw new Error(`bincode: recipient count ${toLen} implausible`);
  const to: string[] = [];
  for (let i = 0; i < toLen; i++) to.push(r.str());
  const seq = r.u64();
  const kind = readKind(r.u32());
  const body = readBody(r);
  const sent_at_ms = r.u64();
  const msg_id = r.fixed();
  const quote_of = r.optional();
  const receipt = r.u8() === 1 ? readReceipt(r) : null;
  if (kindOf(body) !== kind) {
    // the two are derivable in Rust; a disagreement means a corrupt frame
    throw new Error(`bincode: kind ${kind} inconsistent with body ${body.t}`);
  }
  return {
    version,
    conv_id,
    sender,
    to,
    seq,
    kind,
    body,
    sent_at_ms,
    msg_id,
    quote_of,
    receipt,
  };
}

function readKind(index: number): MsgKind {
  const kind = index === 0 ? "Text" : index === 1 ? "Blob" : index === 2 ? "System" : null;
  if (!kind) throw new Error(`bincode: unknown MsgKind index ${index}`);
  return kind;
}

function readBody(r: BinReader): MsgBody {
  switch (r.u32()) {
    case 0:
      return { t: "Text", value: r.str() };
    case 1:
      return { t: "Bytes", value: r.bytesVec() };
    case 2:
      return { t: "Notice", value: r.str() };
    case 3:
      return { t: "Recall", target: r.fixed() };
    case 4: {
      const kind = r.str();
      const text = r.str();
      let payload: unknown;
      try {
        payload = JSON.parse(text) as unknown;
      } catch {
        throw new Error(`bincode: Custom payload is not JSON: ${text.slice(0, 80)}`);
      }
      return { t: "Custom", kind, payload };
    }
    default:
      throw new Error("bincode: unknown MsgBody variant");
  }
}

function readReceipt(r: BinReader): Receipt {
  switch (r.u32()) {
    case 0:
      return { t: "Delivered" };
    case 1:
      return { t: "Read", up_to_msg_id: r.fixed() };
    default:
      throw new Error("bincode: unknown Receipt variant");
  }
}

// ---------------------------------------------------------------------------
// helpers shared with the transport layer
// ---------------------------------------------------------------------------

/** `hex::encode` — the form the UI, HTTP routes and IndexedDB records use. */
export function hex(b: Uint8Array): string {
  let out = "";
  for (const byte of b) out += byte.toString(16).padStart(2, "0");
  return out;
}

/** `hex::decode`, any length. Byte fields (media references) use this. */
export function hexToBytes(s: string): Uint8Array {
  const clean = s.trim();
  if (!/^[\da-fA-F]*$/.test(clean) || clean.length % 2 !== 0) {
    throw new Error(`expected hex, got "${clean.slice(0, 40)}"`);
  }
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** `hex::decode` for a `MsgId` — 16 bytes exactly, which is what the server refuses otherwise. */
export function unhex(s: string): Uint8Array {
  const bytes = hexToBytes(s);
  if (bytes.length !== MSG_ID_LEN) {
    throw new Error(`expected ${MSG_ID_LEN} bytes as hex, got "${s.trim().slice(0, 40)}"`);
  }
  return bytes;
}

/** Envelope → the `im-in` event payload the UI renders (`render_event_with`). */
export function envelopeToEvent(
  env: Envelope,
  decrypt?: (payload: Record<string, unknown>) => { text: string; failed: boolean } | null,
): Record<string, unknown> {
  const ev: Record<string, unknown> = {
    sender: env.sender,
    conv_id: env.conv_id,
    seq: env.seq,
    sent_at_ms: env.sent_at_ms,
    msg_id: hex(env.msg_id),
    quote_of: env.quote_of ? hex(env.quote_of) : null,
  };
  if (env.receipt) {
    ev.kind = "receipt";
    const target =
      env.receipt.t === "Read" ? hex(env.receipt.up_to_msg_id) : env.quote_of ? hex(env.quote_of) : "";
    ev.receipt = { state: env.receipt.t === "Delivered" ? "delivered" : "read", target };
    return ev;
  }
  switch (env.body.t) {
    case "Text":
      ev.kind = "text";
      ev.text = env.body.value;
      break;
    case "Bytes":
      // image messages carry the media id as UTF-8 bytes, never the bytes
      ev.kind = "image";
      ev.media_id = new TextDecoder().decode(env.body.value);
      break;
    case "Notice":
      ev.kind = "system";
      ev.text = env.body.value;
      break;
    case "Recall":
      ev.kind = "recall";
      ev.target = hex(env.body.target);
      break;
    case "Custom": {
      ev.kind = env.body.kind;
      ev.payload = env.body.payload;
      if (env.body.kind === "e2ee.dm") {
        ev.kind = "e2ee";
        const box = env.body.payload as Record<string, unknown>;
        const out = decrypt ? decrypt(box) : null;
        if (out) {
          ev.text = out.text;
          ev[out.failed ? "decrypt_failed" : "encrypted"] = true;
        } else {
          ev.text = "[加密消息·本端未启用或格式不符]";
          ev.decrypt_failed = true;
        }
      }
      break;
    }
  }
  return ev;
}
