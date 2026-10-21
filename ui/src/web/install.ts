/**
 * Installs the browser bridge before anything else runs.
 *
 * This module's body is the install: `ui/src/main.ts` calls `invoke("local_autopen")`
 * while it is being evaluated, so the bridge has to exist first. Static import
 * order gives us that for free (this module, then `main`), which is why the
 * entry file is two imports and not an `await`.
 */

import { installWebBridge, prepareWeb } from "./transport";

declare const __VERSION__: string | undefined;

declare global {
  interface Window {
    __wxlikeWebVersion?: string;
  }
}

installWebBridge();

// A5 injects `__VERSION__` at build time; a dev bundle without it stays honest.
const version = typeof __VERSION__ === "string" ? __VERSION__ : "dev";
window.__wxlikeWebVersion = version;
console.info(`[wxlike web] ${version} · transport = HTTP + /ws`);

void prepareWeb();
