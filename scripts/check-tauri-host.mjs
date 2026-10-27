#!/usr/bin/env node
/**
 * G10（issue #158）桌面宿主守卫：证明「打包出来的就是新 UI」，且打包后 Rust 桥真的能通。
 *
 * 用法：
 *   node scripts/check-tauri-host.mjs                 # 静态检查（不需要构建产物）
 *   node scripts/check-tauri-host.mjs --require-built # 再要求 apps/desktop-web/dist 已产出
 *
 * 为什么要这么麻烦：`ui/src-tauri/tauri.conf.json` 的 frontendDist 曾经指着旧 UI
 * （`../dist`，由 `ui/` 的 esbuild 产出），所以任何 `tauri build` 打包的都是旧界面，
 * 用户根本打不开 React 新 UI。这个脚本把两件事钉死：
 *   1) 默认配置必须指向 desktop-web 的构建产物，且该目录确实存在；
 *   2) 宿主 CSP 与前端 CSP 的交集里必须留着 `ipc:` / `http://ipc.localhost`，
 *      否则 `core_dispatch` 会被 CSP 拦掉（见下面的“双 CSP”说明）。
 *
 * 双 CSP：Tauri 不是改写而是**追加**第二个 <meta http-equiv="Content-Security-Policy">
 * （tauri-utils/src/html.rs::inject_csp）。一份文档上多条策略是「同时成立」的关系，
 * 取交集。所以宿主侧 connect-src 只有 `ipc:` 时，前端 meta 也必须有 `ipc:`，
 * 交集才不为空。
 *
 * 未解决的约束（不要靠放松 CSP 来“修”）：wallet/moments/video/files/mini/discover/calls
 * 这七个域走的是 HTTP fetch。打包宿主里既没有同源的服务端，宿主 CSP 也只允许 ipc:，
 * 所以它们必须迁到 Rust 桥（core_dispatch）上——那是 issue #293/#294 的活，
 * 由其他小组负责。同理 `frame-src 'self' https:`（网页型小程序 iframe）在打包宿主里
 * 会被宿主侧 default-src 'self' 压成 'self'，需要 #293/#294 给出等价方案。
 *
 * 编辑本文件必须用 LF（.gitattributes：*.sh text eol=lf；*.mjs 同样按 text 处理）。
 */
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** 默认宿主必须加载的产物（相对 `ui/src-tauri/`）。 */
const NEW_UI_DIST_REL = "../../apps/desktop-web/dist";
/** 逃生阀：旧 UI 的产物目录（相对 `ui/src-tauri/`）。 */
const OLD_UI_DIST_REL = "../dist";
/** 桥：Windows 走 http://ipc.localhost，macOS/Linux 走 ipc://localhost，两种都要留。 */
const BRIDGE_SOURCES = ["ipc:", "http://ipc.localhost"];

// 未知参数必须报错退出：曾经 `--config <路径>` 被静默忽略，守卫照样返回 0，
// 于是"打错一个 flag"就等于把这道门关掉。
const KNOWN_FLAGS = ["--require-built"];
const unknownFlags = process.argv.slice(2).filter((a) => a.startsWith("-") && !KNOWN_FLAGS.includes(a));
if (unknownFlags.length > 0) {
  console.error(`FAIL: 未知参数 ${unknownFlags.join(" ")}；本脚本只认 ${KNOWN_FLAGS.join(" ")}`);
  process.exit(2);
}

const requireBuilt = process.argv.includes("--require-built");

const problems = [];
const notes = [];

/** 读 JSON 配置；解析失败直接算一条问题。 */
function readJson(rel) {
  const abs = path.join(ROOT, rel);
  if (!fs.existsSync(abs)) {
    problems.push(`缺少文件 ${rel}`);
    return null;
  }
  try {
    return JSON.parse(fs.readFileSync(abs, "utf8"));
  } catch (err) {
    problems.push(`${rel} 不是合法 JSON：${err.message}`);
    return null;
  }
}

/**
 * 解析 CSP 串 -> Map<指令, Set<source>>。
 * 指令内多个 source 用空格分隔；指令之间用分号分隔。
 */
