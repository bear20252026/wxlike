/**
 * Typed access to the Tauri bridge (`withGlobalTauri` injects window.__TAURI__).
 * The frontend NEVER opens HTTP/QUIC itself — everything is an invoke into the
 * Rust backend (task book §0.1.1).
 */

type Json = Record<string, unknown>;

interface TauriBridge {
  core: { invoke: (cmd: string, args?: Json) => Promise<unknown> };
  event: { listen: (event: string, handler: (e: { payload: unknown }) => void) => Promise<void> };
}

declare global {
  interface Window {
    __TAURI__: TauriBridge;
  }
}

export function bridge(): TauriBridge {
  const t = window.__TAURI__;
  if (!t) throw new Error("Tauri bridge unavailable (run inside the wxlike app)");
  return t;
}

export async function invoke<T = unknown>(cmd: string, args?: Json): Promise<T> {
  return bridge().core.invoke(cmd, args) as Promise<T>;
}

export function listen(event: string, handler: (payload: unknown) => void): Promise<void> {
  return bridge().event.listen(event, (e) => handler(e.payload));
}
