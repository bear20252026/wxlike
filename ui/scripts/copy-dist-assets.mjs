// Build step (P3): refresh dist/'s static assets so the packaged Tauri app
// serves current files. dist/index.html loads ./app.js and ./styles.css, but
// nothing copied styles.css — the checked-in dist/styles.css went stale and
// the app shipped old styles. This script is the single writer of both files;
// esbuild owns dist/app.js.
import { copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const uiRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = resolve(uiRoot, "dist");
mkdirSync(dist, { recursive: true });

// styles.css: copy the authored source over the stale checked-in copy
copyFileSync(resolve(uiRoot, "src/styles.css"), resolve(dist, "styles.css"));

// index.html: canonical shell (same as the checked-in one — references
// ./styles.css and ./app.js). Written every build so a wiped dist rebuilds.
const indexHtml = `<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>wxlike</title>
  <link rel="stylesheet" href="./styles.css" />
  <style>html,body{margin:0;height:100%}</style>
</head>
<body>
  <div id="app"></div>
  <script src="./app.js"></script>
</body>
</html>
`;
writeFileSync(resolve(dist, "index.html"), indexHtml);
console.log("dist assets refreshed: styles.css copied, index.html written");
