/**
 * Video/live client (G11-03): video.* / live.* — D6 逐域退役后的形态。
 *
 * 控制面（meta/stats/view/feed 与 live 六条）不再自己读旧会话：`request()` 的
 * 默认出口在组合根装上核心会话后是"按域路由"的 transport，这些路径已被
 * `lib/http.ts` 的 CORE_ONLY_PREFIXES 拒绝走遗留 fetch，只能通过 video.* 与 live.*
 * action 经认证 Rust 核心出去（Bearer 不进 webview）。
 *
 * 两个面按服务端设计（crates/video/src/lib.rs，issue #7）区别对待：
 * - 播放面（play.mp4 / playlist.m3u8 / thumb.png / 分片）**匿名可用**：<video>
 *   与 HLS 播放器不会带 Authorization，故保留为相对 URL，不经核心也不带凭据。
 * - 上传（POST /api/video，multipart 二进制）**没有流式提供者**：显式阻塞
 *   （501），不回落旧会话、不伪造成功；页面渲染"上游未开通"。
 */

import { blockedError, idemKey, request } from "../../lib/http";
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

/** 匿名播放面：服务端按设计不鉴权（issue #7），返回相对 URL 供播放器直连。 */
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

export class VideoApi implements VideoPorts {
  async meta(videoId: string, opts?: RequestOpts): Promise<VideoMeta> {
    return request<VideoMeta>("GET", `/api/video/${encodeURIComponent(videoId)}`, {
      signal: opts?.signal,
    });
  }

  playbackUrls(videoId: string): PlaybackUrls {
    return playbackUrls(videoId);
  }

  /**
   * 上传契约面：multipart 二进制无流式提供者（桥不搬二进制），显式阻塞。
   * 旧实现用 XHR 读 localStorage bearer——那条出口已随 D6 退役拆除。
   */
  async uploadVideo(_file: File | Blob, _opts: UploadVideoOpts = {}): Promise<IngestedVideo> {
    throw blockedError("/api/video", "视频上传（multipart）尚无流式核心提供者");
  }

  async recordView(videoId: string, watchedMs: number, opts: ViewOpts = {}): Promise<void> {
    // viewer 缺省 "ui"：服务端点播面不鉴权、viewer 为自报标识（stats.rs）。
    // 不再从 localStorage 读身份（旧会话出口已退役）；调用方可显式传 viewer。
    const viewer = (opts.viewer ?? "ui").trim();
    await request<unknown>("POST", `/api/video/${encodeURIComponent(videoId)}/view`, {
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
      ...(reply.delivered === false ? {} : {}),
    };
  }
}