/**
 * WebSocket client for the hub gateway (`/ws`).
 *
 * Browsers have no QUIC, so `/ws` is the only real-time path — and it speaks
 * the same bincode `WireFrame` protocol as a QUIC stream, `Hello` first
 * (`server/src/lib.rs::ws_routes`, `crates/net/src/conn.rs`). One binary
 * message is one frame; there is no length prefix here.
 *
 * The hub answers `WireFrame::Ping` by ignoring it (`mq_hub.rs`: only `Msg`
 * frames are processed), so the keepalive below is purely about keeping the
 * socket busy. There is no `Pong` variant in the protocol.
 */

import { decodeFrame, encodeFrame, type WireFrame } from "./wire";

export type HubListener = (frame: WireFrame) => void;
export type StatusListener = (online: boolean, reason: string) => void;

const FIRST_PING_DELAY_MS = 25_000;
const RECONNECT_MIN_MS = 1_000;
const RECONNECT_MAX_MS = 15_000;

export class Hub {
  private ws: WebSocket | null = null;
  /// 连接凭据（HTTP 登录拿到的 token）。**不是 user_id**：身份由服务端解析
  /// （issue #3），客户端从 `HelloAck` 得知自己是谁。
  private token = "";
  /// 服务端在 `HelloAck` 里给出的身份。握手前是空串。
  private userId = "";
  private url = "";
  private manual = false;
  private attempts = 0;
  private reconnectTimer = 0;
  private pingTimer = 0;
  private frameListeners = new Set<HubListener>();
  private statusListeners = new Set<StatusListener>();
  /** Resolves the first `connect()` once Hello is on the wire. */
  private ready: Promise<void> | null = null;
  private settleReady: (() => void) | null = null;
  private failReady: ((e: Error) => void) | null = null;

  onFrame(l: HubListener): () => void {
    this.frameListeners.add(l);
    return () => this.frameListeners.delete(l);
  }

  onStatus(l: StatusListener): () => void {
    this.statusListeners.add(l);
    return () => this.statusListeners.delete(l);
  }

  get online(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  /** Opens the socket and authenticates with `token`. Rejects if it cannot. */
  connect(url: string, token: string): Promise<void> {
    this.manual = false;
    // 换了服务器或账号必须重连：登录页可以先连 A 再登 B
    const sameTarget = this.online && this.url === url && this.token === token;
    this.url = url;
    this.token = token;
    if (sameTarget) return Promise.resolve();
    if (this.ws) {
      this.manual = true; // 旧 socket 的 onclose 不该触发重连
      try {
        this.ws.close();
      } catch {
        /* already gone */
      }
      this.ws = null;
      window.clearInterval(this.pingTimer);
      this.manual = false;
    }
    this.ready = new Promise<void>((resolve, reject) => {
      this.settleReady = resolve;
      this.failReady = reject;
    });
    this.open();
    return this.ready;
  }

  close(reason = "closed"): void {
    this.manual = true;
    window.clearTimeout(this.reconnectTimer);
    window.clearInterval(this.pingTimer);
    this.settle();
    if (this.ws) {
      try {
        this.ws.close();
      } catch {
        /* already gone */
      }
      this.ws = null;
    }
    this.emitStatus(false, reason);
  }

  /** Sends one frame; rejects when the socket is not usable (caller queues). */
  async send(frame: WireFrame): Promise<void> {
    const ws = this.ws;
    if (!ws || ws.readyState !== WebSocket.OPEN) throw new Error("未连接到服务器");
    ws.send(encodeFrame(frame));
  }

  private open(): void {
    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    ws.onopen = () => {
      this.attempts = 0;
      ws.send(encodeFrame({ t: "Hello", token: this.token }));
      this.startPings();
      this.settle();
      this.emitStatus(true, "open");
    };
    ws.onmessage = (ev) => this.receive(ev.data as ArrayBuffer);
    ws.onerror = () => {
      this.settleFail(new Error("WebSocket 连接失败"));
    };
    ws.onclose = (ev) => {
      if (this.ws === ws) this.ws = null;
      window.clearInterval(this.pingTimer);
      this.emitStatus(false, ev.reason || "closed");
      if (!this.manual) this.scheduleReconnect();
    };
  }

  private receive(buffer: ArrayBuffer): void {
    let frame: WireFrame;
    try {
      frame = decodeFrame(new Uint8Array(buffer));
    } catch (err) {
      // A frame we cannot parse is a protocol mismatch, not noise: say so.
      console.error("[hub] undecodable frame:", err, buffer.byteLength);
      return;
    }
    // 握手应答由这里吸收：`HelloAck` 记下服务端认定的身份，`HelloReject`
    // 说明凭据被拒——两者都不该交给业务层当成普通消息。
    if (frame.t === "HelloAck") {
      this.userId = frame.user_id;
    } else if (frame.t === "HelloReject") {
      this.emitStatus(false, `凭据被拒：${frame.reason}`);
      this.manual = true; // 凭据错了就别重连了，会一直失败
      return;
    }
    for (const l of this.frameListeners) l(frame);
  }

  /** 服务端认定的身份（`HelloAck` 之后才有值）。 */
  currentUserId(): string {
    return this.userId;
  }

  private startPings(): void {
    window.clearInterval(this.pingTimer);
    this.pingTimer = window.setInterval(() => {
      if (this.online) this.ws?.send(encodeFrame({ t: "Ping" }));
    }, FIRST_PING_DELAY_MS);
  }

  private scheduleReconnect(): void {
    window.clearTimeout(this.reconnectTimer);
    const delay = Math.min(RECONNECT_MAX_MS, RECONNECT_MIN_MS * 2 ** this.attempts);
    this.attempts += 1;
    this.reconnectTimer = window.setTimeout(() => {
      if (this.manual || !this.token) return;
      this.ready = new Promise<void>((resolve, reject) => {
        this.settleReady = resolve;
        this.failReady = reject;
      });
      this.open();
      void this.ready.catch((err) => console.warn("[hub] reconnect failed:", String(err)));
    }, delay);
  }

  private settle(): void {
    const done = this.settleReady;
    this.settleReady = null;
    this.failReady = null;
    done?.();
  }

  private settleFail(err: Error): void {
    const fail = this.failReady;
    this.settleReady = null;
    this.failReady = null;
    fail?.(err);
  }

  private emitStatus(online: boolean, reason: string): void {
    for (const l of this.statusListeners) l(online, reason);
  }
}

export const hub = new Hub();
