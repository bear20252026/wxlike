import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

const here = path.dirname(fileURLToPath(import.meta.url));

/** Git sha for build provenance; empty when git is unavailable (fallback then). */
function gitSha(): string {
  try {
    return execSync("git rev-parse --short HEAD", { cwd: here, stdio: ["ignore", "pipe", "ignore"] })
      .toString()
      .trim();
  } catch {
    return "";
  }
}

const appVersion = JSON.parse(readFileSync(path.join(here, "package.json"), "utf8")).version as string;

/** Strict CSP for production builds; the dev variant below adds what Vite's
 *  dev server needs (inline react-refresh preamble, HMR websocket, itself).
 *
 *  `ipc:` + `http://ipc.localhost` are NOT optional here. The Tauri host
 *  (`ui/src-tauri/tauri.conf.json`, `app.security.csp`) injects its own
 *  `<meta http-equiv="Content-Security-Policy">` next to ours
 *  (tauri-utils/html.rs::inject_csp appends a second tag — it does not merge),
 *  and several policies on one document are enforced together: a request must
 *  satisfy every one of them. Without `ipc:` here the intersection is empty and
 *  `core_dispatch` is blocked in the packaged app — the React shell would boot
 *  straight into the "unavailable core" fallback.
 *
 *  `http://127.0.0.1:8099` / `ws://127.0.0.1:8099` / `frame-src … https:` are
 *  honoured by the browser build only (same-origin platform API, mini-app
 *  iframes). Inside the Tauri bundle the host's `connect-src ipc: …` wins, so
 *  the seven HTTP-path suites (wallet/moments/video/files/mini/discover/calls)
 *  stay blocked there. That is deliberate: they must move onto the Rust bridge
 *  (issues #293/#294), not be granted a wider CSP. Guarded by
 *  `node scripts/check-tauri-host.mjs`.
 */
const CSP_STRICT =
  "default-src 'self'; img-src 'self' data: blob:; media-src 'self' data: blob:; " +
  "style-src 'self' 'unsafe-inline'; frame-src 'self' https:; " +
  "connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:8099 ws://127.0.0.1:8099";

const DEV_ORIGIN = "http://localhost:5183";
const CSP_DEV =
  "default-src 'self'; img-src 'self' data: blob:; media-src 'self' data: blob:; " +
  "style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; frame-src 'self' https:; " +
  `connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:8099 ws://127.0.0.1:8099 ${DEV_ORIGIN} ws://localhost:5183 http://127.0.0.1:5183 ws://127.0.0.1:5183`;

/** Injects the CSP <meta> via transformIndexHtml: strict on build, lenient on
 *  serve — a static meta in index.html could not be both. */
function cspMeta(): Plugin {
  return {
    name: "wxlike-csp-meta",
    transformIndexHtml: {
      order: "pre",
      handler(html, ctx) {
        const csp = ctx.server ? CSP_DEV : CSP_STRICT;
        const tag = `\n    <meta http-equiv="Content-Security-Policy" content="${csp}" />`;
        return html.replace("</title>", "</title>" + tag);
      },
    },
  };
}

export default defineConfig(({ mode }) => {
  const production = mode === "production";
  return {
    plugins: [react(), tailwindcss(), cspMeta()],
    resolve: {
      alias: { "@": path.resolve(here, "src") },
    },
    server: { port: 5183 },
    define: {
      // Build provenance for the About page / fixture appInfo. Always defined
      // so `import.meta.env.VITE_*` reads don't depend on shell env vars.
      "import.meta.env.VITE_GIT_SHA": JSON.stringify(gitSha() || "dev"),
      "import.meta.env.VITE_BUILD_TIME": JSON.stringify(new Date().toISOString()),
      "import.meta.env.VITE_APP_VERSION": JSON.stringify(appVersion),
    },
    build: {
      target: "es2022",
      // Sources are shipped to users; keep maps out of production bundles.
      sourcemap: !production,
    },
  };
});
