/** Types for the video/live feature. Mirrors crates/video + crates/platform/{video_feed,live}.rs. */

export interface Rendition {
  height: number;
  width: number;
  /** Measured bitrate = mp4 bytes × 8 / duration (master playlist BANDWIDTH). */
  bandwidth?: number;
}

export interface VideoMeta {
  video_id: string;
  source_size: number;
  duration_ms: number;
  width?: number;
  height?: number;
  video_codec?: string;
  audio_codec?: string;
  /** Transcode parameters actually used (highest rendition). */
  transcode?: Record<string, unknown>;
  /** Multi-resolution renditions (absent on old meta.json). */
  renditions?: Rendition[];
}

/**
 * POST /api/video (upload) reply — crates/video `Ingested`. The URLs are
 * server-issued relative paths: prefer them over the derived playbackUrls().
 */
export interface IngestedVideo {
  video_id: string;
  playlist_url: string;
  /** Progressive MP4 remux — browsers' native <video> cannot read HLS. */
  play_url: string;
  thumb_url: string;
  duration_ms: number;
  width: number;
  height: number;
  renditions: Rendition[];
}

export interface PlaybackUrls {
  mp4: string;
  hls: string;
  thumb: string;
}

/**
 * Channels feed item as the UI consumes it. The server's `VideoView`
 * (GET /api/videos/feed) is mapped onto this shape in api.ts:
 * id→video_id, caption→title, like_count→likes, created_at→published_at,
 * liked→followed (the per-viewer interaction flag), thumb_url→thumb.
 */
export interface VideoFeedItem {
  video_id: string;
  author: string;
  author_name: string;
  title: string;
  thumb?: string;
  views?: number;
  likes?: number;
  followed?: boolean;
  published_at: number;
}

/** Raw server row for GET /api/videos/feed items (crates/platform VideoView). */
export interface VideoViewRow {
  id: string;
  author: string;
  author_name: string;
  caption: string;
  state: string;
  created_at: number;
  like_count: number;
  comment_count: number;
  liked: boolean;
  live: boolean;
  playable: boolean;
  playlist_url: string;
  play_url: string;
  thumb_url: string;
  duration_ms: number;
  reason?: string;
}

/** Listing envelope shared by the platform's list endpoints. */
export interface FeedEnvelope<T> {
  items: T[];
  next_cursor: string | number | null;
  has_more: boolean;
  snapshot_version?: number;
}

export type FeedTab = "following" | "trending";

export interface LiveState {
  channel: string;
  live: boolean;
  viewers?: number;
  stream_key?: string;
  started_at?: number;
  recording?: boolean;
}

/** The live session object nested inside the start reply (crates/platform LiveSession). */
export interface LiveSessionInfo {
  id: string;
  channel_id: string;
  host: string;
  stream_key: string;
  state: string;
  recording: boolean;
  started_at: number;
  ended_at?: number | null;
  viewers: number;
  peak_viewers?: number;
}

/** POST /api/live/{channel}/start reply — stream_key lives INSIDE session. */
export interface LiveStartReply {
  session: LiveSessionInfo;
  ingest_url: string;
  ingest_token: string;
}

export interface DanmakuMessage {
  id: string;
  author: string;
  text: string;
  at: number;
  self?: boolean;
}

export interface RequestOpts {
  signal?: AbortSignal;
  idempotencyKey?: string;
}

export interface VideoPorts {
  meta(videoId: string, opts?: RequestOpts): Promise<VideoMeta>;
  playbackUrls(videoId: string): PlaybackUrls;
  uploadVideo(file: File | Blob, opts?: UploadVideoOpts): Promise<IngestedVideo>;
  recordView(videoId: string, watchedMs: number, opts?: ViewOpts): Promise<void>;
  stats(videoId: string, opts?: RequestOpts): Promise<{ views?: number; viewers?: number }>;
  feed(tab: FeedTab, opts?: FeedOpts): Promise<VideoFeedItem[]>;
  liveState(channel: string, opts?: RequestOpts): Promise<LiveState>;
  liveStart(channel: string, recording: boolean, opts?: RequestOpts): Promise<LiveStartReply>;
  liveStop(channel: string, opts?: RequestOpts): Promise<void>;
  liveJoin(channel: string, opts?: RequestOpts): Promise<{ viewers: number }>;
  liveLeave(channel: string, opts?: RequestOpts): Promise<void>;
  sendDanmaku(channel: string, text: string, opts?: RequestOpts): Promise<DanmakuMessage>;
}

export interface UploadVideoOpts extends RequestOpts {
  onProgress?: (fraction: number) => void;
}

/** POST /api/video/{id}/view requires a JSON body {viewer, watched_ms}. */
export interface ViewOpts extends RequestOpts {
  /** Self-reported viewer identity; defaults to the stored session user id. */
  viewer?: string;
}

export interface FeedOpts extends RequestOpts {
  /** Opaque cursor from a previous page (`next_cursor` in the envelope). */
  cursor?: string | number | null;
  limit?: number;
}
