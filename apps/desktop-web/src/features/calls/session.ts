/**
 * CallSession (G11-03) — framework-free call controller.
 *
 * Owns: local media acquisition through PlatformPort.media (real
 * getUserMedia), track enable/disable, peer-connection lifecycle with
 * injectable factories, signaling pump over the real /api/call/{conv}/signal
 * endpoints, and deterministic teardown (leave + stop tracks + close pc).
 *
 * Rules the acceptance card pins down:
 * - 最小化只改变布局不断通话 → `minimized` is pure UI state, never teardown.
 * - 接听/拒绝/结束原因和权限错误明确 → every terminal path carries a reason.
 * - 切页无摄像头泄漏 → dispose() stops every local track exactly once.
 */

import type { CallPorts, CallEndReason, CallTrack } from "./model";

export type PermissionKind = "camera" | "microphone";

export interface CallSessionDeps {
  ports: CallPorts;
  /** PlatformPort-bound media acquisition. */
  media: (kind: "camera" | "microphone") => Promise<MediaStream>;
  /** Injectable RTCPeerConnection factory (tests use stubs). */
  createPeer?: () => RTCPeerConnection;
  /** Signaling poll interval; default 2000ms. */
  pollMs?: number;
  onState?: (snapshot: CallSnapshot) => void;
}

export interface CallSnapshot {
  phase: "idle" | "ringing" | "connecting" | "connected" | "ended";
  minimized: boolean;
  micMuted: boolean;
  cameraMuted: boolean;
  handRaised: boolean;
  localStream: MediaStream | null;
  remoteStream: MediaStream | null;
  endReason?: CallEndReason;
  error?: string;
}

export class CallSession {
  private deps: CallSessionDeps;
  private conv = "";
  private localStream: MediaStream | null = null;
  private peer: RTCPeerConnection | null = null;
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private disposed = false;
  private snap: CallSnapshot = {
    phase: "idle",
    minimized: false,
    micMuted: false,
    cameraMuted: false,
    handRaised: false,
    localStream: null,
    remoteStream: null,
  };

  constructor(deps: CallSessionDeps) {
    this.deps = deps;
  }

  get snapshot(): CallSnapshot {
    return this.snap;
  }

  private emit(patch: Partial<CallSnapshot>): void {
    this.snap = { ...this.snap, ...patch };
    this.deps.onState?.(this.snap);
  }

  /** Caller side: create the room and acquire media. */
  async start(conv: string): Promise<void> {
    if (this.snap.phase !== "idle" && this.snap.phase !== "ended") return; // 重复提交
    this.emit({ phase: "connecting" }); // synchronous: blocks a concurrent second start
    this.conv = conv;
    try {
      await this.deps.ports.start(conv);
    } catch (err) {
      this.emit({ phase: "ended", endReason: "error", error: describeError(err) });
      return;
    }
    await this.acquireMediaAndConnect();
  }

  /** Callee side: accept a ringing invite. */
  async accept(conv: string): Promise<void> {
    if (this.snap.phase !== "idle" && this.snap.phase !== "ended") return;
    this.emit({ phase: "connecting" });
    this.conv = conv;
    try {
      await this.deps.ports.join(conv);
    } catch (err) {
      this.emit({ phase: "ended", endReason: "error", error: describeError(err) });
      return;
    }
    await this.acquireMediaAndConnect();
  }

  /** Callee side: decline with an explicit reason — no media acquired. */
  async decline(conv: string, reason: "declined" | "busy" = "declined"): Promise<void> {
    this.conv = conv;
    try {
      await this.deps.ports.leave(conv);
    } finally {
      this.emit({ phase: "ended", endReason: reason });
    }
  }

  private async acquireMediaAndConnect(): Promise<void> {
    this.emit({ phase: "connecting", endReason: undefined, error: undefined });    try {
      const [mic, cam] = await Promise.all([
        this.deps.media("microphone"),
        this.deps.media("camera"),
      ]);
      this.localStream = new MediaStream([...mic.getAudioTracks(), ...cam.getVideoTracks()]);
      // The two source streams stay unowned here: tracks are moved into the
      // combined stream, and dispose() stops every track exactly once.
    } catch (err) {
      // 权限错误必须明确：NotAllowedError → 权限被拒绝；其余按设备错误呈现
      this.emit({
        phase: "ended",
        endReason: "error",
        error: describeError(err),
        localStream: null,
      });
      await this.leaveQuietly();
      return;
    }
    this.peer = (this.deps.createPeer ?? defaultPeerFactory)();

    this.peer.ontrack = (event) => {
      const [remote] = event.streams;
      if (remote) this.emit({ remoteStream: remote });
    };
    this.peer.onicecandidate = (event) => {
      if (event.candidate) {
        void this.deps.ports
          .postSignal(this.conv, { kind: "candidate", candidate: event.candidate.toJSON() })
          .catch(() => undefined);
      }
    };
    for (const track of this.localStream.getTracks()) this.peer.addTrack(track, this.localStream);

    const offer = await this.peer.createOffer();
    await this.peer.setLocalDescription(offer);
    void this.deps.ports
      .postSignal(this.conv, { kind: "offer", sdp: this.peer.localDescription?.sdp })
      .catch(() => undefined);

    this.startSignalPump();
    this.emit({ phase: "connected", localStream: this.localStream, error: undefined });
  }

