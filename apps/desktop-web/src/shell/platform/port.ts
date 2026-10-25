/**
 * PlatformPort: the only route from React to OS capabilities (ADR-02).
 * Every method either performs the real OS call or reports `supported: false`.
 * There is deliberately no `notifySuccess()`-style helper.
 */

export type PlatformKind = "tauri" | "web";

export interface PlatformCapability {
  supported: boolean;
  /** Human-readable replacement or reason when unsupported. */
  reason?: string;
}

export interface PlatformCapabilities {
  kind: PlatformKind;
  window_controls: PlatformCapability;
  tray: PlatformCapability;
  autostart: PlatformCapability;
  global_shortcuts: PlatformCapability;
  notifications: PlatformCapability;
  clipboard: PlatformCapability;
  screenshot: PlatformCapability;
  files: PlatformCapability;
  secrets: PlatformCapability & { storage: "os_keychain" | "session_only" | "none" };
}

export type PermissionState = "granted" | "denied" | "prompt" | "unsupported";

export type PlatformFailure = {
  ok: false;
  code: "UNSUPPORTED" | "DENIED" | "CANCELLED" | "FAILED";
  message: string;
};

export type PlatformResult<T> = { ok: true; value: T } | PlatformFailure;

export interface PickedFile {
  name: string;
  size: number;
  type: string;
  lastModified: number;
  /** Present on desktop: lets uploaders stream without loading 1GB into JS heap. */
  path?: string;
  bytes: () => Promise<Uint8Array>;
  /** Optional ranged read used by the chunked uploader. */
  read?: (offset: number, length: number) => Promise<Uint8Array>;
}

export interface SaveFileInput {
  suggestedName: string;
  mime: string;
  bytes: Uint8Array;
}

export interface PlatformPort {
  capabilities(): PlatformCapabilities;
  window: {
    minimize(): Promise<PlatformResult<void>>;
    toggleMaximize(): Promise<PlatformResult<boolean>>;
    close(): Promise<PlatformResult<void>>;
  };
  notifications: {
    permission(): Promise<PermissionState>;
    request(): Promise<PermissionState>;
    show(input: { title: string; body: string; silent?: boolean }): Promise<PlatformResult<void>>;
  };
  clipboard: {
    writeText(text: string): Promise<PlatformResult<void>>;
  };
  files: {
    open(input?: { accept?: string; multiple?: boolean }): Promise<PlatformResult<PickedFile[]>>;
    save(input: SaveFileInput): Promise<PlatformResult<{ path?: string; via: "dialog" | "browser_download" }>>;
  };
  screenshot: {
    capture(): Promise<PlatformResult<{ png: Uint8Array }>>;
  };
  autostart: {
    get(): Promise<PlatformResult<boolean>>;
    set(enabled: boolean): Promise<PlatformResult<boolean>>;
  };
  shortcuts: {
    register(accelerator: string, handler: () => void): Promise<PlatformResult<void>>;
    unregister(accelerator: string): Promise<PlatformResult<void>>;
  };
  tray: {
    setBadge(count: number): Promise<PlatformResult<void>>;
  };
  secrets: {
    /** Persisted only where the platform has an OS-backed store. */
    get(key: string): Promise<PlatformResult<string | null>>;
    set(key: string, value: string): Promise<PlatformResult<void>>;
    delete(key: string): Promise<PlatformResult<void>>;
  };
}

export function platformFailure(
  code: PlatformFailure["code"],
  message: string,
): PlatformFailure {
  return { ok: false, code, message };
}

export function unsupported<T>(what: string, replacement: string): PlatformResult<T> {
  return platformFailure("UNSUPPORTED", `${what} 在当前平台不可用；${replacement}`);
}

/** Tauri exposes `window.__TAURI__.core.invoke` (withGlobalTauri). */
export interface TauriBridge {
  core?: { invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
  event?: {
    listen?: (event: string, handler: (e: { payload: unknown }) => void) => Promise<() => void>;
  };
}

export function tauriBridge(): TauriBridge | null {
  const bridge = (globalThis as { __TAURI__?: TauriBridge }).__TAURI__;
  return bridge?.core?.invoke ? bridge : null;
}

/** Normalises invoke errors without treating an unknown command as success. */
export function invokeFailure(error: unknown): PlatformFailure {
  const text = error instanceof Error ? error.message : String(error);
  if (/not found|unknown command|not allowed|command .* not found/i.test(text)) {
    return platformFailure("UNSUPPORTED", "宿主未挂载该命令（等待 G00/G01 集成）");
  }
  if (/denied|not permitted|permission/i.test(text)) {
    return platformFailure("DENIED", "系统权限被拒绝");
  }
  return platformFailure("FAILED", text.slice(0, 300));
}
