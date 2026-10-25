import {
  invokeFailure,
  platformFailure,
  tauriBridge,
  type PickedFile,
  type PlatformCapabilities,
  type PlatformPort,
  type PlatformResult,
  type PermissionState,
} from "./port";

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const step = 0x8000;
  for (let i = 0; i < bytes.length; i += step) {
    binary += String.fromCharCode(...bytes.subarray(i, i + step));
  }
  return btoa(binary);
}

interface PickedFileDto {
  name: string;
  size: number;
  type?: string;
  lastModified?: number;
  path: string;
}

export function createTauriPlatform(invoke: Invoke): PlatformPort {
  const shortHandlers = new Map<string, () => void>();
  let shortcutListenStarted = false;

  async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<PlatformResult<T>> {
    try {
      return { ok: true, value: (await invoke(cmd, args)) as T };
    } catch (error) {
      return invokeFailure(error);
    }
  }

  async function ensureShortcutListener(): Promise<void> {
    if (shortcutListenStarted) return;
    const listen = tauriBridge()?.event?.listen;
    if (!listen) return;
    shortcutListenStarted = true;
    await listen("platform://shortcut", (event) => {
      const payload = event.payload as { accelerator?: string } | undefined;
      if (!payload?.accelerator) return;
      shortHandlers.get(payload.accelerator)?.();
    });
  }

  const capabilities = (): PlatformCapabilities => ({
    kind: "tauri",
    window_controls: { supported: true },
    tray: { supported: true },
    autostart: { supported: true },
    global_shortcuts: { supported: true },
    notifications: { supported: true },
    clipboard: { supported: true },
    screenshot: { supported: true },
    files: { supported: true },
    secrets: { supported: true, storage: "os_keychain" },
  });

  return {
    capabilities,
    window: {
      minimize: () => call("platform_window", { action: "minimize" }).then(normalizeVoid),
      toggleMaximize: async () => {
        const result = await call<boolean>("platform_window", { action: "toggle_maximize" });
        return result.ok ? { ok: true, value: Boolean(result.value) } : result;
      },
      close: () => call("platform_window", { action: "close" }).then(normalizeVoid),
    },
    notifications: {
      permission: async () => {
        const result = await call<string>("platform_notification_state");
        return result.ok && isPermission(result.value) ? result.value : "unsupported";
      },
      request: async () => {
        const result = await call<string>("platform_notification_request");
        return result.ok && isPermission(result.value) ? result.value : "denied";
      },
      show: ({ title, body, silent }) =>
        call("platform_notify", { title, body, silent: Boolean(silent) }).then(normalizeVoid),
    },
    clipboard: {
      writeText: (text) => call("platform_clipboard_write", { text }).then(normalizeVoid),
    },
    files: {
      open: async ({ accept, multiple } = {}) => {
        const result = await call<{ files?: PickedFileDto[] }>("platform_pick_file", {
          accept: accept ?? null,
          multiple: Boolean(multiple),
        });
        if (!result.ok) return result;
        const files = result.value.files ?? [];
        if (!files.length) return platformFailure("CANCELLED", "未选择文件");
        return { ok: true, value: files.map(toDesktopFile) };
      },
      save: ({ suggestedName, mime, bytes }) =>
        call<{ path?: string }>("platform_save_file", {
          suggestedName,
          mime,
          base64: bytesToBase64(bytes),
        }).then((result) =>
          result.ok ? { ok: true as const, value: { path: result.value.path, via: "dialog" as const } } : result,
        ),
    },
    screenshot: {
      capture: async () => {
        const result = await call<{ base64?: string }>("platform_screenshot");
        if (!result.ok) return result;
        if (!result.value.base64) return platformFailure("FAILED", "截图返回为空");
        return { ok: true, value: { png: base64ToBytes(result.value.base64) } };
      },
    },
    autostart: {
      get: async () => {
        const result = await call<boolean>("platform_autostart_get");
        return result.ok ? { ok: true, value: Boolean(result.value) } : result;
      },
      set: async (enabled) => {
        const result = await call<boolean>("platform_autostart_set", { enabled });
        return result.ok ? { ok: true, value: Boolean(result.value) } : result;
      },
    },
    shortcuts: {
      register: async (accelerator, handler) => {
        const result = await call("platform_shortcut_register", { accelerator }).then(normalizeVoid);
        if (result.ok) {
          shortHandlers.set(accelerator, handler);
          await ensureShortcutListener().catch(() => undefined);
        }
        return result;
      },
      unregister: async (accelerator) => {
        const result = await call("platform_shortcut_unregister", { accelerator }).then(normalizeVoid);
        if (result.ok) shortHandlers.delete(accelerator);
        return result;
      },
    },
    tray: {
      setBadge: (count) => call("platform_tray_badge", { count }).then(normalizeVoid),
    },
    secrets: {
      get: async (key) => {
        const result = await call<{ value?: string | null }>("platform_secret_get", { key });
        return result.ok ? { ok: true, value: result.value.value ?? null } : result;
      },
      set: (key, value) => call("platform_secret_set", { key, value }).then(normalizeVoid),
      delete: (key) => call("platform_secret_delete", { key }).then(normalizeVoid),
    },
  };
}

function normalizeVoid<T>(result: PlatformResult<T>): PlatformResult<void> {
  return result.ok ? { ok: true, value: undefined } : result;
}

function isPermission(value: string): value is PermissionState {
  return value === "granted" || value === "denied" || value === "prompt" || value === "unsupported";
}

function toDesktopFile(dto: PickedFileDto): PickedFile {
  return {
    name: dto.name,
    size: dto.size,
    type: dto.type ?? "application/octet-stream",
    lastModified: dto.lastModified ?? Date.now(),
    path: dto.path,
    bytes: async () => {
      const result = await tauriBridge()!.core!.invoke!("platform_read_file_chunk", {
        path: dto.path,
        offset: 0,
        length: dto.size,
      });
      const payload = result as { base64: string };
      return base64ToBytes(payload.base64);
    },
    read: async (offset, length) => {
      const result = (await tauriBridge()!.core!.invoke!("platform_read_file_chunk", {
        path: dto.path,
        offset,
        length,
      })) as { base64: string };
      return base64ToBytes(result.base64);
    },
  };
}
