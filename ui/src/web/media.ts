/**
 * Media in the browser (A4): `fetch` + `Blob` replace the desktop's
 * base64-over-IPC commands.
 *
 * The webview still asks for `{base64, content_type}` (`api.ts`'s
 * `fetchMediaDataUrl` / `fetchVideoDataUrl` build a data URL from it), so the
 * answer keeps that shape — but the bytes now come from a browser fetch and
 * are cached as Blobs, so re-rendering an avatar or an image bubble does not
 * re-download it. A follow-up worth doing when `ui/src/api.ts` is in scope:
 * hand views `mediaUrl()` (a `blob:` URL) and drop the base64 hop entirely.
 */

import { asView, fetchBytes, postForm, sha256Hex } from "./session";

const CACHE_MAX = 60;
const cache = new Map<string, Blob>();

async function cached(path: string): Promise<Blob> {
  const hit = cache.get(path);
  if (hit) {
    // refresh recency without reallocating the map
    cache.delete(path);
    cache.set(path, hit);
    return hit;
  }
  const blob = await fetchBytes(path);
  cache.set(path, blob);
  if (cache.size > CACHE_MAX) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  return blob;
}

function toBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = String(reader.result);
      resolve(url.slice(url.indexOf(",") + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error("读取媒体失败"));
    reader.readAsDataURL(blob);
  });
}

export interface MediaBlob {
  base64: string;
  content_type: string;
}

/** `download_media` — `/api/media/{id}` needs no auth and serves thumbs too. */
export async function downloadMedia(mediaId: string, thumb: boolean): Promise<MediaBlob> {
  const blob = await cached(`/api/media/${encodeURIComponent(mediaId)}${thumb ? "?thumb=1" : ""}`);
  return { base64: await toBase64(blob), content_type: blob.type || "application/octet-stream" };
}

/** `download_video` — the same trick for video-service artifacts (mp4 / png). */
export async function downloadVideo(path: string): Promise<MediaBlob> {
  const ok = path.startsWith("/api/video/") && (path.endsWith("/play.mp4") || path.endsWith("/thumb.png")) && !path.includes("..");
  if (!ok) throw new Error("video path outside /api/video");
  const blob = await cached(path);
  return { base64: await toBase64(blob), content_type: blob.type || "application/octet-stream" };
}

export interface UploadReply {
  media_id: string;
  size: number;
  content_type: string;
  newly_stored: boolean;
  has_thumb: boolean;
}

/**
 * `upload_media` — field names are exactly what the platform accepts
 * (`bytes` + optional `sha256`; any other field is a hard 422). The digest is
 * computed here so the server's declared-vs-actual check has something to
 * verify against.
 */
export async function uploadMedia(
  bytes: Uint8Array,
  contentType: string,
  sha256?: string | null,
): Promise<UploadReply> {
  const form = new FormData();
  form.append("sha256", sha256 || (await sha256Hex(bytes)));
  form.append("bytes", new Blob([asView(bytes)], { type: contentType || "application/octet-stream" }), "blob");
  return postForm<UploadReply>("/api/media", form);
}

/** `upload_video` — video-service takes exactly one multipart field: `bytes`. */
export async function uploadVideo(bytes: Uint8Array): Promise<Record<string, unknown>> {
  const form = new FormData();
  form.append("bytes", new Blob([asView(bytes)], { type: "video/mp4" }), "video");
  return postForm<Record<string, unknown>>("/api/video", form);
}

/** `number[]` on the wire (Tauri cannot take a byte slice) back to bytes. */
export function toBytes(value: unknown): Uint8Array {
  if (value instanceof Uint8Array) return value;
  if (Array.isArray(value)) {
    const out = new Uint8Array(value.length);
    for (let i = 0; i < value.length; i++) {
      const n = Number(value[i]);
      if (!Number.isInteger(n) || n < 0 || n > 255) throw new Error("bytes 里有非 0..255 的值");
      out[i] = n;
    }
    return out;
  }
  throw new Error("缺少 bytes 参数");
}
