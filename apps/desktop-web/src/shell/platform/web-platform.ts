import {
  platformFailure,
  unsupported,
  type PickedFile,
  type PlatformCapabilities,
  type PlatformPort,
  type PlatformResult,
  type PermissionState,
} from "./port";

const SESSION_PREFIX = "wxlike.session.";

const memorySecrets = new Map<string, string>();

function sessionStore(): Storage | null {
  try {
    return globalThis.sessionStorage ?? null;
  } catch {
    return null;
  }
}

function notificationApi(): typeof Notification | null {
  const api = (globalThis as { Notification?: typeof Notification }).Notification;
  return typeof api === "function" ? api : null;
}

function permissionOf(api: typeof Notification | null): PermissionState {
  if (!api) return "unsupported";
  if (api.permission === "granted" || api.permission === "denied" || api.permission === "default") {
    return api.permission === "default" ? "prompt" : api.permission;
  }
  return "unsupported";
}

async function pickViaInput(accept?: string, multiple = false): Promise<PlatformResult<PickedFile[]>> {
  if (typeof document === "undefined") return unsupported("文件选择", "没有浏览器文档环境");
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = multiple;
    if (accept) input.accept = accept;
    let settled = false;
    const finish = (result: PlatformResult<PickedFile[]>) => {
      if (settled) return;
      settled = true;
      input.remove();
      resolve(result);
    };
    input.addEventListener("change", () => {
      const files = Array.from(input.files ?? []).map(toPickedFile);
      finish(files.length ? { ok: true, value: files } : platformFailure("CANCELLED", "未选择文件"));
    });
    input.addEventListener("cancel", () => finish(platformFailure("CANCELLED", "未选择文件")));
    input.style.display = "none";
    document.body.append(input);
    input.click();
  });
}

function toPickedFile(file: File): PickedFile {
  return {
    name: file.name,
    size: file.size,
    type: file.type,
    lastModified: file.lastModified,
    bytes: async () => new Uint8Array(await file.arrayBuffer()),
    read: async (offset, length) =>
      new Uint8Array(await file.slice(offset, offset + length).arrayBuffer()),
  };
}

type PickerWindow = Window & {
  showOpenFilePicker?: (options?: {
    multiple?: boolean;
    types?: { description: string; accept: Record<string, string[]> }[];
  }) => Promise<{ getFile: () => Promise<File> }[]>;
  showSaveFilePicker?: (options?: {
    suggestedName?: string;
    types?: { description: string; accept: Record<string, string[]> }[];
  }) => Promise<{ createWritable: () => Promise<{ write: (data: BufferSource) => Promise<void>; close: () => Promise<void> }> }>;
};