  private startSignalPump(): void {
    this.stopSignalPump();
    const pollMs = this.deps.pollMs ?? 2_000;
    this.pollTimer = setInterval(() => {
      if (this.disposed) return;
      void this.deps.ports
        .takeSignals(this.conv)
        .then((signals) => this.applySignals(signals))
        .catch(() => undefined); // transient poll failures are retried next tick
    }, pollMs);
  }

  private stopSignalPump(): void {
    if (this.pollTimer !== null) {
      clearInterval(this.pollTimer);
      this.pollTimer = null;
    }
  }

  private async applySignals(signals: Array<{ kind: string }>): Promise<void> {
    for (const signal of signals) {
      if (signal.kind === "answer") {
        const sdp = (signal as { sdp?: string }).sdp;
        if (sdp && this.peer && !this.peer.currentRemoteDescription) {
          await this.peer.setRemoteDescription({ type: "answer", sdp });
        }
      } else if (signal.kind === "candidate") {
        const candidate = (signal as { candidate?: RTCIceCandidateInit }).candidate;
        if (candidate && this.peer) await this.peer.addIceCandidate(candidate).catch(() => undefined);
      } else if (signal.kind === "bye") {
        await this.hangup("remote-hangup");
      }
    }
  }

  /** Track controls — UI buttons drive real tracks AND the server state. */
  async toggleMute(track: CallTrack, muted: boolean): Promise<void> {
    if (this.localStream) {
      const tracks = track === "audio" ? this.localStream.getAudioTracks() : this.localStream.getVideoTracks();
      for (const t of tracks) t.enabled = !muted;
    }
    this.emit(
      track === "audio"
        ? { micMuted: muted }
        : { cameraMuted: muted },
    );
    try {
      await this.deps.ports.mute(this.conv, track, muted);
    } catch (err) {
      this.emit({ error: describeError(err) });
    }
  }

  async raiseHand(value: boolean): Promise<void> {
    this.emit({ handRaised: value });
    try {
      await this.deps.ports.raiseHand(this.conv, value);
    } catch (err) {
      this.emit({ error: describeError(err) });
    }
  }

  /** 最小化只改变布局：状态翻转，媒体/信令/房间一律不动。 */
  setMinimized(minimized: boolean): void {
    this.emit({ minimized });
  }

  /** Hang up with a reason; idempotent — second call is a no-op. */
  async hangup(reason: CallEndReason = "local-hangup"): Promise<void> {
    if (this.disposed) return;
    this.disposed = true;
    this.stopSignalPump();
    const local = this.localStream;
    this.localStream = null;
    if (local) {
      for (const track of local.getTracks()) track.stop();
    }
    if (this.peer) {
      this.peer.ontrack = null;
      this.peer.onicecandidate = null;
      try {
        this.peer.close();
      } catch {
        // already closed
      }
      this.peer = null;
    }
    await this.leaveQuietly();
    void this.deps.ports
      .postSignal(this.conv, { kind: "bye" })
      .catch(() => undefined);
    this.emit({ phase: "ended", endReason: reason, localStream: null, remoteStream: null });
  }

  private async leaveQuietly(): Promise<void> {
    if (!this.conv) return;
    try {
      await this.deps.ports.leave(this.conv);
    } catch {
      // teardown must not throw past the caller
    }
  }
}

function defaultPeerFactory(): RTCPeerConnection {
  return new RTCPeerConnection();
}

export function describeError(err: unknown): string {
  if (err instanceof DOMException) {
    if (err.name === "NotAllowedError") return "摄像头/麦克风权限被拒绝，请在系统设置中允许后重试";
    if (err.name === "NotFoundError") return "未找到可用的摄像头或麦克风设备";
    if (err.name === "NotReadableError") return "设备被其他应用占用，无法读取";
    return `设备错误：${err.name}`;
  }
  return err instanceof Error ? err.message : String(err);
}