function parseCsp(csp) {
  const map = new Map();
  if (typeof csp !== "string") return map;
  for (const part of csp.split(";")) {
    const tokens = part.trim().split(/\s+/).filter(Boolean);
    if (tokens.length === 0) continue;
    const [directive, ...sources] = tokens;
    map.set(directive.toLowerCase(), new Set(sources.map((s) => s.toLowerCase())));
  }
  return map;
}

/** 取指令的生效 source 集；缺省时回落到该策略自己的 default-src。 */
function sourcesFor(policy, directive) {
  return policy.get(directive) ?? policy.get("default-src") ?? new Set();
}

/** 两条策略的交集（按 source 逐条求交，即浏览器实际生效的那份）。 */
function intersect(a, b) {
  return [...a].filter((s) => b.has(s)).sort();
}

const defaultConf = readJson(path.join("ui", "src-tauri", "tauri.conf.json"));
const oldUiConf = readJson(path.join("ui", "src-tauri", "tauri.conf.old-ui.json"));
const newUiConf = readJson(path.join("ui", "src-tauri", "tauri.conf.new-ui.json"));

if (defaultConf) {
  // 1) 默认宿主 = 新 UI。
  const dist = defaultConf?.build?.frontendDist;
  if (dist === OLD_UI_DIST_REL) {
    problems.push(
      `tauri.conf.json 的 frontendDist 还是 "${OLD_UI_DIST_REL}"（旧 UI），打包出来就是旧界面——issue #158 回归了`,
    );
  } else if (dist !== NEW_UI_DIST_REL) {
    problems.push(`tauri.conf.json 的 frontendDist 期望 "${NEW_UI_DIST_REL}"，实际 "${dist}"`);
  } else {
    notes.push(`frontendDist = ${dist}（新 UI）`);
  }

  // 2) 该目录必须真的存在（frontendDist 相对 src-tauri/）。
  const abs = path.resolve(ROOT, "ui", "src-tauri", dist ?? "");
  const indexHtml = path.join(abs, "index.html");
  if (dist && !fs.existsSync(abs)) {
    problems.push(
      `${path.relative(ROOT, abs)} 不存在：先跑 npm run build:desktop-web（或 bash scripts/build-web-v2.sh）`,
    );
  } else if (dist && requireBuilt) {
    if (fs.existsSync(indexHtml)) {
      notes.push(`产物已就位：${path.relative(ROOT, indexHtml).replace(/\\/g, "/")}`);
    } else {
      problems.push(`${path.relative(ROOT, indexHtml).replace(/\\/g, "/")} 缺失：vite build 没产出 index.html？`);
    }
  }

  // 3) CSP 只允许桥，不许开野口子。
  const policy = parseCsp(defaultConf?.app?.security?.csp);
  const connect = sourcesFor(policy, "connect-src");
  for (const bad of ["*", "http:", "https:", "ws:", "wss:", "data:", "blob:"]) {
    if (connect.has(bad)) {
      problems.push(`宿主 CSP 的 connect-src 含通配/宽松源 "${bad}"——issue #158 明确不允许靠放宽 CSP 解决`);
    }
  }
  const kept = intersect(connect, new Set(BRIDGE_SOURCES));
  if (kept.length !== BRIDGE_SOURCES.length) {
    problems.push(`宿主 CSP 的 connect-src 必须恰好覆盖桥：期望 ${BRIDGE_SOURCES.join(" ")}，实际 ${[...connect].join(" ")}`);
  } else {
    notes.push(`宿主 connect-src = ${[...connect].join(" ")}`);
  }
}

// 4) 旧 UI 逃生阀：文件必须在，且确实指回 ui/dist（旧 UI 没被删掉）。
if (oldUiConf) {
  const dist = oldUiConf?.build?.frontendDist;
  if (dist !== OLD_UI_DIST_REL) {
    problems.push(`tauri.conf.old-ui.json 的 frontendDist 期望 "${OLD_UI_DIST_REL}"，实际 "${dist}"`);
  } else {
    notes.push(`旧 UI 逃生阀 OK：--config src-tauri/tauri.conf.old-ui.json -> ${path.join("ui", "dist")}`);
  }
}

