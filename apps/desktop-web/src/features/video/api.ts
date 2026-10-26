/**
 * Video/live client (G11-03): video.* / live.*.
 *
 * Real today: upload (POST /api/video, multipart field "bytes"), meta/playback
 * (mp4 · HLS playlist · thumb), view/stats (`crates/video`), feed
 * (GET /api/videos/feed?mode=hot|follow, `crates/platform/src/video_feed.rs`),
 * live start/stop/state/join/leave/danmaku (`crates/platform/src/live.rs`).
 */

import { ApiError, baseUrl, idemKey, request, token, userIdSafe } from "../../lib/http";
import type {
  DanmakuMessage,
  FeedEnvelope,
  FeedOpts,
  FeedTab,
  IngestedVideo,
  LiveStartReply,
  LiveState,
  PlaybackUrls,
  RequestOpts,
  UploadVideoOpts,
  VideoFeedItem,
  VideoMeta,
  VideoPorts,
  VideoViewRow,
  ViewOpts,
} from "./model";

export function playbackUrls(videoId: string): PlaybackUrls {
  const id = encodeURIComponent(videoId);
  return {
    mp4: `/api/video/${id}/play.mp4`,
    hls: `/api/video/${id}/playlist.m3u8`,
    thumb: `/api/video/${id}/thumb.png`,
  };
}

/** Map the UI feed tab onto the server's feed mode. */
export function feedMode(tab: FeedTab): "hot" | "follow" {
  return tab === "trending" ? "hot" : "follow";
}

/** Server VideoView → UI VideoFeedItem (field names differ on purpose). */
export function mapFeedRow(row: VideoViewRow): VideoFeedItem {
  return {
    video_id: row.id,
    author: row.author,
    author_name: row.author_name,
    title: row.caption,
    thumb: row.thumb_url,
    likes: row.like_count,
    followed: row.liked,
    published_at: row.created_at,
  };
}

/** XHR upload — the video ingest endpoint takes one multipart "bytes" field. */
export const xhrVideoUpload = async (
  form: FormData,
  opts: { signal?: AbortSignal; onProgress?: (fraction: number) => void } = {},
): Promise<IngestedVideo> =>
  new Promise<IngestedVideo>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `${baseUrl()}/api/video`);
    const tok = token();
    if (tok) xhr.setRequestHeader("authorization", `Bearer ${tok}`);
    xhr.responseType = "json";
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable) opts.onProgress?.(event.loaded / event.total);
    };
    xhr.onload = () => {
      const body = xhr.response as IngestedVideo | { error?: string } | null;
      if (xhr.status >= 200 && xhr.status < 300) resolve(body as IngestedVideo);
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

  async uploadVideo(file: File | Blob, opts: UploadVideoOpts = {}): Promise<IngestedVideo> {
    const form = new FormData();
    // crates/video read_field expects the multipart part to be named "bytes"
    // (`curl -F bytes=@demo.mp4 POST /api/video`).
    form.append("bytes", file, file instanceof File ? file.name : "clip.mp4");
    const send = (opts as { send?: typeof xhrVideoUpload }).send ?? this.sendVideo;
    return send(form, { signal: opts.signal, onProgress: opts.onProgress });
  }

  async recordView(videoId: string, watchedMs: number, opts: ViewOpts = {}): Promise<void> {
    const viewer = (opts.viewer ?? (userIdSafe() || "ui")).trim();
    await request<unknown>("POST", `/api/video/${encodeURIComponent(videoId)}/view`, {
      // The stats endpoint requires a JSON body {viewer, watched_ms} — it
      // rejects an empty body before the payload is ever validated.
      body: { viewer, watched_ms: Math.max(1, Math.round(watchedMs)) },
      idempotencyKey: opts.idempotencyKey ?? idemKey("video-view"),
      signal: opts.signal,
    });
  }

  async stats(videoId: string, opts?: RequestOpts): Promise<{ views?: number; viewers?: number }> {
    return request<{ views?: number; viewers?: number }>(
      "GET",
      `/api/video/${encodeURIComponent(videoId)}/stats`,
      { signal: opts?.signal },
    );
  }

  async feed(tab: FeedTab, opts: FeedOpts = {}): Promise<VideoFeedItem[]> {
    const usp = new URLSearchParams({ mode: feedMode(tab) });
    if (opts.cursor !== undefined && opts.cursor !== null) usp.set("cursor", String(opts.cursor));
    if (opts.limit !== undefined) usp.set("limit", String(opts.limit));
    const envelope = await request<FeedEnvelope<VideoViewRow>>(
      "GET",
      `/api/videos/feed?${usp.toString()}`,
      { signal: opts.signal },
    );
    return (envelope.items ?? []).map(mapFeedRow);
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
