import { createTauriCore } from "./tauri-core";
import { createFixtureCore, type FixtureCore } from "./fixture/fixture-core";
import { createUnavailableCore } from "./unavailable-core";
import { tauriBridge } from "../platform/port";
import type { CoreHandle } from "../core-contract";

export type CoreSource = "tauri" | "fixture" | "wasm" | "unavailable";

export interface CoreSelection {
  core: CoreHandle;
  fixture: FixtureCore | null;
  source: CoreSource;
  note?: string;
}

/**
 * 选择核心适配器。
 *
 * - Tauri 宿主在：走 `core_dispatch` 桥；
 * - DEV / `VITE_CORE_FIXTURE=1`：走 fixture（仅界面开发，不作验收证据）；
 * - 生产 Web：动态加载 G01-03 的 wasm 包（`wxlike-core`）。
 *
 * 加载是异步的（要 `import()` + `init()` + 打开 IndexedDB），失败时回退
 * `UnavailableCore` 并把真实原因放进 `note`——不伪造离线能力。
 */
export async function selectCore(): Promise<CoreSelection> {
  const bridge = tauriBridge();
  if (bridge?.core?.invoke && import.meta.env.VITE_CORE_FIXTURE !== "1") {
    return { core: createTauriCore(bridge.core.invoke), fixture: null, source: "tauri" };
  }
  if (import.meta.env.DEV || import.meta.env.VITE_CORE_FIXTURE === "1") {
    const fixture = createFixtureCore({ latencyMs: 90 });
    return {
      core: fixture,
      fixture,
      source: "fixture",
      note: "开发数据（fixture）：不连服务端，不作为验收证据",
    };
  }
  try {
    const { createWasmCore } = await import("./wasm-core");
    const core = await createWasmCore();
    return {
      core,
      fixture: null,
      source: "wasm",
      note: "浏览器端 Rust WASM 核心（G01-03 产物 packages/wasm-client）",
    };
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return {
      core: createUnavailableCore(`WASM 核心加载失败：${reason}`),
      fixture: null,
      source: "unavailable",
      note: `Web 核心不可用：${reason}`,
    };
  }
}
