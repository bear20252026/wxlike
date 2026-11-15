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

import { ApiError, blockedError, idemKey, request } from "../../lib/http";
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

export class FilesApi implements FilesPorts {
  /**
   * 二进制上传（multipart）没有 JSON 流式核心提供者：显式阻塞（同视频域上传
   * 口径，#294）。G04 的分片上传通道（/api/v1/uploads*）已在服务端落地，但把
   * 字节流搬进 CoreHandle 需要独立的流式 action 契约，不在本控制面切片里伪造。
   * 注入式 send（测试夹具）仍然优先，便于用例驱动进度/取消语义。
   */
  async upload(_file: File | Blob, _meta: UploadMeta, opts: UploadOpts = {}): Promise<UploadReply> {
    if (opts.send) {
      const form = new FormData();
      return opts.send(form, { signal: opts.signal, onProgress: opts.onProgress });
    }
    throw blockedError("/api/media", "文件上传（multipart 二进制）尚无流式核心提供者");
  }

  /**
   * 二进制下载（Range 流）同样没有 JSON 流式核心提供者：显式阻塞。签名 URL
   * 读面（/api/media/{id}/url）归内容域 moment.media_url，可经核心取短期链接；
   * 整对象字节流下载待流式 action 契约落地，不在本切片伪造成功。
   */
  async download(mediaId: string, _opts: RequestOpts & { expectedDigest?: string } = {}): Promise<Blob> {
    throw blockedError(`/api/media/${mediaId}`, "文件下载（二进制流）尚无流式核心提供者");
  }

  /** Raw bytes for direct callers (tests, hash checks). */
  async downloadBytes(mediaId: string, _opts?: RequestOpts): Promise<Uint8Array> {
    throw blockedError(`/api/media/${mediaId}`, "文件下载（二进制流）尚无流式核心提供者");
  }

  /**
   * 文件列表：经核心路由 file.list 打到 G04 /api/v1/files，投影回 FileRecord[]。
   * 服务端不可达/未授权时如实抛错，页面据此呈现明确状态 + 本端真实上传日志，
   * 不伪造一份空目录当成功。
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

  /**
   * 用量：G04 没有独立的 usage 端点，由文件列表客户端派生（总字节 + 计数）。
   * 列表经核心路由（file.list），所以这份用量也来自认证会话，不读旧 localStorage。
   */
  async usage(_opts?: RequestOpts): Promise<StorageUsage> {
    const records = await this.list();
    return {
      totalBytes: records.reduce((sum, record) => sum + record.size, 0),
      fileCount: records.length,
    };
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

  /**
   * 批量删除：G04 没有 batch-remove 端点，逐项经核心删除（file.remove）并汇总
   * 逐项结果——一项失败不掩盖其余（与原契约的逐项语义一致）。每项独立幂等键。
   */
  async batchRemove(mediaIds: string[], opts?: RequestOpts): Promise<FileActionResult[]> {
    const results: FileActionResult[] = [];
    for (const mediaId of mediaIds) {
      try {
        await this.remove(mediaId, opts);
        results.push({ media_id: mediaId, action: "delete", ok: true });
      } catch (err) {
        results.push({
          media_id: mediaId,
          action: "delete",
          ok: false,
          detail: err instanceof Error ? err.message : String(err),
        });
      }
    }
    return results;
  }

  /**
   * 备份/清理计划：G04 服务端是导出包计划（BackupPlan：entries/payload_bytes/
   * publishable…），与前端清理契约（plan_id/steps/evidence）形状未对齐，没有
   * 可诚实投影的提供者：显式阻塞（#294），不伪造一份看起来像清理计划的数据。
   */
  async backupPlan(_opts?: RequestOpts): Promise<BackupPlan> {
    throw blockedError("/api/backups/plan", "备份/清理计划契约尚未与服务端导出包计划对齐");
  }

  async backupExecute(_planId: string, _opts?: RequestOpts): Promise<BackupPlan> {
    throw blockedError("/api/backups/plan/execute", "备份/清理执行契约尚未与服务端导出包计划对齐");
  }
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
