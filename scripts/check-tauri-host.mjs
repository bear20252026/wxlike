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
 * 两个曾经只看一份输入的漏洞（本轮补上）：
 *   · 宿主侧只读 tauri.conf.json，但 desktop/desktop-release 两个作业都是
 *     `tauri build --config <override.json>`，Tauri 对 override 做**深合并**。
 *     谁往 override 里塞一句 `app.security.csp`，打包出的策略就换了，而守卫
 *     照旧返回 0。现在逐份"打包时会生效的配置"（路径直接从 job.yml 里现读，
 *     不写死）都跑同一套断言。
 *   · 前端侧只取 index.html 的**第一条** CSP meta（`match`）。真出现第二条更严
 *     格的 meta 时交集算漏了它。现在 `matchAll` 收全，逐条求交。
 *   override 名单一个都读不到时也 fail-closed：job.yml 里 `--config` 的写法一改
 *   （换成 shell 变量、去掉引号、拆成多行）就等于把这道门悄悄关掉，所以脚本会
 *   数 `--config` 出现次数并核对解析出的路径数，对不上就红。
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
/** 那份不经过任何 override 的配置（`tauri dev` 用的就是它）。 */
const DEFAULT_POLICY = "tauri.conf.json（默认/dev）";
/** 会往宿主配置上叠 override 的两个打包作业（override 名单从这里现读，不写死文件路径）。 */
const DESKTOP_JOB = path.join("ci", "jobs", "desktop", "job.yml");
const DESKTOP_RELEASE_JOB = path.join("ci", "jobs", "desktop-release", "job.yml");
/** 宿主侧一律禁止的宽松源（放宽 CSP 从来不是 #158 的解法）。 */
const LOOSE_SOURCES = ["*", "http:", "https:", "ws:", "wss:", "data:", "blob:"];

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

/**
 * Tauri 的 `--config <json>` 是把 override **深合并**到 tauri.conf.json 上
 * （见 tauri-cli 的 config merge），所以 override 里的 `app.security.csp`
 * 会顶掉打包宿主的策略。守卫只读 tauri.conf.json 就等于给人留了一个
 * 「改 override 就能绕过守卫」的后门——这一段就是把后门堵上。
 */
function deepMerge(base, over) {
  if (over === undefined) return base;
  // 只有「两边都是可递归的普通对象」才继续下钻；其余情况（字符串、数字、null、
  // 数组、一边是对象一边是标量）一律以 override 为准——这正是 Tauri 合并配置时
  // `app.security.csp` 这类标量被整条替换的行为。少这一条，注入的 override
  // 就会被当成"没改任何东西"，守卫继续报绿。
  const bothObjects =
    over !== null && base !== null && typeof over === "object" && typeof base === "object" &&
    !Array.isArray(over) && !Array.isArray(base);
  if (!bothObjects) return structuredClone(over);
  const out = structuredClone(base);
  for (const [k, v] of Object.entries(over)) out[k] = deepMerge(out[k], v);
  return out;
}

/**
 * 打包时真正生效的宿主配置有哪几份：
 *   · `tauri.conf.json` 本身（`tauri dev`、以及任何不带 --config 的构建）
 *   · 每一个 job.yml 里 `--config "$GITHUB_WORKSPACE/…"` 指到的 override
 * 路径从 CI 片段里现读，不写死——否则加了第三个 override 又成了守卫的盲区。
 */
