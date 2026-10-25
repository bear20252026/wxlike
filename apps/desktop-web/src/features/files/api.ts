/**
 * Files domain client (G11-02): files.* / media.upload.* / backup.*.
 *
 * Real today (server `crates/platform/src/media.rs`):
 *   - POST /api/media  — multipart upload, `sha256` declared and verified
 *   - GET  /api/media/{id} — authorized download with Range support
 *   - GET  /api/media/{id}/url — short-lived signed URL (preview)
 * Contract-gated (G04 MediaPort still open; the card allows fixture
 * development until the upstream merges): /files listing metadata, rename,
 * share references, delete, /backups plan+execute. Those raise a typed
 * error the page renders as "上游未开通" instead of fake success.
 *
 * Resume semantics: the object store is content-addressed, so a retried
 * upload with the same declared digest cannot create a second object —
 * interrupted transfers resume by re-sending from the last confirmed byte
 * once G04's chunked `/uploads` lands; until then whole-object retry with
 * digest dedup is the honest equivalent.
 */

import { ApiError, baseUrl, idemKey, request, token } from "../../lib/http";
import { sha256Hex } from "./bytes";
import type {
  BackupPlan,
  FileActionResult,
  FileQuery,
  FileRecord,
  FilesPorts,
  RequestOpts,
  StorageUsage,
  UploadMeta,
  UploadOpts,
  UploadReply,
} from "./model";
import { categoryOf } from "./model";

/**
 * Production uploader. XHR because fetch has no upload progress. Resolves on
 * 2xx, rejects with `ApiError` using the same error contract as `http.ts`.
 */
export const xhrUpload = async (
  form: FormData,
  opts: { signal?: AbortSignal; onProgress?: (fraction: number) => void } = {},
): Promise<UploadReply> => {
  return new Promise<UploadReply>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `${baseUrl()}/api/media`);
    const tok = token();
    if (tok) xhr.setRequestHeader("authorization", `Bearer ${tok}`);
    xhr.responseType = "json";
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable) opts.onProgress?.(event.loaded / event.total);
    };
    xhr.onload = () => {
      const body = xhr.response as UploadReply | { error?: string } | null;
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve(body as UploadReply);
      } else {
        const message =
          body && typeof body === "object" && "error" in body && body.error
            ? String(body.error)
            : `上传失败 (${xhr.status})`;
        reject(new ApiError(xhr.status, message, "/api/media"));
      }
    };
    xhr.onerror = () => reject(new ApiError(0, "网络错误：上传中断", "/api/media", "network"));
    xhr.onabort = () => reject(new ApiError(0, "已取消", "/api/media", "cancelled"));
    opts.signal?.addEventListener("abort", () => xhr.abort(), { once: true });
    xhr.send(form);
  });
};

export class FilesApi implements FilesPorts {
  async upload(file: File | Blob, meta: UploadMeta, opts: UploadOpts = {}): Promise<UploadReply> {
    // Digest first: the server verifies the declared sha256, and a retry with
    // the same digest lands on the same object (dedup), never a duplicate.
    const bytes = await blobToBytes(file);
    const digest = await sha256Hex(bytes);
    const form = new FormData();
    form.append("file", file, meta.name);
    form.append("sha256", digest);
    if (meta.conv) form.append("conv", meta.conv);
    if (meta.visibility) form.append("visibility", meta.visibility);
    const send = opts.send ?? xhrUpload;
    return send(form, { signal: opts.signal, onProgress: opts.onProgress });
  }

  /**
   * Download and verify: when the caller supplies the record's digest, bytes
   * must hash to it (下载与源 hash 一致) or the promise rejects. Bytes are
   * taken straight off the Response — no Blob round-trip, which is realm-safe
   * under jsdom and avoids re-copying in the browser.
   */
  async download(mediaId: string, opts: RequestOpts & { expectedDigest?: string } = {}): Promise<Blob> {
    const tok = token();
    const res = await fetch(`${baseUrl()}/api/media/${encodeURIComponent(mediaId)}`, {
      headers: tok ? { authorization: `Bearer ${tok}` } : {},
      signal: opts?.signal,
    });
    if (!res.ok) {
      const text = await res.text().catch(() => "");
      let message = text.trim();
      try {
        const parsed = JSON.parse(text) as { error?: string };
        if (parsed.error) message = parsed.error;
      } catch {
        // plain-text error body (server route family)
      }
      throw new ApiError(res.status, message || `下载失败 (${res.status})`, `/api/media/${mediaId}`);
    }
    const bytes = new Uint8Array(await res.arrayBuffer());
    if (opts.expectedDigest) {
      const digest = await sha256Hex(bytes);
      if (digest !== opts.expectedDigest.toLowerCase()) {
        throw new ApiError(0, "下载校验失败：sha256 与源不一致", `/api/media/${mediaId}`, "server");
      }
    }
    return new Blob([bytes], { type: res.headers.get("content-type") ?? "application/octet-stream" });
  }

  /** Raw bytes for direct callers (tests, hash checks). */
  async downloadBytes(mediaId: string, opts?: RequestOpts): Promise<Uint8Array> {
    const tok = token();
    const res = await fetch(`${baseUrl()}/api/media/${encodeURIComponent(mediaId)}`, {
      headers: tok ? { authorization: `Bearer ${tok}` } : {},
      signal: opts?.signal,
    });
    if (!res.ok) {
      const text = await res.text().catch(() => "");
      let message = text.trim();
      try {
        const parsed = JSON.parse(text) as { error?: string };
        if (parsed.error) message = parsed.error;
      } catch {
        // plain-text error body (server route family)
      }
      throw new ApiError(res.status, message || `下载失败 (${res.status})`, `/api/media/${mediaId}`);
    }
    return new Uint8Array(await res.arrayBuffer());
  }

