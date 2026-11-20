import { afterEach, describe, expect, it, vi } from "vitest";

/**
 * selectCore 的选择逻辑：Tauri 宿主 > DEV/fixture > 生产 Web 的 wasm 核心。
 * wasm 加载用 mock 替身（本机没有 wasm-pack 产物），断言的是"选到谁"和
 * "失败时回退到谁"，不是 wasm 内部行为。
 */
const mocks = vi.hoisted(() => ({
  createWasmCore: vi.fn(),
  bridge: null as null | { core: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> } },
}));

vi.mock("./wasm-core", () => ({
  createWasmCore: mocks.createWasmCore,
}));

vi.mock("../platform/port", () => ({
  tauriBridge: () => mocks.bridge,
}));

import { selectCore } from "./index";

afterEach(() => {
  vi.unstubAllEnvs();
  mocks.createWasmCore.mockReset();
  mocks.bridge = null;
});

describe("selectCore", () => {
  it("生产 Web（非 DEV、非 fixture）加载 wasm 核心", async () => {
    vi.stubEnv("DEV", false);
    vi.stubEnv("VITE_CORE_FIXTURE", undefined);
    const core = { dispatch: vi.fn(), subscribe: vi.fn(), capabilities: vi.fn() };
    mocks.createWasmCore.mockResolvedValue(core);

    const selection = await selectCore();

    expect(selection.source).toBe("wasm");
    expect(selection.core).toBe(core);
    expect(selection.fixture).toBeNull();
    expect(mocks.createWasmCore).toHaveBeenCalledTimes(1);
  });

  it("DEV 仍走 fixture，不碰 wasm", async () => {
    vi.stubEnv("DEV", true);
    vi.stubEnv("VITE_CORE_FIXTURE", undefined);

    const selection = await selectCore();

    expect(selection.source).toBe("fixture");
    expect(selection.fixture).not.toBeNull();
    expect(mocks.createWasmCore).not.toHaveBeenCalled();
  });

  it("VITE_CORE_FIXTURE=1 强制 fixture（即便生产构建）", async () => {
    vi.stubEnv("DEV", false);
    vi.stubEnv("VITE_CORE_FIXTURE", "1");

    const selection = await selectCore();

    expect(selection.source).toBe("fixture");
    expect(selection.fixture).not.toBeNull();
    expect(mocks.createWasmCore).not.toHaveBeenCalled();
  });

  it("Tauri 宿主在且未强制 fixture 时走 tauri", async () => {
    vi.stubEnv("DEV", false);
    vi.stubEnv("VITE_CORE_FIXTURE", undefined);
    mocks.bridge = { core: { invoke: vi.fn(async () => ({ ok: true, data: null })) } };

    const selection = await selectCore();

    expect(selection.source).toBe("tauri");
    expect(mocks.createWasmCore).not.toHaveBeenCalled();
  });

  it("wasm 加载失败回退 unavailable，note 带可诊断原因", async () => {
    vi.stubEnv("DEV", false);
    vi.stubEnv("VITE_CORE_FIXTURE", undefined);
    mocks.createWasmCore.mockRejectedValue(new Error("wxlike-core 产物缺失"));

    const selection = await selectCore();

    expect(selection.source).toBe("unavailable");
    expect(selection.note).toContain("wxlike-core 产物缺失");
    const caps = await selection.core.capabilities();
    expect(caps.transport).toBe("wasm");
    expect(caps.notes?.join(" ")).toContain("wxlike-core 产物缺失");
  });
});