function shippedHostPolicies() {
  const policies = [{ name: DEFAULT_POLICY, conf: defaultConf, isBase: true }];
  for (const rel of [DESKTOP_JOB, DESKTOP_RELEASE_JOB]) {
    const abs = path.join(ROOT, rel);
    if (!fs.existsSync(abs)) {
      problems.push(`找不到 ${rel}：override 名单没法核对`);
      continue;
    }
    const yml = fs.readFileSync(abs, "utf8").replace(/\r\n/g, "\n");
    const mentions = (yml.match(/--config/g) || []).length;
    if (mentions === 0) {
      problems.push(`${rel} 里已经没有 --config 了：override 名单突然为空，本守卫会只核对基础配置，故 fail-closed`);
      continue;
    }
    const parsed = [...yml.matchAll(/--config\s+"([^"]+)"/g)];
    if (parsed.length !== mentions) {
      problems.push(
        `${rel} 有 ${mentions} 处 --config，但只解析出 ${parsed.length} 个带引号的路径——` +
          `换成 shell 变量或多行写法后，这道门会静默漏掉那份 override，故 fail-closed`,
      );
    }
    for (const m of parsed) {
      const overrideRel = m[1].replace(/^\$GITHUB_WORKSPACE\//, "").replace(/^\.\.\//, "");
      const conf = readJson(overrideRel);
      if (!conf) continue;
      const merged = deepMerge(defaultConf, conf);
      const touchedSecurity = conf?.app?.security !== undefined || conf?.build !== undefined;
      policies.push({
        name: `${overrideRel}（${path.basename(rel)} --config，深合并后${touchedSecurity ? "，且 override 动了 app.security/build" : ""}）`,
        conf: merged,
        isBase: false,
      });
    }
  }
  return policies.filter((p) => p.conf);
}

/** 一份宿主配置的 connect-src：不许开野口子，且必须恰好覆盖桥。 */
function checkHostConnect(name, csp) {
  const connect = sourcesFor(parseCsp(csp), "connect-src");
  for (const bad of LOOSE_SOURCES) {
    if (connect.has(bad)) {
      problems.push(`${name} 的 connect-src 含通配/宽松源 "${bad}"——issue #158 明确不允许靠放宽 CSP 解决`);
    }
  }
  const kept = intersect(connect, new Set(BRIDGE_SOURCES));
  if (kept.length !== BRIDGE_SOURCES.length) {
    problems.push(`${name} 的 connect-src 必须恰好覆盖桥：期望 ${BRIDGE_SOURCES.join(" ")}，实际 ${[...connect].join(" ")}`);
    return false;
  }
  return true;
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
  //    "产物在不在"只在 --require-built 时算问题：`npm test` 里那条用例不该
  //    依赖构建顺序（#158 的 CI 就是这么红的）。frontendDist 指向哪儿、
  //    CSP 交集够不够，仍然无条件检查。
  const abs = path.resolve(ROOT, "ui", "src-tauri", dist ?? "");
  const indexHtml = path.join(abs, "index.html");
  if (dist && !fs.existsSync(abs)) {
    if (requireBuilt) {
      problems.push(
        `${path.relative(ROOT, abs)} 不存在：先跑 npm run build:desktop-web（或 bash scripts/build-web-v2.sh）`,
      );
    } else {
      notes.push(`${path.relative(ROOT, abs)} 还没构建（本次不要求产物，只核对配置与 CSP）`);
    }
  } else if (dist && requireBuilt) {
    if (fs.existsSync(indexHtml)) {
      notes.push(`产物已就位：${path.relative(ROOT, indexHtml).replace(/\\/g, "/")}`);
    } else {
      problems.push(`${path.relative(ROOT, indexHtml).replace(/\\/g, "/")} 缺失：vite build 没产出 index.html？`);
    }
  }

  // 3) CSP 只允许桥，不许开野口子（逐份"打包时会生效的配置"检查，见第 7 节）。
  checkHostConnect(DEFAULT_POLICY, defaultConf?.app?.security?.csp);
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

// 6) 前端侧的每一份 CSP meta。Tauri 是**追加**第二条 meta，浏览器把所有 meta
//    当作"同时成立"，所以这里必须收全，只取第一条会漏掉后来加的那条限制。
//    dist 与 vite.config.ts **两边都算**，不做"有产物就不看源码"的取舍：
//    打包用的是 dist，但源码回归不能被一份旧的 dist 遮住（反之亦然）。
const META_RE = /<meta[^>]*http-equiv="Content-Security-Policy"[^>]*content="([^"]*)"/gi;

