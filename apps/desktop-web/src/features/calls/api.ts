/**
 * Calls client (G11-03): call.* — real endpoints on crates/platform/call.rs.
 * Signaling envelopes are opaque (wxlike_rtc::signal::Signal) — the WebRTC
 * session layer owns their contents, the client never rewrites them.
 */

import { idemKey, request } from "../../lib/http";
import type { CallPorts, CallState, CallTrack, RequestOpts, SignalEnvelope } from "./model";

export class CallApi implements CallPorts {
  async state(conv: string, opts?: RequestOpts): Promise<CallState> {
    return request<CallState>("GET", `/api/call/${encodeURIComponent(conv)}`, {
      signal: opts?.signal,
    });
  }

  /** Empty body is legal server-side (default capacity). */
  async start(conv: string, capacity?: number, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/start`, {
      body: capacity === undefined ? {} : { capacity },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-start"),
      signal: opts?.signal,
    });
  }

  async join(conv: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/join`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-join"),
      signal: opts?.signal,
    });
  }

  async leave(conv: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/leave`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-leave"),
      signal: opts?.signal,
    });
  }

  async mute(
    conv: string,
    track: CallTrack,
    muted: boolean,
    target?: string,
    opts?: RequestOpts,
  ): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/mute`, {
      body: { track, muted, ...(target ? { target } : {}) },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-mute"),
      signal: opts?.signal,
    });
  }

  async muteAll(conv: string, track: CallTrack, muted: boolean, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/mute-all`, {
      body: { track, muted },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-mute-all"),
      signal: opts?.signal,
    });
  }

  async lock(conv: string, locked: boolean, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/lock`, {
      body: { value: locked },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-lock"),
      signal: opts?.signal,
    });
  }

  async setRecording(conv: string, on: boolean, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/record`, {
      body: { value: on },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-record"),
      signal: opts?.signal,
    });
  }

  async raiseHand(conv: string, value: boolean, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/hand`, {
      body: { value },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-hand"),
      signal: opts?.signal,
    });
  }

  async kick(conv: string, target: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/kick`, {
      body: { target },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("call-kick"),
      signal: opts?.signal,
    });
  }

  async postSignal(conv: string, signal: SignalEnvelope, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/call/${encodeURIComponent(conv)}/signal`, {
      body: signal,
      signal: opts?.signal,
    });
  }

  async takeSignals(conv: string, opts?: RequestOpts): Promise<SignalEnvelope[]> {
    const reply = await request<{ signals?: SignalEnvelope[] }>(
      "GET",
      `/api/call/${encodeURIComponent(conv)}/signal`,
      { signal: opts?.signal },
    );
    return reply.signals ?? [];
  }
}
