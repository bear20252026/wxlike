import type { ActionResponse, DomainError } from "../../shell/core-contract";
import { ACTIONS } from "../../shell/core-contract";
import type { PickedFile } from "../../shell/platform/port";

/** Matches the server-side budget promised on the login screen. */
export const MAX_UPLOAD_BYTES = 1024 * 1024 * 1024;

export class UploadError extends Error {
  readonly error: DomainError;
  constructor(error: DomainError) {
    super(error.message);
    this.name = "UploadError";
    this.error = error;
  }
}

export class UploadCancelled extends Error {
  constructor() {
    super("上传已取消");
    this.name = "UploadCancelled";
  }
}

export interface UploadProgress {
  uploadId: string;
  sent: number;
  total: number;
  chunkIndex: number;
  chunks: number;
}

export interface UploadedMedia {
  media_id: string;
  name: string;
  size: number;
  mime: string;
  digest: string;
  digest_algorithm: string;
}

type Dispatch = <T>(
  actionId: string,
  params: unknown,
  options?: { idempotencyKey?: string; silent?: boolean },
) => Promise<ActionResponse<T>>;

interface BeginResult {
  upload_id: string;
  chunk_size: number;
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/**
 * Chunk digest. WebCrypto has no streaming API, so each 4MB chunk is hashed and
 * the file digest is the SHA-256 of the chunk digests — never the whole file in
 * memory. `digest_algorithm` reports exactly what was computed.
 */
async function digestChunk(bytes: Uint8Array): Promise<{ hex: string; algorithm: string }> {
  const subtle = globalThis.crypto?.subtle;
  if (subtle) {
    const hash = await subtle.digest("SHA-256", bytes as unknown as BufferSource);
    return { hex: toHex(new Uint8Array(hash)), algorithm: "SHA-256/chunked" };
  }
  let a = 0x811c9dc5;
  let b = 0x01000193;
  for (const byte of bytes) {
    a = Math.imul(a ^ byte, 0x01000193) >>> 0;
    b = Math.imul(b + byte, 0x85ebca6b) >>> 0;
  }
  return { hex: `${a.toString(16).padStart(8, "0")}${b.toString(16).padStart(8, "0")}`, algorithm: "fnv1a-64/fallback" };
}

async function readRange(file: PickedFile, offset: number, length: number): Promise<Uint8Array> {
  if (file.read) return file.read(offset, length);
  const all = await file.bytes();
  return all.slice(offset, offset + length);
}

function ensure<T>(response: ActionResponse<T>): T {
  if (response.ok) return response.data;
  throw new UploadError(response.error);
}

export async function uploadFile(input: {
  file: PickedFile;
  dispatch: Dispatch;
  signal: AbortSignal;
  onProgress?: (progress: UploadProgress) => void;
  onUploadId?: (uploadId: string) => void;
}): Promise<UploadedMedia> {
  const { file, dispatch, signal, onProgress, onUploadId } = input;
  if (file.size <= 0) {
    throw new UploadError({
      code: "VALIDATION",
      message: "文件为空",
      retryable: false,
      request_id: "ui",
    });
  }
  if (file.size > MAX_UPLOAD_BYTES) {
    throw new UploadError({
      code: "VALIDATION",
      message: `单文件上限 1GB，当前 ${(file.size / 1024 / 1024 / 1024).toFixed(2)}GB`,
      retryable: false,
      request_id: "ui",
    });
  }

  const begin = ensure(
    await dispatch<BeginResult>(ACTIONS.mediaUploadBegin, {
      name: file.name,
      size: file.size,
      mime: file.type || "application/octet-stream",
      last_modified: file.lastModified,
    }),
  );
  onUploadId?.(begin.upload_id);

  const chunkSize = Math.max(64 * 1024, begin.chunk_size || 4 * 1024 * 1024);
  const chunks = Math.ceil(file.size / chunkSize);
  const digests: string[] = [];
  let sent = 0;

  try {
    for (let index = 0; index < chunks; index += 1) {
      if (signal.aborted) throw new UploadCancelled();
      const offset = index * chunkSize;
      const length = Math.min(chunkSize, file.size - offset);
      const bytes = await readRange(file, offset, length);
      const { hex } = await digestChunk(bytes);
      digests.push(hex);
      if (signal.aborted) throw new UploadCancelled();
      ensure(
        await dispatch(ACTIONS.mediaUploadPart, {
          upload_id: begin.upload_id,
          index,
          offset,
          length,
          sha256: hex,
        }, { idempotencyKey: `upload:${begin.upload_id}:${index}` }),
      );
      sent += length;
      onProgress?.({ uploadId: begin.upload_id, sent, total: file.size, chunkIndex: index, chunks });
    }

    if (signal.aborted) throw new UploadCancelled();

    const fileDigest = (await digestChunk(new TextEncoder().encode(digests.join(":")))).hex;
    const complete = ensure(
      await dispatch<{ media_id: string; name: string; size: number; mime: string }>(
        ACTIONS.mediaUploadComplete,
        { upload_id: begin.upload_id, digest: fileDigest, chunks, chunk_digests: digests },
        { idempotencyKey: `upload-complete:${begin.upload_id}` },
      ),
    );
    return {
      ...complete,
      digest: fileDigest,
      digest_algorithm: "SHA-256/chunked",
    };
  } catch (error) {
    if (error instanceof UploadCancelled || signal.aborted) {
      await dispatch(ACTIONS.mediaUploadCancel, { upload_id: begin.upload_id }, { silent: true, idempotencyKey: `upload-cancel:${begin.upload_id}` });
      throw new UploadCancelled();
    }
    // Cancellation is still required when a part fails: the server keeps the session otherwise.
    await dispatch(ACTIONS.mediaUploadCancel, { upload_id: begin.upload_id }, { silent: true, idempotencyKey: `upload-cancel:${begin.upload_id}` });
    throw error;
  }
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
