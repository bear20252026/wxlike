/** Types for the files feature (G11-02). Mirrors media.upload.* / files.* / backup.* contract. */

export interface UploadReply {
  media_id: string;
  size: number;
  content_type: string;
  /** False when the object was already present (dedup hit). */
  newly_stored: boolean;
  has_thumb: boolean;
}

export type FileCategory = "all" | "image" | "document" | "audio" | "video" | "other";

export type FileSortKey = "newest" | "oldest" | "largest" | "name";

export interface FileRecord {
  /** Media object id (sha256 hex); the only address the server understands. */
  media_id: string;
  name: string;
  size: number;
  content_type: string;
  /** sha256 hex of the bytes — verified again on download. */
  digest: string;
  uploaded_at: number;
  /** Conversation the object was attached to, if any (member-scoped read). */
  conv?: string;
  visibility: "private" | "public";
}

export type FileAction = "rename" | "share" | "delete" | "cleanup";

export interface FileActionResult {
  media_id: string;
  action: FileAction;
  ok: boolean;
  detail?: string;
}

export interface FileQuery {
  keyword?: string;
  category?: FileCategory;
  sort?: FileSortKey;
}

export interface StorageUsage {
  totalBytes: number;
  fileCount: number;
  /** Backup/migration plan from the backup.* contract (G04). */
  plan?: BackupPlan;
}

export interface BackupPlan {
  plan_id: string;
  /** Bytes the plan expects to free/move. */
  plannedBytes: number;
  steps: Array<{ step_id: string; label: string; target_bytes: number }>;
  /** Present only after the plan ran: per-step completion evidence. */
  evidence?: Array<{ step_id: string; done_at: number; freed_bytes: number }>;
}

export interface FilesPorts {
  /** Content-addressed upload; retry with the same digest is safe (server dedup). */
  upload(file: File | Blob, meta: UploadMeta, opts?: UploadOpts): Promise<UploadReply>;
  /** When `expectedDigest` is given the bytes are re-hashed and verified. */
  download(mediaId: string, opts?: RequestOpts & { expectedDigest?: string }): Promise<Blob>;
  list(query?: FileQuery, opts?: RequestOpts): Promise<FileRecord[]>;
  usage(opts?: RequestOpts): Promise<StorageUsage>;
  rename(mediaId: string, name: string, opts?: RequestOpts): Promise<FileRecord>;
  share(mediaId: string, opts?: RequestOpts): Promise<{ url: string; expires_at: number }>;
  remove(mediaId: string, opts?: RequestOpts): Promise<void>;
  /** Batch operations return per-item results — 逐项返回结果, no all-or-nothing. */
  batchRemove(mediaIds: string[], opts?: RequestOpts): Promise<FileActionResult[]>;
  backupPlan(opts?: RequestOpts): Promise<BackupPlan>;
  backupExecute(planId: string, opts?: RequestOpts): Promise<BackupPlan>;
}

export interface UploadMeta {
  name: string;
  content_type: string;
  conv?: string;
  visibility?: "private" | "public";
}

export interface RequestOpts {
  signal?: AbortSignal;
  idempotencyKey?: string;
}

export interface UploadOpts extends RequestOpts {
  /** 0..1 progress callbacks; multiple calls per upload. */
  onProgress?: (fraction: number) => void;
  /**
   * Low-level sender, injectable for tests; the production implementation is
   * `xhrUpload` (XHR is the only browser API with upload progress).
   */
  send?: UploadSender;
}

export type UploadSender = (
  form: FormData,
  opts: { signal?: AbortSignal; onProgress?: (fraction: number) => void },
) => Promise<UploadReply>;

export function categoryOf(contentType: string, name: string): Exclude<FileCategory, "all"> {
  const lower = (contentType || "").toLowerCase();
  const ext = name.includes(".") ? (name.split(".").pop() ?? "").toLowerCase() : "";
  if (lower.startsWith("image/")) return "image";
  if (lower.startsWith("audio/")) return "audio";
  if (lower.startsWith("video/")) return "video";
  if (
    lower.startsWith("text/") ||
    lower.includes("pdf") ||
    lower.includes("word") ||
    lower.includes("excel") ||
    lower.includes("spreadsheet") ||
    lower.includes("zip") ||
    lower.includes("presentation") ||
    ["doc", "docx", "xls", "xlsx", "ppt", "pptx", "pdf", "txt", "md", "csv", "zip", "7z", "rar"].includes(ext)
  )
    return "document";
  return "other";
}
