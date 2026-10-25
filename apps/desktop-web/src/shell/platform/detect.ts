import { createTauriPlatform } from "./tauri-platform";
import type { PlatformPort } from "./port";
import { createWebPlatform } from "./web-platform";
import { tauriBridge } from "./port";

export function detectPlatform(): PlatformPort {
  const bridge = tauriBridge();
  if (bridge?.core?.invoke) return createTauriPlatform(bridge.core.invoke);
  return createWebPlatform();
}
