/**
 * Video/live client (G11-03): video.* / live.*.
 *
 * Real today: upload (POST /api/video), meta/playback (mp4 · HLS playlist ·
 * thumb), view/stats, live start/stop/state/join/leave/danmaku
 * (`crates/video`, `crates/platform/src/live.rs`).
 * Contract-gated: the channels feed listing (G05 ContentPort `/videos`)
 * — absent upstream renders the explicit state.
 */

import { ApiError, baseUrl, idemKey, request, token } from "../../lib/http";
import type {
  DanmakuMessage,
  FeedTab,
  LiveStartReply,
  LiveState,
  PlaybackUrls,
  RequestOpts,
  UploadVideoOpts,
  VideoFeedItem,
  VideoMeta,
  VideoPorts,
} from "./model";

export function playbackUrls(videoId: string): PlaybackUrls {
  const id = encodeURIComponent(videoId);
  return {
    mp4: `/api/video/${id}/play.mp4`,
    hls: `/api/video/${id}/playlist.m3u8`,
    thumb: `/api/video/${id}/thumb.png`,
  };
}

/** XHR upload — the video ingest endpoint takes one multipart "file" field. */
export const xhrVideoUpload = async (
  form: FormData,
  opts: { signal?: AbortSignal; onProgress?: (fraction: number) => void } = {},
): Promise<VideoMeta> =>
  new Promise<VideoMeta>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `${baseUrl()}/api/video`);
    const tok = token();
    if (tok) xhr.setRequestHeader("authorization", `Bearer ${tok}`);
    xhr.responseType = "json";
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable) opts.onProgress?.(event.loaded / event.total);
    };
    xhr.onload = () => {
      const body = xhr.response as VideoMeta | { error?: string } | null;
      if (xhr.status >= 200 && xhr.status < 300) resolve(body as VideoMeta);
      else {
        const message =
          body && typeof body === "object" && "error" in body && body.error
            ? String(body.error)
            : `视频上传失败 (${xhr.status})`;
        reject(new ApiError(xhr.status, message, "/api/video"));
      }
    };
    xhr.onerror = () => reject(new ApiError(0, "网络错误：上传中断", "/api/video", "network"));
    xhr.onabort = () => reject(new ApiError(0, "已取消", "/api/video", "cancelled"));
    opts.signal?.addEventListener("abort", () => xhr.abort(), { once: true });
    xhr.send(form);
  });

export class VideoApi implements VideoPorts {
  /** Low-level sender injectable for tests (default: real XHR upload). */
  constructor(private readonly sendVideo: typeof xhrVideoUpload = xhrVideoUpload) {}

  async meta(videoId: string, opts?: RequestOpts): Promise<VideoMeta> {
    return request<VideoMeta>("GET", `/api/video/${encodeURIComponent(videoId)}`, {
      signal: opts?.signal,
    });
  }

  playbackUrls(videoId: string): PlaybackUrls {
    return playbackUrls(videoId);
  }

  async uploadVideo(file: File | Blob, opts: UploadVideoOpts = {}): Promise<VideoMeta> {
    const form = new FormData();
    form.append("file", file, file instanceof File ? file.name : "clip.mp4");
    const send = (opts as { send?: typeof xhrVideoUpload }).send ?? this.sendVideo;
    return send(form, { signal: opts.signal, onProgress: opts.onProgress });
  }

  async recordView(videoId: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/video/${encodeURIComponent(videoId)}/view`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("video-view"),
      signal: opts?.signal,
    });
  }

  async stats(videoId: string, opts?: RequestOpts): Promise<{ views?: number; viewers?: number }> {
    return request<{ views?: number; viewers?: number }>(
      "GET",
      `/api/video/${encodeURIComponent(videoId)}/stats`,
      { signal: opts?.signal },
    );
  }

  async feed(_tab: FeedTab, _opts?: RequestOpts): Promise<VideoFeedItem[]> {
    return request<VideoFeedItem[]>("GET", "/api/videos");
  }

  async liveState(channel: string, opts?: RequestOpts): Promise<LiveState> {
    return request<LiveState>("GET", `/api/live/${encodeURIComponent(channel)}`, {
      signal: opts?.signal,
    });
  }

  async liveStart(channel: string, recording: boolean, opts?: RequestOpts): Promise<LiveStartReply> {
    return request<LiveStartReply>("POST", `/api/live/${encodeURIComponent(channel)}/start`, {
      body: { recording },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("live-start"),
      signal: opts?.signal,
    });
  }

  async liveStop(channel: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/live/${encodeURIComponent(channel)}/stop`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("live-stop"),
      signal: opts?.signal,
    });
  }

  async liveJoin(channel: string, opts?: RequestOpts): Promise<{ viewers: number }> {
    return request<{ viewers: number }>("POST", `/api/live/${encodeURIComponent(channel)}/join`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("live-join"),
      signal: opts?.signal,
    });
  }

  async liveLeave(channel: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/live/${encodeURIComponent(channel)}/leave`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("live-leave"),
      signal: opts?.signal,
    });
  }

  async sendDanmaku(channel: string, text: string, opts?: RequestOpts): Promise<DanmakuMessage> {
    const reply = await request<{ delivered: boolean }>(
      "POST",
      `/api/live/${encodeURIComponent(channel)}/danmaku`,
      {
        body: { text },
        idempotencyKey: opts?.idempotencyKey ?? idemKey("danmaku"),
        signal: opts?.signal,
      },
    );
    return {
      id: `local-${Date.now()}`,
      author: "我",
      text,
      at: Date.now(),
      self: true,
      ...(reply.delivered === false ? { } : {}),
    };
  }
}
