/**
 * 通话面板 (G11-03). Buttons drive real tracks/devices through CallSession;
 * minimizing flips a layout flag only — the call keeps running. Local preview
 * via <video>; remote renders whatever the peer connection delivered.
 */

import { useEffect, useRef, useState } from "react";
import { CallApi } from "./api";
import { CallSession, describeError, type CallSnapshot } from "./session";
import type { CallPorts } from "./model";
import { platform } from "../../lib/platform";
import { Banner, GhostButton, PrimaryButton } from "../content/ui";

const DEFAULT_PORTS = new CallApi();

export function CallPanel({ conv, ports = DEFAULT_PORTS }: { conv: string; ports?: CallPorts }) {
  const [session] = useState(
    () =>
      new CallSession({
        ports,
        media: (kind) => platform().media(kind),
        onState: (snapshot) => setSnap({ ...snapshot }),
      }),
  );
  const [snap, setSnap] = useState<CallSnapshot>(session.snapshot);
  const localVideoRef = useRef<HTMLVideoElement>(null);
  const remoteVideoRef = useRef<HTMLVideoElement>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (localVideoRef.current) localVideoRef.current.srcObject = snap.localStream;
  }, [snap.localStream]);
  useEffect(() => {
    if (remoteVideoRef.current) remoteVideoRef.current.srcObject = snap.remoteStream;
  }, [snap.remoteStream]);

  const dial = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await session.start(conv);
    } finally {
      setBusy(false);
    }
  };

  if (snap.phase === "ended" && snap.endReason) {
    return (
      <div className="rounded-xl border border-zinc-200 bg-white p-4" data-testid="call-ended">
        <p className="text-sm font-medium text-zinc-800">
          通话已结束（{endReasonLabel(snap.endReason)}）
        </p>
        {snap.error ? <Banner tone="error" text={snap.error} /> : null}
      </div>
    );
  }

  if (snap.phase === "idle") {
    return (
      <div className="flex items-center gap-2 rounded-xl border border-zinc-200 bg-white p-4" data-testid="call-idle">
        <span className="text-sm text-zinc-600">会话 {conv}</span>
        <PrimaryButton onClick={() => void dial()} busy={busy}>
          发起通话
        </PrimaryButton>
      </div>
    );
  }

  const minimized = snap.minimized;

  return (
    <div
      className={`${minimized ? "w-56" : "w-full max-w-2xl"} rounded-xl border border-zinc-200 bg-white p-3.5 transition-all`}
      data-testid="call-panel"
      data-minimized={minimized ? "true" : "false"}
    >
      <div className="flex items-center justify-between">
        <p className="text-sm font-medium text-zinc-800">
          通话中 · {conv} <span className="text-xs text-zinc-400">{phaseLabel(snap.phase)}</span>
        </p>
        {/* 最小化：只切换布局类，不断媒体/信令/房间 */}
        <GhostButton
          onClick={() => session.setMinimized(!minimized)}
          testId="call-minimize"
          title={minimized ? "还原" : "最小化"}
        >
          {minimized ? "还原" : "最小化"}
        </GhostButton>
      </div>

      {!minimized ? (
        <div className="mt-2 grid grid-cols-2 gap-2">
          <video
            ref={localVideoRef}
            autoPlay
            muted
            playsInline
            data-testid="call-local-video"
            className="aspect-video w-full rounded-lg bg-black object-cover"
          />
          <video
            ref={remoteVideoRef}
            autoPlay
            playsInline
            data-testid="call-remote-video"
            className="aspect-video w-full rounded-lg bg-black object-cover"
          />
        </div>
      ) : null}

      <div className="mt-2 flex flex-wrap items-center gap-2" data-testid="call-controls">
        <GhostButton
          onClick={() => void session.toggleMute("audio", !snap.micMuted)}
          tone={snap.micMuted ? "danger" : "neutral"}
        >
          {snap.micMuted ? "取消静音" : "静音"}
        </GhostButton>
        <GhostButton
          onClick={() => void session.toggleMute("video", !snap.cameraMuted)}
          tone={snap.cameraMuted ? "danger" : "neutral"}
        >
          {snap.cameraMuted ? "打开摄像头" : "关闭摄像头"}
        </GhostButton>
        <GhostButton onClick={() => void session.raiseHand(!snap.handRaised)}>
          {snap.handRaised ? "放下手" : "举手"}
        </GhostButton>
        <GhostButton
          tone="danger"
          onClick={() => void session.hangup("local-hangup")}
          testId="call-hangup"
        >
          挂断
        </GhostButton>
      </div>

      {snap.error ? <Banner tone="error" text={snap.error} /> : null}
    </div>
  );
}

function phaseLabel(phase: CallSnapshot["phase"]): string {
  switch (phase) {
    case "connecting": return "连接中";
    case "connected": return "已接通";
    case "ringing": return "响铃中";
    default: return "";
  }
}

function endReasonLabel(reason: string): string {
  switch (reason) {
    case "local-hangup": return "本端挂断";
    case "remote-hangup": return "对方挂断";
    case "declined": return "已拒绝";
    case "busy": return "对方忙";
    case "timeout": return "超时";
    default: return "异常结束";
  }
}

export { describeError };
