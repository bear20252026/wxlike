/**
 * G11-03 test matrix (calls side) — session lifecycle with injected media and
 * peer stubs: accept/decline reasons, mute drives real tracks + server state,
 * minimize never tears down, hangup stops every track exactly once, permission
 * errors are explicit, duplicate start guarded.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { CallApi } from "./api";
import { CallSession, describeError, type CallSnapshot } from "./session";
import type { CallPorts, CallTrack } from "./model";

afterEach(() => {
  vi.restoreAllMocks();
});

// jsdom has no MediaStream; CallSession composes one from the acquired tracks
// (assignment must come after the class declaration).


// ---- test doubles ----------------------------------------------------------

class FakeTrack {
  enabled = true;
  stopped = false;
  stopCount = 0;
  constructor(readonly kind: "audio" | "video") {}
  stop(): void {
    this.stopped = true;
    this.stopCount += 1;
  }
}

class FakeMediaStream {
  constructor(private tracks: FakeTrack[]) {}
  getAudioTracks(): FakeTrack[] {
    return this.tracks.filter((t) => t.kind === "audio");
  }
  getVideoTracks(): FakeTrack[] {
    return this.tracks.filter((t) => t.kind === "video");
  }
  getTracks(): FakeTrack[] {
    return this.tracks;
  }
}

class FakePeer {
  ontrack: ((event: unknown) => void) | null = null;
  onicecandidate: ((event: unknown) => void) | null = null;
  localDescription: RTCSessionDescriptionInit | null = null;
  currentRemoteDescription: RTCSessionDescriptionInit | null = null;
  closed = false;
  addedTracks: FakeTrack[] = [];
  async createOffer(): Promise<RTCSessionDescriptionInit> {
    return { type: "offer", sdp: "offer-sdp" };
  }
  async setLocalDescription(desc: RTCSessionDescriptionInit): Promise<void> {
    this.localDescription = desc;
  }
  async setRemoteDescription(desc: RTCSessionDescriptionInit): Promise<void> {
    this.currentRemoteDescription = desc;
  }
  async addIceCandidate(): Promise<void> {}
  addTrack(track: FakeTrack): void {
    this.addedTracks.push(track);
  }
  close(): void {
    this.closed = true;
  }
}

function fakeMedia(audio: FakeTrack, video: FakeTrack) {
  return async (kind: "camera" | "microphone"): Promise<MediaStream> =>
    (kind === "microphone" ? new FakeMediaStream([audio]) : new FakeMediaStream([video])) as unknown as MediaStream;
}

(globalThis as unknown as { MediaStream?: unknown }).MediaStream = FakeMediaStream;

interface Harness {
  session: CallSession;
  audio: FakeTrack;
  video: FakeTrack;
  peer: FakePeer;
  posts: Array<{ path: string; body: unknown }>;
  snapshot: () => CallSnapshot;
}

async function harness(overrides: Partial<CallPorts> = {}, mediaError?: Error): Promise<Harness> {
  const audio = new FakeTrack("audio");
  const video = new FakeTrack("video");
  const peer = new FakePeer();
  const posts: Array<{ path: string; body: unknown }> = [];
  const ports: CallPorts = {
    state: async () => ({ conv: "c1", state: "active", participants: [] }),
    start: async (conv) => {
      posts.push({ path: `/api/call/${conv}/start`, body: null });
    },
    join: async (conv) => {
      posts.push({ path: `/api/call/${conv}/join`, body: null });
    },
    leave: async (conv) => {
      posts.push({ path: `/api/call/${conv}/leave`, body: null });
    },
    mute: async (conv, track, muted) => {
      posts.push({ path: `/api/call/${conv}/mute`, body: { track, muted } });
    },
    muteAll: async () => undefined,
    lock: async () => undefined,
    setRecording: async () => undefined,
    raiseHand: async () => undefined,
    kick: async () => undefined,
    postSignal: async (conv, signal) => {
      posts.push({ path: `/api/call/${conv}/signal`, body: signal });
    },
    takeSignals: async () => [],
    ...overrides,
  };
  const media = mediaError
    ? async () => {
        throw mediaError;
      }
    : fakeMedia(audio, video);
  let latest: CallSnapshot | undefined;
  const session = new CallSession({
    ports,
    media: media as (kind: "camera" | "microphone") => Promise<MediaStream>,
    createPeer: () => peer as unknown as RTCPeerConnection,
    pollMs: 60_000,
    onState: (snap) => {
      latest = snap;
    },
  });
  return {
    session,
    audio,
    video,
    peer,
    posts,
    snapshot: () => {
      if (!latest) throw new Error("session never emitted a snapshot");
      return latest;
    },
  };
}

// ---- tests -----------------------------------------------------------------

describe("CallSession — lifecycle", () => {
  it("start：获取真实媒体、加入 peer、发出 offer 信令、进入 connected", async () => {
    const h = await harness();
    await h.session.start("c1");
    expect(h.snapshot().phase).toBe("connected");
    expect(h.snapshot().localStream).not.toBeNull();
    expect(h.peer.addedTracks).toHaveLength(2);
    expect(h.posts).toContainEqual({ path: "/api/call/c1/start", body: null });
    const offer = h.posts.find((post) => post.path.endsWith("/signal"));
    expect(offer?.body).toMatchObject({ kind: "offer", sdp: "offer-sdp" });
  });

  it("权限被拒绝：错误明确（NotAllowedError → 权限提示）并回房", async () => {
    const h = await harness({}, new DOMException("denied", "NotAllowedError"));
    await h.session.start("c1");
    const snap = h.snapshot();
    expect(snap.phase).toBe("ended");
    expect(snap.error).toContain("权限被拒绝");
    expect(snap.localStream).toBeNull();
    expect(h.posts).toContainEqual({ path: "/api/call/c1/leave", body: null });
  });

  it("设备不存在：错误明确", async () => {
    const h = await harness({}, new DOMException("none", "NotFoundError"));
    await h.session.start("c1");
    expect(h.snapshot().error).toContain("未找到可用的摄像头或麦克风");
  });

  it("accept：加入房间后 connected", async () => {
    const h = await harness();
    await h.session.accept("c2");
    expect(h.snapshot().phase).toBe("connected");
    expect(h.posts).toContainEqual({ path: "/api/call/c2/join", body: null });
  });

  it("decline：不取媒体，直接 leave 并给出拒绝原因", async () => {
    const h = await harness();
    await h.session.decline("c3", "declined");
    expect(h.snapshot().phase).toBe("ended");
    expect(h.snapshot().endReason).toBe("declined");
    expect(h.snapshot().localStream).toBeNull();
    expect(h.posts).toContainEqual({ path: "/api/call/c3/leave", body: null });
  });

  it("hangup：停掉所有本地轨（各一次）、关闭 peer、leave、bye", async () => {
    const h = await harness();
    await h.session.start("c1");
    await h.session.hangup("local-hangup");
    const snap = h.snapshot();
    expect(snap.phase).toBe("ended");
    expect(snap.endReason).toBe("local-hangup");
    expect(h.audio.stopCount).toBe(1);
    expect(h.video.stopCount).toBe(1);
    expect(h.peer.closed).toBe(true);
    expect(h.posts).toContainEqual({ path: "/api/call/c1/leave", body: null });
    const bye = h.posts.filter((post) => post.path.endsWith("/signal"));
    expect(bye.some((post) => (post.body as { kind: string }).kind === "bye")).toBe(true);
    // 二次挂断无副作用
    await h.session.hangup("local-hangup");
    expect(h.audio.stopCount).toBe(1);
  });

  it("远端 bye → 本地以 remote-hangup 结束", async () => {
    const leaveCalls: string[] = [];
    const h = await harness({
      takeSignals: async () => [{ kind: "bye" }],
      leave: async (conv) => {
        leaveCalls.push(conv);
      },
    });
    await h.session.start("c1");
    // pump interval 60s — drive the signal application directly via a poll tick
    await (h.session as unknown as { applySignals(signals: Array<{ kind: string; sdp?: string }>): Promise<void> }).applySignals([
      { kind: "bye" },
    ]);
    expect(h.snapshot().endReason).toBe("remote-hangup");
    expect(leaveCalls).toContain("c1");
  });
});

describe("CallSession — controls", () => {
  it("静音：真实轨道 enabled=false + 服务端 mute 状态", async () => {
    const h = await harness();
    await h.session.start("c1");
    await h.session.toggleMute("audio", true);
    expect(h.audio.enabled).toBe(false);
    expect(h.video.enabled).toBe(true);
    expect(h.posts).toContainEqual({ path: "/api/call/c1/mute", body: { track: "audio", muted: true } });
    expect(h.snapshot().micMuted).toBe(true);
    await h.session.toggleMute("video", true);
    expect(h.video.enabled).toBe(false);
    expect(h.snapshot().cameraMuted).toBe(true);
  });

  it("举手状态翻转", async () => {
    const h = await harness();
    await h.session.start("c1");
    await h.session.raiseHand(true);
    expect(h.snapshot().handRaised).toBe(true);
  });

  it("最小化只改变布局标志：媒体、peer、阶段全部不变", async () => {
    const h = await harness();
    await h.session.start("c1");
    await h.session.setMinimized(true);
    const snap = h.snapshot();
    expect(snap.minimized).toBe(true);
    expect(snap.phase).toBe("connected");
    expect(snap.localStream).not.toBeNull();
    expect(h.audio.stopped).toBe(false);
    expect(h.peer.closed).toBe(false);
  });

  it("重复 start 只执行一次", async () => {
    const starts: string[] = [];
    const h = await harness({
      start: async (conv) => {
        starts.push(conv);
      },
    });
    await Promise.all([h.session.start("c1"), h.session.start("c1")]);
    expect(starts).toEqual(["c1"]);
  });

  it("远端 answer 设置 remoteDescription", async () => {
    const h = await harness();
    await h.session.start("c1");
    await (h.session as unknown as { applySignals(signals: Array<{ kind: string; sdp?: string }>): Promise<void> }).applySignals([
      { kind: "answer", sdp: "answer-sdp" },
    ]);
    expect(h.peer.currentRemoteDescription).toMatchObject({ type: "answer", sdp: "answer-sdp" });
  });
});

describe("unit", () => {
  it("describeError 映射常见设备错误", () => {
    expect(describeError(new DOMException("x", "NotAllowedError"))).toContain("权限被拒绝");
    expect(describeError(new DOMException("x", "NotReadableError"))).toContain("被其他应用占用");
    expect(describeError(new Error("boom"))).toBe("boom");
  });

  it("CallApi 走真实端点与幂等键", async () => {
    const { loggingTransport, CallLog } = await import("../../test/fixture");
    const log = new CallLog();
    const { setTransport } = await import("../../lib/http");
    setTransport(
      loggingTransport(
        {
          "POST /api/call/:conv/mute": () => null,
        },
        log,
      ),
    );
    const api = new CallApi();
    await api.mute("c9", "audio" as CallTrack, true);
    const rows = log.all({ path: "/api/call/c9/mute" });
    expect(rows).toHaveLength(1);
    expect(rows[0]?.idempotencyKey).toMatch(/^call-mute-/);
  });
});
