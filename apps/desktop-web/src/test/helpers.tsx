import { render } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { screen } from "@testing-library/react";
import { App } from "../app";
import { createFixtureCore, type FixtureCore } from "../shell/adapters/fixture/fixture-core";
import type { CoreHandle, ActionRequest } from "../shell/core-contract";
import type { PlatformCapabilities, PlatformPort } from "../shell/platform/port";
import { platformFailure, unsupported } from "../shell/platform/port";

export interface TestPlatformOptions {
  capabilities?: Partial<PlatformCapabilities>;
  overrides?: Partial<PlatformPort>;
}

export function createTestPlatform(options: TestPlatformOptions = {}): PlatformPort {
  const secrets = new Map<string, string>();
  const caps: PlatformCapabilities = {
    kind: "web",
    window_controls: { supported: false, reason: "测试平台不提供窗口控制" },
    tray: { supported: false, reason: "测试平台不提供托盘" },
    autostart: { supported: false, reason: "测试平台不提供开机启动" },
    global_shortcuts: { supported: false, reason: "测试平台不提供全局快捷键" },
    notifications: { supported: true },
    clipboard: { supported: true },
    screenshot: { supported: false, reason: "测试平台不提供截图" },
    files: { supported: true },
    secrets: { supported: true, storage: "session_only" },
    ...options.capabilities,
  };

  const base: PlatformPort = {
    capabilities: () => caps,
    window: {
      minimize: async () => unsupported("最小化", "测试平台无法最小化"),
      toggleMaximize: async () => unsupported("最大化", "测试平台无法最大化"),
      close: async () => unsupported("关闭", "测试平台无法关闭窗口"),
    },
    notifications: {
      permission: async () => "granted",
      request: async () => "granted",
      show: async () => ({ ok: true, value: undefined }),
    },
    clipboard: {
      writeText: async () => ({ ok: true, value: undefined }),
    },
    files: {
      open: async () => platformFailure("CANCELLED", "未选择文件"),
      save: async () => ({ ok: true, value: { via: "dialog" } }),
    },
    screenshot: {
      capture: async () => unsupported("截图", "测试平台无法截图"),
    },
    autostart: {
      get: async () => unsupported("开机启动", "测试平台不支持"),
      set: async () => unsupported("开机启动", "测试平台不支持"),
    },
    shortcuts: {
      register: async () => unsupported("全局快捷键", "测试平台不支持"),
      unregister: async () => unsupported("全局快捷键", "测试平台不支持"),
    },
    tray: {
      setBadge: async () => unsupported("托盘角标", "测试平台不支持"),
    },
    secrets: {
      get: async (key) => ({ ok: true, value: secrets.get(key) ?? null }),
      set: async (key, value) => {
        secrets.set(key, value);
        return { ok: true, value: undefined };
      },
      delete: async (key) => {
        secrets.delete(key);
        return { ok: true, value: undefined };
      },
    },
  };

  return { ...base, ...options.overrides, capabilities: () => caps };
}

export interface RecordingCore {
  core: CoreHandle;
  requests: ActionRequest[];
}

export function wrapCore(core: CoreHandle): RecordingCore {
  const requests: ActionRequest[] = [];
  return {
    requests,
    core: {
      dispatch: <T,>(request: ActionRequest) => {
        requests.push(request);
        return core.dispatch<T>(request);
      },
      subscribe: (cursor, handler) => core.subscribe(cursor, handler),
      capabilities: () => core.capabilities(),
    },
  };
}

export function renderApp(options: {
  hash?: string;
  fixture?: FixtureCore;
  platform?: PlatformPort;
  record?: boolean;
} = {}) {
  const fixture = options.fixture ?? createFixtureCore({ latencyMs: 0 });
  const platform = options.platform ?? createTestPlatform();
  const recording = options.record ? wrapCore(fixture) : null;
  window.location.hash = options.hash ?? "#/login";
  const view = render(
    <App
      selection={{ core: recording?.core ?? fixture, fixture, source: "fixture", note: "单元测试夹具（非验收证据）" }}
      platform={platform}
    />,
  );
  return { ...view, fixture, platform, requests: recording?.requests ?? [] };
}

export async function loginAs(userId = "user-alpha", password = "demo1234"): Promise<void> {
  await screen.findByTestId("login-username");
  const user = userEvent.setup();
  await user.type(screen.getByTestId("login-username"), userId);
  await user.type(screen.getByTestId("login-password"), password);
  await user.click(screen.getByTestId("login-submit"));
  await screen.findByTestId("rail-chats");
}

export async function openConversation(convId = "conv-lin"): Promise<void> {
  const user = userEvent.setup();
  const row = await screen.findByTestId(`conv-${convId}`);
  await user.click(row);
  await screen.findByTestId("composer-input");
}

export async function openSettingsTab(tab: string): Promise<void> {
  const user = userEvent.setup();
  await user.click(await screen.findByTestId(`settings-tab-${tab}`));
}