function frontMetas() {
  const found = [];
  const built = path.join(ROOT, "apps", "desktop-web", "dist", "index.html");
  if (fs.existsSync(built)) {
    const html = fs.readFileSync(built, "utf8");
    for (const m of html.matchAll(META_RE)) {
      found.push({ from: `apps/desktop-web/dist/index.html #${found.length + 1}（构建产物）`, csp: m[1] });
    }
  }
  const vite = path.join(ROOT, "apps", "desktop-web", "vite.config.ts");
  if (fs.existsSync(vite)) {
    // vite.config.ts 是 CRLF（仓库没给它 eol=lf 规则），先归一化再匹配。
    const src = fs.readFileSync(vite, "utf8").replace(/\r\n/g, "\n");
    // CSP_STRICT = "a" + "b" 这种拼接形式：取 `= ` 到该行尾分号之间的所有字符串片段。
    // 字符串里也有分号，所以必须以「分号紧跟换行」作为结束标志。
    for (const name of ["CSP_STRICT"]) {
      const body = src.match(new RegExp(`const ${name}\\s*=([\\s\\S]*?);\\n`));
      if (body) {
        const csp = [...body[1].matchAll(/"([^"]*)"/g)].map((x) => x[1]).join("");
        found.push({ from: `apps/desktop-web/vite.config.ts ${name}（源码）`, csp });
      }
    }
  }
  return found;
}

// 7) 双 CSP 交集：桥必须在"打包时会生效的每份宿主配置 ∩ 每份前端 meta"里存活。
const metas = frontMetas();
if (metas.length === 0) {
  problems.push("找不到前端 meta CSP（既没有 dist/index.html，也没解析出 vite.config.ts 的 CSP_STRICT）");
} else {
  for (const meta of metas) notes.push(`前端 meta CSP #${metas.indexOf(meta) + 1} 取自：${meta.from}`);
  const policies = shippedHostPolicies();
  const overrides = policies.filter((p) => !p.isBase);
  if (overrides.length === 0) {
    problems.push(
      "两份 job.yml 里一个 override 都没解析出来（上面应有具体原因）；只剩基础配置可核对，故这里同样 fail-closed",
    );
  }
  for (const host of policies) {
    // 基础配置在第 3 节查过，这里只补 override 深合并后的那几份。
    if (!host.isBase && !checkHostConnect(host.name, host.conf?.app?.security?.csp)) continue;
    if (!host.isBase) notes.push(`宿主配置：${host.name}`);
    const hostPolicy = parseCsp(host.conf?.app?.security?.csp);
    for (const meta of metas) {
      const front = parseCsp(meta.csp);
      const effectiveConnect = intersect(sourcesFor(hostPolicy, "connect-src"), sourcesFor(front, "connect-src"));
      const missing = BRIDGE_SOURCES.filter((s) => !effectiveConnect.includes(s));
      if (missing.length > 0) {
        problems.push(
          `打包后桥会被 CSP 拦掉：${host.name} ∩ ${meta.from} 的 connect-src = ` +
            `${effectiveConnect.join(" ") || "(空)"}，缺 ${missing.join(" ")}。` +
            `Tauri 是追加第二个 CSP meta（不合并），策略取交集，所以 vite.config.ts 的 connect-src 也必须含 ipc: 与 http://ipc.localhost`,
        );
      } else if (!host.isBase || metas.length > 1) {
        notes.push(`生效 connect-src（${host.name} ∩ meta#${metas.indexOf(meta) + 1}）= ${effectiveConnect.join(" ")}`);
      }
      // 图片/音视频/样式不能被交集清空，否则界面直接瞎掉。
      for (const directive of ["img-src", "media-src", "style-src"]) {
        const eff = intersect(sourcesFor(hostPolicy, directive), sourcesFor(front, directive));
        if (eff.length === 0) {
          problems.push(`${directive} 在 ${host.name} ∩ ${meta.from} 之后为空集，打包后该资源全部加载失败`);
        }
      }
    }
  }
  notes.push(`本次核对：${policies.length} 份宿主配置 × ${metas.length} 份前端 meta`);
}

for (const line of notes) console.log(`  · ${line}`);
if (problems.length > 0) {
  console.error(`FAIL: scripts/check-tauri-host.mjs 发现 ${problems.length} 个问题：`);
  for (const p of problems) console.error(`  ✗ ${p}`);
  process.exit(1);
}
console.log(`OK: 默认桌面宿主 = 新 UI，桥在双 CSP 交集下存活${requireBuilt ? "，产物已就位" : ""}`);
