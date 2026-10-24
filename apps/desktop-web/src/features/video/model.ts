/** Types for the video/live feature (G11-03). Mirrors crates/video + crates/platform/live.rs. */

export interface VideoMeta {
  video_id: string;
  source_size: number;
  duration_ms: number;
  width?: number;
  height?: number;
  video_codec?: string;
  audio_codec?: string;
}

export interface PlaybackUrls {
  mp4: string;
  hls: string;
  thumb: string;
}

/** G05 ContentPort contract shape for the channels feed (upstream pending). */
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

export type FeedTab = "following" | "trending";

export interface LiveState {
  channel: string;
  live: boolean;
  viewers?: number;
  stream_key?: string;
  started_at?: number;
  recording?: boolean;
}

export interface LiveStartReply {
  stream_key: string;
  ingest_url: string;
  ingest_token: string;
  expires_in_ms?: number;
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
  uploadVideo(file: File | Blob, opts?: UploadVideoOpts): Promise<VideoMeta>;
  recordView(videoId: string, opts?: RequestOpts): Promise<void>;
  stats(videoId: string, opts?: RequestOpts): Promise<{ views?: number; viewers?: number }>;
  feed(tab: FeedTab, opts?: RequestOpts): Promise<VideoFeedItem[]>;
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