  /**
   * G04 /files contract. The current server stores content-addressed blobs
   * without a listing; when the endpoint is absent we fall back to the local
   * upload journal so the page reflects what this client really uploaded.
   */
  async list(_query: FileQuery = {}, _opts?: RequestOpts): Promise<FileRecord[]> {
    try {
      return await request<FileRecord[]>("GET", "/api/files");
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        throw err; // page renders the explicit upstream state + local journal
      }
      throw err;
    }
  }

  async usage(_opts?: RequestOpts): Promise<StorageUsage> {
    return request<StorageUsage>("GET", "/api/files/usage");
  }

  async rename(mediaId: string, name: string, opts?: RequestOpts): Promise<FileRecord> {
    return request<FileRecord>("PATCH", `/api/files/${encodeURIComponent(mediaId)}`, {
      body: { name },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("file-rename"),
      signal: opts?.signal,
    });
  }

  async share(mediaId: string, opts?: RequestOpts): Promise<{ url: string; expires_at: number }> {
    return request<{ url: string; expires_at: number }>(
      "POST",
      `/api/files/${encodeURIComponent(mediaId)}/share`,
      {
        idempotencyKey: opts?.idempotencyKey ?? idemKey("file-share"),
        signal: opts?.signal,
      },
    );
  }

  async remove(mediaId: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("DELETE", `/api/files/${encodeURIComponent(mediaId)}`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("file-remove"),
      signal: opts?.signal,
    });
  }

  /** Batch: server answers per item; one item's failure never hides the rest. */
  async batchRemove(mediaIds: string[], opts?: RequestOpts): Promise<FileActionResult[]> {
    return request<FileActionResult[]>("POST", "/api/files/batch-remove", {
      body: { media_ids: mediaIds },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("file-batch-remove"),
      signal: opts?.signal,
    });
  }

  async backupPlan(_opts?: RequestOpts): Promise<BackupPlan> {
    return request<BackupPlan>("GET", "/api/backups/plan");
  }

  async backupExecute(planId: string, opts?: RequestOpts): Promise<BackupPlan> {
    return request<BackupPlan>("POST", "/api/backups/plan/execute", {
      body: { plan_id: planId },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("backup-execute"),
      signal: opts?.signal,
    });
  }
}

/**
 * Blob → bytes without `Blob.arrayBuffer`, which some realms (jsdom) lack;
 * FileReader is universal and stays on one copy.
 */
async function blobToBytes(blob: Blob): Promise<Uint8Array> {
  const withAB = blob as Blob & { arrayBuffer?: () => Promise<ArrayBuffer> };
  if (typeof withAB.arrayBuffer === "function") {
    return new Uint8Array(await withAB.arrayBuffer());
  }
  return new Uint8Array(
    await new Promise<ArrayBuffer>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(reader.result as ArrayBuffer);
      reader.onerror = () => reject(reader.error ?? new Error("读取文件失败"));
      reader.readAsArrayBuffer(blob);
    }),
  );
}

/** Local upload journal: real records of what this client uploaded. */
const JOURNAL_KEY = "wxlike-file-journal";

export function journalAll(): FileRecord[] {
  try {
    const raw = localStorage.getItem(JOURNAL_KEY);
    return raw ? (JSON.parse(raw) as FileRecord[]) : [];
  } catch {
    return [];
  }
}

export function journalAdd(record: FileRecord): void {
  const all = journalAll().filter((item) => item.media_id !== record.media_id);
  all.unshift(record);
  try {
    localStorage.setItem(JOURNAL_KEY, JSON.stringify(all.slice(0, 200)));
  } catch {
    // storage full — journal is a cache, not the source of truth
  }
}

export function journalRemove(mediaId: string): void {
  try {
    localStorage.setItem(
      JOURNAL_KEY,
      JSON.stringify(journalAll().filter((item) => item.media_id !== mediaId)),
    );
  } catch {
    // ignore
  }
}

export function recordFromUpload(
  reply: UploadReply,
  meta: UploadMeta,
  digest: string,
): FileRecord {
  return {
    media_id: reply.media_id,
    name: meta.name,
    size: reply.size,
    content_type: reply.content_type,
    digest,
    uploaded_at: Date.now(),
    conv: meta.conv,
    visibility: meta.visibility ?? "private",
  };
}

export function applyFileQuery(records: FileRecord[], query: FileQuery): FileRecord[] {
  const keyword = query.keyword?.trim().toLowerCase() ?? "";
  const category = query.category ?? "all";
  const sort = query.sort ?? "newest";
  let out = records.filter((record) => {
    if (category !== "all" && categoryOf(record.content_type, record.name) !== category) return false;
    if (!keyword) return true;
    return record.name.toLowerCase().includes(keyword) || record.media_id.includes(keyword);
  });
  out = [...out].sort((a, b) => {
    if (sort === "oldest") return a.uploaded_at - b.uploaded_at;
    if (sort === "largest") return b.size - a.size;
    if (sort === "name") return a.name.localeCompare(b.name);
    return b.uploaded_at - a.uploaded_at;
  });
  return out;
}