// 5) README/handover 提到的 tauri.conf.new-ui.json 还在被引用，留着当别名；
//    但它必须和默认配置一致，否则两份配置会各说各话。
if (newUiConf && defaultConf) {
  if (newUiConf?.build?.frontendDist !== defaultConf?.build?.frontendDist) {
    problems.push("tauri.conf.new-ui.json 与 tauri.conf.json 的 frontendDist 不一致（会漂移）");
  }
  if (newUiConf?.app?.security?.csp !== defaultConf?.app?.security?.csp) {
    problems.push("tauri.conf.new-ui.json 与 tauri.conf.json 的 CSP 不一致（会漂移）");
  }
  if (!problems.some((p) => p.includes("new-ui"))) notes.push("tauri.conf.new-ui.json 与默认配置一致（保留为文档别名）");
}

// 6) 双 CSP 交集：桥必须在交集里存活，否则打包后 core_dispatch 直接被拦。
const metaCsp = (() => {
  const built = path.join(ROOT, "apps", "desktop-web", "dist", "index.html");
  if (fs.existsSync(built)) {
    const html = fs.readFileSync(built, "utf8");
    const m = html.match(
      /<meta[^>]*http-equiv="Content-Security-Policy"[^>]*content="([^"]*)"/i,
    );
    if (m) return { from: "apps/desktop-web/dist/index.html（构建产物）", csp: m[1] };
  }
  const vite = path.join(ROOT, "apps", "desktop-web", "vite.config.ts");
  if (fs.existsSync(vite)) {
    // vite.config.ts 是 CRLF（仓库没给它 eol=lf 规则），先归一化再匹配。
    const src = fs.readFileSync(vite, "utf8").replace(/\r\n/g, "\n");
    // CSP_STRICT = "a" + "b" 这种拼接形式：取 `= ` 到该行尾分号之间的所有字符串片段。
    // 字符串里也有分号，所以必须以「分号紧跟换行」作为结束标志。
    const body = src.match(/const CSP_STRICT\s*=([\s\S]*?);\n/);
    if (body) {
      const csp = [...body[1].matchAll(/"([^"]*)"/g)].map((x) => x[1]).join("");
      return { from: "apps/desktop-web/vite.config.ts CSP_STRICT（源码，未构建时）", csp };
    }
  }
  return null;
})();

if (metaCsp && defaultConf) {
  const host = parseCsp(defaultConf?.app?.security?.csp);
  const front = parseCsp(metaCsp.csp);
  notes.push(`前端 meta CSP 取自：${metaCsp.from}`);
  const effectiveConnect = intersect(sourcesFor(host, "connect-src"), sourcesFor(front, "connect-src"));
  const missing = BRIDGE_SOURCES.filter((s) => !effectiveConnect.includes(s));
  if (missing.length > 0) {
    problems.push(
      `打包后桥会被 CSP 拦掉：宿主 ∩ meta 的 connect-src = ${effectiveConnect.join(" ") || "(空)"}，缺 ${missing.join(" ")}。` +
        `Tauri 是追加第二个 CSP meta（不合并），策略取交集，所以 vite.config.ts 的 connect-src 也必须含 ipc: 与 http://ipc.localhost`,
    );
  } else {
    notes.push(`生效 connect-src（宿主 ∩ meta）= ${effectiveConnect.join(" ")}`);
  }
  // 图片/音视频/样式不能被交集清空，否则界面直接瞎掉。
  for (const directive of ["img-src", "media-src", "style-src"]) {
    const eff = intersect(sourcesFor(host, directive), sourcesFor(front, directive));
    if (eff.length === 0) {
      problems.push(`${directive} 在宿主 ∩ meta 之后为空集，打包后该资源全部加载失败`);
    }
  }
} else {
  problems.push("找不到前端 meta CSP（既没有 dist/index.html，也没解析出 vite.config.ts 的 CSP_STRICT）");
}

for (const line of notes) console.log(`  · ${line}`);
if (problems.length > 0) {
  console.error(`FAIL: scripts/check-tauri-host.mjs 发现 ${problems.length} 个问题：`);
  for (const p of problems) console.error(`  ✗ ${p}`);
  process.exit(1);
}
console.log(`OK: 默认桌面宿主 = 新 UI，桥在双 CSP 交集下存活${requireBuilt ? "，产物已就位" : ""}`);