export function createWebPlatform(): PlatformPort {
  const api = notificationApi();
  const picker = globalThis as unknown as PickerWindow;

  const capabilities = (): PlatformCapabilities => ({
    kind: "web",
    window_controls: { supported: false, reason: "浏览器标签页由浏览器管理" },
    tray: { supported: false, reason: "浏览器没有系统托盘" },
    autostart: { supported: false, reason: "浏览器不能注册开机启动" },
    global_shortcuts: { supported: false, reason: "浏览器只能注册页面内快捷键（⌘K 已内建）" },
    notifications: api
      ? { supported: true }
      : { supported: false, reason: "当前浏览器不提供 Notification API" },
    clipboard: navigator.clipboard
      ? { supported: true }
      : { supported: false, reason: "需要 HTTPS 与用户手势；可手动选中复制" },
    screenshot: { supported: false, reason: "浏览器不提供系统级屏幕截图；改用文件选择" },
    files: {
      supported: Boolean(picker.showOpenFilePicker || document),
      reason: picker.showOpenFilePicker ? undefined : "使用 <input type=file> 与浏览器下载替代",
    },
    secrets: {
      supported: true,
      storage: "session_only",
      reason: "浏览器无法访问 OS 密钥库；自动登录使用独立的会话级策略",
    },
  });

  return {
    capabilities,
    window: {
      minimize: async () => unsupported("最小化", "请使用浏览器窗口按钮"),
      toggleMaximize: async () => unsupported("最大化", "请使用浏览器窗口按钮"),
      close: async () => unsupported("关闭窗口", "请关闭浏览器标签页"),
    },
    notifications: {
      permission: async () => permissionOf(api),
      request: async () => {
        if (!api) return "unsupported";
        try {
          return (await api.requestPermission()) === "granted" ? "granted" : "denied";
        } catch {
          return "denied";
        }
      },
      show: async ({ title, body, silent }) => {
        if (!api) return unsupported("系统通知", "请在应用内查看消息");
        if (api.permission !== "granted") return platformFailure("DENIED", "通知权限未授予");
        try {
          new api(title, { body, silent });
          return { ok: true, value: undefined };
        } catch (error) {
          return platformFailure("FAILED", error instanceof Error ? error.message : String(error));
        }
      },
    },
    clipboard: {
      writeText: async (text) => {
        if (!navigator.clipboard) return unsupported("剪贴板写入", "请手动选中复制");
        try {
          await navigator.clipboard.writeText(text);
          return { ok: true, value: undefined };
        } catch (error) {
          return platformFailure(
            "DENIED",
            error instanceof Error ? error.message : "剪贴板写入被拒绝",
          );
        }
      },
    },
    files: {
      open: async ({ accept, multiple } = {}) => {
        if (picker.showOpenFilePicker) {
          try {
            const handles = await picker.showOpenFilePicker({
              multiple,
              types: accept
                ? [{ description: "允许的文件", accept: acceptToTypes(accept) }]
                : undefined,
            });
            const files = await Promise.all(handles.map(async (h) => toPickedFile(await h.getFile())));
            return { ok: true, value: files };
          } catch (error) {
            if (error instanceof DOMException && error.name === "AbortError") {
              return platformFailure("CANCELLED", "未选择文件");
            }
            return pickViaInput(accept, multiple);
          }
        }
        return pickViaInput(accept, multiple);
      },
      save: async ({ suggestedName, mime, bytes }) => {
        if (picker.showSaveFilePicker) {
          try {
            const handle = await picker.showSaveFilePicker({
              suggestedName,
              types: [{ description: mime || "文件", accept: { [mime || "application/octet-stream"]: [`.${suggestedName.split(".").pop() || "bin"}`] } }],
            });
            const writable = await handle.createWritable();
            await writable.write(bytes as unknown as BufferSource);
            await writable.close();
            return { ok: true, value: { via: "dialog" } };
          } catch (error) {
            if (error instanceof DOMException && error.name === "AbortError") {
              return platformFailure("CANCELLED", "未保存文件");
            }
          }
        }
        if (typeof document === "undefined" || typeof URL.createObjectURL !== "function") {
          return unsupported("保存文件", "当前环境没有下载能力");
        }
        const blob = new Blob([bytes as unknown as BlobPart], { type: mime || "application/octet-stream" });
        const url = URL.createObjectURL(blob);
        const anchor = document.createElement("a");
        anchor.href = url;
        anchor.download = suggestedName;
        anchor.click();
        URL.revokeObjectURL(url);
        return { ok: true, value: { via: "browser_download" } };
      },
    },
    screenshot: {
      capture: async () => unsupported("屏幕截图", "请选择已有的图片文件"),
    },
    autostart: {
      get: async () => unsupported("开机启动", "浏览器不支持"),
      set: async () => unsupported("开机启动", "浏览器不支持"),
    },
    shortcuts: {
      register: async () => unsupported("全局快捷键", "页面内 ⌘/Ctrl+K 已内建"),
      unregister: async () => unsupported("全局快捷键", "页面内 ⌘/Ctrl+K 已内建"),
    },
    tray: {
      setBadge: async () => unsupported("托盘角标", "浏览器没有系统托盘"),
    },
    secrets: {
      get: async (key) => {
        const store = sessionStore();
        const value = store ? store.getItem(SESSION_PREFIX + key) : memorySecrets.get(key);
        return { ok: true, value: value ?? null };
      },
      set: async (key, value) => {
        const store = sessionStore();
        if (store) store.setItem(SESSION_PREFIX + key, value);
        else memorySecrets.set(key, value);
        return { ok: true, value: undefined };
      },
      delete: async (key) => {
        const store = sessionStore();
        if (store) store.removeItem(SESSION_PREFIX + key);
        else memorySecrets.delete(key);
        return { ok: true, value: undefined };
      },
    },
  };
}

function acceptToTypes(accept: string): Record<string, string[]> {
  const types: Record<string, string[]> = {};
  for (const entry of accept.split(",").map((part) => part.trim()).filter(Boolean)) {
    if (entry.startsWith(".")) {
      types["application/octet-stream"] = [...(types["application/octet-stream"] ?? []), entry];
    } else if (entry.includes("/")) {
      types[entry] = [];
    }
  }
  return types;
}
