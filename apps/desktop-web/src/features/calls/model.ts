/** Types for the calls feature (G11-03). Mirrors crates/platform/src/call.rs. */

export type CallTrack = "audio" | "video";

export interface CallParticipant {
  user: string;
  device?: string;
  role?: string;
  state?: string;
  muted_audio?: boolean;
  muted_video?: boolean;
  hand_raised?: boolean;
}

export interface CallState {
  conv: string;
  state: "idle" | "active" | "locked" | string;
  participants: CallParticipant[];
  recording?: boolean;
  capacity?: number;
}

export interface CallSignal {
  kind: "offer" | "answer" | "candidate" | "bye" | string;
  from?: string;
  payload?: unknown;
}

export type CallEndReason =
  | "local-hangup"
  | "remote-hangup"
  | "declined"
  | "busy"
  | "timeout"
  | "error";

export interface RequestOpts {
  signal?: AbortSignal;
  idempotencyKey?: string;
}

/** rtc signaling wire shape (wxlike_rtc::signal::Signal) — opaque passthrough. */
export interface SignalEnvelope {
  kind: string;
  [key: string]: unknown;
}

export interface CallPorts {
  state(conv: string, opts?: RequestOpts): Promise<CallState>;
  start(conv: string, capacity?: number, opts?: RequestOpts): Promise<void>;
  join(conv: string, opts?: RequestOpts): Promise<void>;
  leave(conv: string, opts?: RequestOpts): Promise<void>;
  mute(conv: string, track: CallTrack, muted: boolean, target?: string, opts?: RequestOpts): Promise<void>;
  muteAll(conv: string, track: CallTrack, muted: boolean, opts?: RequestOpts): Promise<void>;
  lock(conv: string, locked: boolean, opts?: RequestOpts): Promise<void>;
  setRecording(conv: string, on: boolean, opts?: RequestOpts): Promise<void>;
  raiseHand(conv: string, value: boolean, opts?: RequestOpts): Promise<void>;
  kick(conv: string, target: string, opts?: RequestOpts): Promise<void>;
  postSignal(conv: string, signal: SignalEnvelope, opts?: RequestOpts): Promise<void>;
  takeSignals(conv: string, opts?: RequestOpts): Promise<SignalEnvelope[]>;
}
