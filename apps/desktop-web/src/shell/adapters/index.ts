import { createTauriCore } from "./tauri-core";
import { createFixtureCore, type FixtureCore } from "./fixture/fixture-core";
import { createUnavailableCore } from "./unavailable-core";
import { tauriBridge } from "../platform/port";
import type { CoreHandle } from "../core-contract";

export interface CoreSelection {
  core: CoreHandle;
  fixture: FixtureCore | null;
  source: "tauri" | "fixture" | "unavailable";
  note?: string;
}

export function selectCore(): CoreSelection {
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
  return {
    core: createUnavailableCore("浏览器端 Rust WASM 核心尚未接入（G01-03）；桌面请从 Tauri 宿主启动"),
    fixture: null,
    source: "unavailable",
    note: "Web 核心不可用：G01-03 未合入前不伪造离线能力",
  };
}
