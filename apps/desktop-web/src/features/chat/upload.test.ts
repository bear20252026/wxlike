import { describe, expect, it, vi } from "vitest";
import { ACTIONS, ok, type ActionResponse } from "../../shell/core-contract";
import type { PickedFile } from "../../shell/platform/port";
import { MAX_UPLOAD_BYTES, UploadCancelled, UploadError, uploadFile } from "./upload";

type Dispatch = Parameters<typeof uploadFile>[0]["dispatch"];

function fileOf(bytes: Uint8Array, name = "sample.bin"): PickedFile {
  return {
    name,
    size: bytes.byteLength,
    type: "application/octet-stream",
    lastModified: 1,
    read: async (offset, length) => bytes.slice(offset, offset + length),
    bytes: async () => bytes,
  };
}

describe("G10-02 分块上传", () => {
  it("按服务端分块逐段读取，part 与 complete 都带幂等键", async () => {
    const bytes = new Uint8Array(160_000);
    const calls: { action: string; params: Record<string, unknown>; key?: string }[] = [];
    const offsets: number[] = [];
    const file: PickedFile = {
      ...fileOf(bytes),
      read: async (offset, length) => {
        offsets.push(offset);
        return bytes.slice(offset, offset + length);
      },
    };
    const dispatch = vi.fn(async (action: string, params: unknown, options?: { idempotencyKey?: string }): Promise<ActionResponse<unknown>> => {
      calls.push({ action, params: params as Record<string, unknown>, key: options?.idempotencyKey });
      if (action === ACTIONS.mediaUploadBegin) return ok({ upload_id: "up-1", chunk_size: 65_536 });
      if (action === ACTIONS.mediaUploadPart) return ok({ received: (params as { length: number }).length });
      if (action === ACTIONS.mediaUploadComplete) {
        return ok({ media_id: "media-1", name: file.name, size: file.size, mime: file.type });
      }
      return ok({});
    });

    const media = await uploadFile({ file, dispatch: dispatch as unknown as Dispatch, signal: new AbortController().signal });

    expect(media.media_id).toBe("media-1");
    expect(media.digest_algorithm).toMatch(/SHA-256\/chunked|fnv1a-64\/fallback/);
    expect(offsets).toEqual([0, 65_536, 131_072]);
    const parts = calls.filter((call) => call.action === ACTIONS.mediaUploadPart);
    expect(parts).toHaveLength(3);
    expect(parts.map((call) => call.key)).toEqual(["upload:up-1:0", "upload:up-1:1", "upload:up-1:2"]);
    expect(calls.find((call) => call.action === ACTIONS.mediaUploadComplete)?.key).toBe("upload-complete:up-1");
  });

  it("取消正在进行的上传会通知核心撤销上传会话", async () => {
    const gate: { release?: () => void } = {};
    let started = false;
    const actions: string[] = [];
    const dispatch = vi.fn(async (action: string): Promise<ActionResponse<unknown>> => {
      actions.push(action);
      if (action === ACTIONS.mediaUploadBegin) return ok({ upload_id: "up-2", chunk_size: 4 });
      if (action === ACTIONS.mediaUploadPart) {
        started = true;
        return new Promise<ActionResponse<unknown>>((resolve) => {
          gate.release = () => resolve(ok({ received: 4 }));
        });
      }
      return ok({});
    });

    const controller = new AbortController();
    const pending = uploadFile({ file: fileOf(new Uint8Array(8)), dispatch: dispatch as unknown as Dispatch, signal: controller.signal });
    await vi.waitFor(() => expect(started).toBe(true));
    controller.abort();
    gate.release?.();

    await expect(pending).rejects.toBeInstanceOf(UploadCancelled);
    expect(actions).toContain(ACTIONS.mediaUploadCancel);
    expect(actions).not.toContain(ACTIONS.mediaUploadComplete);
  });

  it("超过 1GB 的文件不会发起上传", async () => {
    const dispatch = vi.fn(async (): Promise<ActionResponse<unknown>> => ok({}));
    const oversized: PickedFile = {
      ...fileOf(new Uint8Array(0), "oversized.bin"),
      size: MAX_UPLOAD_BYTES + 1,
    };

    await expect(
      uploadFile({ file: oversized, dispatch: dispatch as unknown as Dispatch, signal: new AbortController().signal }),
    ).rejects.toBeInstanceOf(UploadError);
    expect(dispatch).not.toHaveBeenCalled();
  });
});
