#!/usr/bin/env node
/**
 * #294（G10）静态守卫：生产业务模块里"绕过 Core 直接 fetch"与"从 localStorage 读
 * 会话（bearer/base/身份）"必须被逐条抓到。
 *
 * ## 为什么它不是"永远为真"的一条规则
 *
 * 卡面第 5 条要的是"接了就会红"。这里用**允许名单只放已迁移的域**表达：
 *
 * * `state: "core"` 的域（现在只有钱包，provider = #293 @ 71cc90c5）——它的生产模块
 *   里出现任何一条 bearer/base/fetch 用法都是**立即红**，没有额度、没有豁免。
 *   把钱包改回"从 localStorage 取 token"，红就在这条（规则 (d)）。
 * * `state: "blocked"` 的域（内容/视频/通话/文件/小程序/发现）——它们**现在就是违例**，
 *   所以本脚本把它们当作**有额度、可下降、不可上升**的债台账逐条计数：文件集合与
 *   条数都必须与 `DEBT_BASELINE` 完全一致，多一条红，少一条也红（少了一条要先把基线
 *   降下来，口径同 `scripts/quality-gate.py`：闸门只会单向收紧）。
 * * 名单本身也要对得上现实：登记为 blocked 的域如果实际已经没有任何遗留用法
 *   （= 其实迁走了），或者被谎报成 core 却没有桥的 action 兜着 —— 都红。
 *   **这一条就是"把允许名单写成包含全部七域"会变红的地方。**
 *
 * ## 四份名单必须同源（漂移即红）
 *
 * `lib/domain-registry.ts`（域状态） ∩ `lib/http.ts` 的 `CORE_ONLY_PREFIXES`（遗留出口
 * 的拒绝面） ∩ `lib/core-transport.ts` 的 `WALLET_ROUTES`/`BLOCKED_WALLET_PREFIXES`
 * （路由与显式阻塞） ∩ `shell/core-contract.ts` 的 `ACTIONS.wallet*`（id 只此一份） ∩
 * `ui/src-tauri/src/core_bridge.rs` 的 `wallet_router`（桥真实注册的臂）。
 * 少一条、多一条、写错名字，全部红 —— "UI 声称接了但桥没有那条 action"正是
 * "页面能点 = 完成"这种假绿的形状。
 *
 * ## 规则引擎可注入
 *
 * `runChecks(overrides)` 把"读到的源码"当成输入，默认从磁盘读；用例可以只替换其中
 * 一个文件的内容来证明规则**真的可达**（注入证明），而不去改仓库里的文件——改文件会
 * 被并行用例读到半态，那种"绿"不作数。
 *
 * `--json` 会把 `rules`（规则名清单）一起吐出来，用例据此逐条喂人造违例并断言它报红，
 * 同时断言"规则清单里的每一条都有对应的注入用例"——以后往 `RULES` 里加一条却忘了配
 * 用例，测试就红，而不是多一条永远没人打得到的死规则。第一版就是这么漏的：真实写法
 * `const tok = token();`（files/video 里就有）不在任何 pattern 里，于是"钱包改回读
 * 独立 bearer"那条注入证明根本打不红，补成了 `legacy_bearer_call`。
 *
 * 用法：
 *   node scripts/check-core-transport.mjs                 # 人读输出
 *   node scripts/check-core-transport.mjs --json          # 机读（违例逐条带位置）
 *   node scripts/check-core-transport.mjs --emit-baseline # 打印当前债台账（只用来收紧基线）
 */
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const ROOT = path.resolve(HERE, "..");
const WEB_REL = "apps/desktop-web/src";

export const RULES = {
  direct_fetch: {
    pattern: /\bfetch\s*\(/,
    why: "绕过公共 transport 直接 fetch（打包宿主里必被 CSP 拦，且不经认证核心会话）",
  },
  legacy_transport: {
    pattern: /\brequest\s*(<[^>]*>)?\s*\(/,
    why: "仍经公共 transport 的遗留 HTTP 出口发请求——默认那一份的 base/token 来自独立 localStorage",
  },
  legacy_token: {
    pattern: /\.token\(\)|localStorage\.getItem\(\s*"[^"]*(?:token|bearer)[^"]*"|authorization:\s*[`"']Bearer|\bsaveSession\(|\bsetBaseUrl\(/,
    why: "从独立 localStorage 会话里取 bearer（#294 要删的就是这条依赖）",
  },
  // 这一条是被注入证明逼出来的：仓库里真实的取 bearer 写法就是 `const tok = token();`
  // （`features/files/api.ts:47/94/122`、`features/video/api.ts:65`），上一版只认
  // `.token()`（动态导入那一种），所以"钱包改回读独立 bearer"这条注入根本打不红。
  legacy_bearer_call: {
    pattern: /\btoken\s*\(\s*\)/,
    why: "直接调用 lib/http 的 token() 取独立 localStorage 里的 bearer",
  },
  legacy_base: {
    pattern: /\bbaseUrl\(\)|localStorage\.getItem\(\s*"wxlike-server/,
    why: "服务器地址取自独立 localStorage，而不是核心会话里的那一份",
  },
  legacy_user: {
    pattern: /\buserIdSafe\s*\(\s*\)|\buserId\s*\(\s*\)/,
    why: "界面自己从 localStorage 取用户身份，而不是读核心会话投影",
  },
};

/** 已经整体走 CoreHandle 的域（不属于 #294 这七格）：一条遗留用法都不许有。 */
export const SHELL_CORE_DIRS = new Set(["auth", "chat", "contacts", "search", "settings"]);
/** 唯一被许可的遗留出口本体（`fetchTransport` 就在里面）。 */
export const LEGACY_EGRESS = "lib/http.ts";

/**
 * 债台账：六个未迁移域的现存违例，实测于 `--emit-baseline`。
 * 每个域的 provider 落地后，删掉对应条目并把域改成 core；条数变少要同时把这里降下来。
 * （`legacy_bearer_call` 是补出来的规则：台账因此比 71cc90c5 多 5 条裸 `token()`。
 * 数字变长只可能是"看得更清"，闸门本身仍然只许降不许升。）
 */
export const DEBT_BASELINE = {
  content: { files: ["features/content/MomentsPage.tsx", "features/content/api.ts"], counts: { legacy_transport: 21, legacy_user: 1 } },
  video: { files: ["features/video/api.ts"], counts: { legacy_base: 1, legacy_bearer_call: 1, legacy_transport: 9, legacy_user: 1 } },
  calls: { files: ["features/calls/api.ts"], counts: { legacy_transport: 12 } },
  files: { files: ["features/files/api.ts"], counts: { legacy_base: 3, direct_fetch: 2, legacy_token: 2, legacy_bearer_call: 3, legacy_transport: 8 } },
  mini: { files: ["features/mini/api.ts"], counts: { legacy_transport: 7, direct_fetch: 1 } },
  discover: { files: ["features/discover/api.ts"], counts: { legacy_transport: 7, legacy_token: 2, direct_fetch: 1, legacy_bearer_call: 1 } },
};

const CONFIG_SOURCES = [
  "apps/desktop-web/src/lib/domain-registry.ts",
  "apps/desktop-web/src/lib/http.ts",
  "apps/desktop-web/src/lib/core-transport.ts",
  "apps/desktop-web/src/shell/core-contract.ts",
  "ui/src-tauri/src/core_bridge.rs",
];

function normalize(text) {
  return text.replace(/\r\n/g, "\n");
}

/** 默认输入：磁盘上现在这些文件。 */
function readDefaultSources() {
  const sources = new Map();
  for (const rel of CONFIG_SOURCES) {
    const abs = path.join(ROOT, rel);
    if (fs.existsSync(abs)) sources.set(rel, normalize(fs.readFileSync(abs, "utf8")));
    else sources.set(rel, "");
  }
  const base = path.join(ROOT, WEB_REL);
  if (fs.existsSync(base)) {
    for (const rel of walkFiles(base)) {
      sources.set(`${WEB_REL}/${rel}`, normalize(fs.readFileSync(path.join(base, rel), "utf8")));
    }
  }
  return sources;
}

function* walkFiles(dir, prefix = "") {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const abs = path.join(dir, entry.name);
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      yield* walkFiles(abs, rel);
      continue;
    }
    if (!/\.(ts|tsx)$/.test(entry.name)) continue;
    if (/\.test\.(ts|tsx)$/.test(entry.name)) continue;
    if (/(^|\/)(tests?|__fixtures__|fixture[s]?(\/|$))/i.test(rel)) continue;
    yield rel;
  }
}

// ---------------------------------------------------------------------------
// 从源码里取回名单（不在脚本里维护第二份常量）
// ---------------------------------------------------------------------------

function parseDomainRegistry(text, problems) {
  if (!text) {
    problems.push("缺少 domain-registry.ts：本守卫的域名单来源断了");
    return [];
  }
  const block = text.match(/export const DOMAIN_REGISTRY[\s\S]*?\n\];/);
  if (!block) {
    problems.push("domain-registry.ts 里找不到 DOMAIN_REGISTRY 数组：无法核对域状态");
    return [];
  }
  // providerSha 允许写成常量引用（`providerSha: WALLET_PROVIDER_SHA`），这里解析回字面量，
  // 免得"记录 SHA"这件事因为写法是常量就被守卫当成没记录。
  const constants = new Map(
    [...text.matchAll(/const\s+(\w+)\s*(?::\s*string\s*)?=\s*"([^"]+)"/g)].map((m) => [m[1], m[2]]),
  );
  const entries = [];
  for (const match of block[0].matchAll(/\{\s*id:\s*"([^"]+)"[\s\S]*?\}/g)) {
    const raw = match[0];
    const featureDir = raw.match(/featureDir:\s*"([^"]+)"/)?.[1] ?? null;
    const state = raw.match(/state:\s*"([^"]+)"/)?.[1] ?? null;
    const shaToken = raw.match(/providerSha:\s*(null|"[^"]+"|[A-Za-z_]\w*)/)?.[1] ?? null;
    const providerSha =
      shaToken === null || shaToken === "null"
        ? null
        : shaToken.startsWith('"')
          ? shaToken.slice(1, -1)
          : constants.get(shaToken) ?? `?unresolved:${shaToken}`;
    if (!featureDir || !state) {
      problems.push(`DOMAIN_REGISTRY 有条目缺 featureDir/state：${match[1]}`);
      continue;
    }
    entries.push({ id: match[1], featureDir, state, providerSha });
  }
  if (entries.length === 0) problems.push("DOMAIN_REGISTRY 解析出 0 个域：规则会空转，故 fail-closed");
  return entries;
}

function parseArrayConstant(text, name, label, problems) {
  if (!text) {
    problems.push(`${label}：文件缺失，读不到 ${name}`);
    return [];
  }
  const block = text.match(new RegExp(`(?:export )?const ${name}[^=]*=\\s*\\[([\\s\\S]*?)\\];`));
  if (!block) {
    problems.push(`${label} 里找不到 ${name}：这条拒绝面/名单消失了`);
    return [];
  }
  return [...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

function parseContractActions(text, problems) {
  if (!text) {
    problems.push("core-contract.ts 缺失：action id 的来源断了");
    return {};
  }
  const block = text.match(/export const ACTIONS\s*=\s*\{([\s\S]*?)\}\s*as const/);
  if (!block) {
    problems.push("core-contract.ts 里找不到 ACTIONS：action id 的来源断了");
    return {};
  }
  return Object.fromEntries([...block[1].matchAll(/(\w+):\s*"([^"]+)"/g)].map((m) => [m[1], m[2]]));
}

function parseRoutes(text, aliasMap, contract, problems) {
  if (!text) {
    problems.push("core-transport.ts 缺失：已迁移域没有路由");
    return [];
  }
  const block = text.match(/export const WALLET_ROUTES[\s\S]*?\n\];/);
  if (!block) {
    problems.push("core-transport.ts 里找不到 WALLET_ROUTES：已迁移域没有路由，只能回落到旧 HTTP");
    return [];
  }
  const routes = [];
  for (const match of block[0].matchAll(/method:\s*"(GET|POST)",\s*pattern:\s*"([^"]+)",\s*action:\s*WALLET_ACTIONS\.(\w+)/g)) {
    const aliasKey = aliasMap[match[3]];
    routes.push({
      method: match[1],
      pattern: match[2],
      action: aliasKey ? (contract[aliasKey] ?? `#unresolved:ACTIONS.${aliasKey}`) : `?unresolved:WALLET_ACTIONS.${match[3]}`,
    });
  }
  if (routes.length === 0) problems.push("WALLET_ROUTES 解析出 0 条路由：规则会空转，故 fail-closed");
  return routes;
}

function parseAliasMap(text, problems) {
  if (!text) return {};
  const aliasBlock = text.match(/const WALLET_ACTIONS\s*=\s*\{([\s\S]*?)\}\s*as const/);
  if (!aliasBlock) {
    problems.push("core-transport.ts 的 WALLET_ACTIONS 别名块不见了：action id 疑似在本文件又抄了一份");
    return {};
  }
  return Object.fromEntries([...aliasBlock[1].matchAll(/(\w+):\s*ACTIONS\.(\w+)/g)].map((m) => [m[1], m[2]]));
}

function parseBridgeArms(text, problems) {
  if (!text) {
    problems.push("core_bridge.rs 缺失：无法核对桥真实注册的钱包臂");
    return [];
  }
  const block = text.match(/fn wallet_router[\s\S]*?\n\}/);
  if (!block) {
    problems.push("core_bridge.rs 里找不到 wallet_router：桥的钱包臂无法核对");
    return [];
  }
  return [...block[0].matchAll(/"(wallet\.[\w.]+)"/g)].map((m) => m[1]);
}

// ---------------------------------------------------------------------------
// 扫描 + 规则
// ---------------------------------------------------------------------------

function scan(sources) {
  const found = [];
  const byDir = new Map();
  for (const [rel, text] of sources) {
    if (!rel.startsWith(`${WEB_REL}/`)) continue;
    const file = rel.slice(`${WEB_REL}/`.length);
    const top = file.split("/")[0];
    const dir = top === "features" ? `features/${file.split("/")[1]}` : file;
    const httpImports = [...text.matchAll(/import\s*(?:type\s*)?\{([^}]*)\}\s*from\s*"[^"]*lib\/http"/g)].map((m) => m[1]);
    const dynamicHttpImport = /import\(\s*"[^"]*lib\/http"\s*\)/.test(text);
    const imports = (symbol) => httpImports.some((clause) => new RegExp(`\\b${symbol}\\b`).test(clause));
    const gate = {
      // `request(` / `userId(` / `token()` 这种词在界面里太常见（Notification 权限也叫 request）。
      // 只有**确实从 lib/http 引了这些符号**的文件才算碰了遗留 transport/遗留身份。
      legacy_transport: imports("request") || dynamicHttpImport,
      legacy_user: imports("userIdSafe") || imports("userId"),
      legacy_bearer_call: imports("token") || dynamicHttpImport,
    };
    text.split("\n").forEach((line, index) => {
      const trimmed = line.trim();
      if (trimmed.startsWith("//") || trimmed.startsWith("/*") || trimmed.startsWith("*")) return;
      for (const [rule, spec] of Object.entries(RULES)) {
        if (!spec.pattern.test(line)) continue;
        if (gate[rule] === false) continue;
        const hit = { file: dir, path: file, line: index + 1, rule, text: trimmed.slice(0, 120) };
        found.push(hit);
        byDir.set(dir, [...(byDir.get(dir) ?? []), hit]);
      }
    });
  }
  const byDomain = new Map();
  for (const hit of found) {
    const dir = hit.file.startsWith("features/") ? hit.file.slice("features/".length).split("/")[0] : null;
    if (!dir) continue;
    const bucket = byDomain.get(dir) ?? {};
    bucket[hit.rule] = (bucket[hit.rule] ?? 0) + 1;
    byDomain.set(dir, bucket);
  }
  return { found, byDir, byDomain };
}

function apiLiteralsFor(sources, featureDir) {
  const out = new Set();
  for (const [rel, text] of sources) {
    if (!rel.startsWith(`${WEB_REL}/features/${featureDir}/`)) continue;
    if (/\.test\.(ts|tsx)$/.test(rel)) continue;
    for (const m of text.matchAll(/["'`](\/api\/[^"'`\r\n]*)/g)) {
      out.add(m[1].split("?")[0].replace(/\$\{[^}]*\}/g, "x"));
    }
  }
  return [...out].sort();
}

/**
 * 全部判定。`overrides` 用仓库根相对路径覆盖单个文件内容（注入证明用），
 * 其余仍从磁盘读——所以被注入的那条规则红，别的规则照旧，不会整片失效。
 */
export function runChecks(overrides = {}) {
  const sources = readDefaultSources();
  for (const [rel, text] of Object.entries(overrides)) sources.set(rel.startsWith(`${WEB_REL}/`) || rel.startsWith("apps/") || rel.startsWith("ui/") ? rel : `${WEB_REL}/${rel}`, normalize(text));

  const problems = [];
  const notes = [];
  const domains = parseDomainRegistry(sources.get("apps/desktop-web/src/lib/domain-registry.ts") ?? "", problems);
  const coreOnlyPrefixes = parseArrayConstant(
    sources.get("apps/desktop-web/src/lib/http.ts") ?? "",
    "CORE_ONLY_PREFIXES",
    "http.ts",
    problems,
  );
  const contract = parseContractActions(sources.get("apps/desktop-web/src/shell/core-contract.ts") ?? "", problems);
  const contractActions = Object.values(contract).filter((id) => id.startsWith("wallet."));
  const transportText = sources.get("apps/desktop-web/src/lib/core-transport.ts") ?? "";
  const aliasMap = parseAliasMap(transportText, problems);
  const routes = parseRoutes(transportText, aliasMap, contract, problems);
  const blockedWalletPrefixes = parseArrayConstant(transportText, "BLOCKED_WALLET_PREFIXES", "core-transport.ts", problems);
  const bridgeArms = parseBridgeArms(sources.get("ui/src-tauri/src/core_bridge.rs") ?? "", problems);
  const { found, byDir, byDomain } = scan(sources);

  const coveredBy = (p, prefixes) => prefixes.some((prefix) => p === prefix || p.startsWith(`${prefix}/`));
  const domainPrefixesCovered = (domain) => {
    const literals = apiLiteralsFor(sources, domain.featureDir);
    return literals.length === 0 || literals.every((p) => coveredBy(p, coreOnlyPrefixes));
  };

  /** (a) 遗留出口只许一份：src/lib 里除 http.ts 之外谁都不许直接 fetch。 */
  {
    const offenders = [...byDir.keys()].filter(
      (dir) => !dir.startsWith("features/") && dir !== LEGACY_EGRESS && dir !== "lib/core-transport.ts",
    );
    const extraFetches = offenders.flatMap((dir) => (byDir.get(dir) ?? []).filter((h) => h.rule === "direct_fetch"));
    if (extraFetches.length > 0) {
      problems.push(`遗留出口必须只有一份：${extraFetches.map((h) => `${h.path}:${h.line}`).join(", ")} 也在直接 fetch`);
    }
    const egressHits = byDir.get(LEGACY_EGRESS) ?? [];
    if (!egressHits.some((h) => h.rule === "direct_fetch")) {
      problems.push("http.ts 里已经找不到 fetchTransport 的 fetch：遗留出口被摘掉了？那就不该再有域标成 blocked");
    }
  }

  /** (b) 已走核心的 shell 域不许碰遗留 transport/遗留会话（一条都不许）。 */
  for (const dir of SHELL_CORE_DIRS) {
    const hits = byDomain.get(dir) ?? {};
    if (Object.keys(hits).length > 0) {
      problems.push(`shell 域 features/${dir} 本应只用 CoreHandle，却碰了遗留出口：${JSON.stringify(hits)}`);
    }
  }

  /** (c) 每个有遗留用法的 feature 目录都必须在注册表里有说法。 */
  {
    const known = new Set([...domains.map((d) => d.featureDir), ...SHELL_CORE_DIRS]);
    for (const dir of new Set([...byDomain.keys()])) {
      if (!known.has(dir)) {
        problems.push(`features/${dir} 有遗留 HTTP/fetch 用法，却没登记在 DOMAIN_REGISTRY 里：新增域必须先说清它的 provider`);
      }
    }
  }

  /** (d) 已迁移域零额度。 */
  const coreDomains = domains.filter((d) => d.state === "core");
  const blockedDomains = domains.filter((d) => d.state === "blocked");
  for (const domain of coreDomains) {
    const hits = { ...(byDomain.get(domain.featureDir) ?? {}) };
    const tolerated = hits.legacy_transport ?? 0;
    delete hits.legacy_transport;
    if (Object.keys(hits).length > 0) {
      problems.push(
        `已迁移域 features/${domain.featureDir}（${domain.id}，provider ${domain.providerSha ?? "?"}）里仍有遗留会话用法：` +
          Object.entries(hits).map(([rule, n]) => `${rule}×${n}`).join(", "),
      );
      for (const hit of byDir.get(`features/${domain.featureDir}`) ?? []) {
        if (hit.rule === "legacy_transport") continue;
        problems.push(`  ↳ ${hit.path}:${hit.line} ${hit.rule}｜${RULES[hit.rule].why}`);
      }
    }
    if (tolerated > 0 && !domainPrefixesCovered(domain)) {
      problems.push(
        `已迁移域 features/${domain.featureDir} 还调用公共 request() ${tolerated} 次，但它部分 /api 路径不在 CORE_ONLY_PREFIXES 的拒绝面里（= 会回落到遗留 fetch + localStorage 会话）`,
      );
    }
  }

  /** (e) blocked 域名不副实：登记成阻塞、实际早已没有遗留用法 = 名单与现实不符。 */
  for (const domain of blockedDomains) {
    if (!byDomain.has(domain.featureDir)) {
      problems.push(
        `features/${domain.featureDir}（${domain.id}）登记为 blocked，但它生产代码里没有任何遗留 HTTP 用法：` +
          `要么它已经迁走（改成 core 并记录 provider SHA），要么这条登记是空占位`,
      );
    }
  }

  /** (f) 标 core 却不记录 provider SHA = 卡面"消费者记录该 SHA"没落地。 */
  for (const domain of coreDomains) {
    if (!domain.providerSha || domain.providerSha.startsWith("?unresolved")) {
      problems.push(`域 ${domain.id} 标成 core 却没有可解析的 providerSha（${domain.providerSha ?? "null"}）：#294 要求消费方记录提供者 SHA`);
    }
  }

  /** (g) 四份名单同源。 */
  {
    const routeActions = [...new Set(routes.map((r) => r.action))].sort();
    const contractSorted = [...new Set(contractActions)].sort();
    const bridgeSorted = [...new Set(bridgeArms)].sort();
    const cmp = (label, a, b, bLabel) => {
      const missing = b.filter((x) => !a.includes(x));
      const extra = a.filter((x) => !b.includes(x));
      if (missing.length || extra.length) {
        problems.push(`${label} 与 ${bLabel} 不一致：只在 ${label} 有 = [${extra.join(", ")}]；只在 ${bLabel} 有 = [${missing.join(", ")}]`);
      }
    };
    cmp("core-transport 的 WALLET_ROUTES", routeActions, contractSorted, "core-contract 的 ACTIONS.wallet*");
    cmp("core-contract 的 ACTIONS.wallet*", contractSorted, bridgeSorted, "core_bridge.rs 的 wallet_router");
    const unresolved = routes.filter((r) => r.action.includes("unresolved"));
    if (unresolved.length > 0) {
      problems.push(`WALLET_ROUTES 有 action 解析不出 id：${unresolved.map((r) => `${r.method} ${r.pattern} -> ${r.action}`).join(", ")}`);
    }
  }

  /** (h) 已迁移域每条路径都要有归属：路由 or 显式阻塞。 */
  {
    for (const domain of coreDomains) {
      const uncovered = apiLiteralsFor(sources, domain.featureDir).filter((p) => !coveredBy(p, coreOnlyPrefixes));
      if (uncovered.length > 0) {
        problems.push(
          `已迁移域 features/${domain.featureDir} 用了这些路径，而 CORE_ONLY_PREFIXES 没拒绝它们（= 还能回落到旧 localStorage 会话）：${uncovered.join(", ")}`,
        );
      }
    }
    const orphans = coreOnlyPrefixes.filter(
      (prefix) =>
        !routes.some((r) => r.pattern === prefix || r.pattern.startsWith(`${prefix}/`)) &&
        !blockedWalletPrefixes.some((p) => prefix === p || prefix.startsWith(`${p}/`) || p.startsWith(prefix)),
    );
    if (orphans.length > 0) {
      problems.push(
        `CORE_ONLY_PREFIXES 里有前缀既没被 WALLET_ROUTES 路由、也没登记进 BLOCKED_WALLET_PREFIXES（归属不清就别说这个域迁完了）：${orphans.join(", ")}`,
      );
    }
  }

  /** (i) 债台账：blocked 域的违例文件集合与条数必须与基线完全一致（只许降不许升）。 */
  const ledger = {};
  for (const [dir, expected] of Object.entries(DEBT_BASELINE)) {
    const hits = byDomain.get(dir) ?? {};
    const got = {
      files: [...new Set((byDir.get(`features/${dir}`) ?? []).map((h) => h.path))].sort(),
      counts: { ...hits },
    };
    ledger[dir] = got;
    const sameFiles = JSON.stringify([...expected.files].sort()) === JSON.stringify(got.files);
    const sameCounts =
      Object.entries(expected.counts).every(([rule, n]) => (got.counts[rule] ?? 0) === n) &&
      Object.entries(got.counts).every(([rule, n]) => (expected.counts[rule] ?? 0) === n);
    if (!sameFiles || !sameCounts) {
      problems.push(
        `债台账漂移：features/${dir} 期望 文件=${JSON.stringify(expected.files)} 条数=${JSON.stringify(expected.counts)}，` +
          `实际 文件=${JSON.stringify(got.files)} 条数=${JSON.stringify(got.counts)}。` +
          `只允许变好——迁完就把这条删掉并把域改成 core，别加新违例。`,
      );
    }
  }
  const total = Object.values(ledger).reduce((sum, d) => sum + Object.values(d.counts).reduce((s, n) => s + n, 0), 0);
  notes.push(`现存违例：${total} 条，分布在 ${Object.keys(DEBT_BASELINE).length} 个未迁移域（已迁移域额度 0）`);
  notes.push(`钱包出口归属：路由 ${routes.length} 条 / 显式阻塞前缀 ${blockedWalletPrefixes.length} 条 / 遗留拒绝面 ${coreOnlyPrefixes.length} 条`);

  return {
    problems,
    notes,
    domains,
    routes,
    bridgeArms,
    contractActions,
    coreOnlyPrefixes,
    blockedWalletPrefixes,
    violations: found,
    ledger,
    total,
  };
}

/**
 * `WXLIKE_CORE_TRANSPORT_OVERRIDES=<json>`：`{"相对路径": "该文件的新内容"}`。
 *
 * 只用于让**用例**能注入一份改过的源码来证明规则可达（注入红），不落盘、不改仓库
 * 文件——改真文件会被并行用例读到半态，那种绿不作数。它只能往 `runChecks` 里加
 * 内容，没有任何"关掉某条规则"的开关；CI 从不设这个变量，默认全部按磁盘现状判。
 */
function readOverridesFromEnv() {
  const file = process.env.WXLIKE_CORE_TRANSPORT_OVERRIDES;
  if (!file) return {};
  if (!fs.existsSync(file)) {
    throw new Error(`WXLIKE_CORE_TRANSPORT_OVERRIDES 指向的文件不存在：${file}`);
  }
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function main() {
  const argv = process.argv.slice(2);
  const KNOWN_FLAGS = ["--json", "--emit-baseline"];
  const unknownFlags = argv.filter((a) => a.startsWith("-") && !KNOWN_FLAGS.includes(a));
  if (unknownFlags.length > 0) {
    console.error(`FAIL: 未知参数 ${unknownFlags.join(" ")}；本脚本只认 ${KNOWN_FLAGS.join(" ")}`);
    process.exit(2);
  }
  const result = runChecks(readOverridesFromEnv());
  const payload = {
    rules: Object.keys(RULES),
    domains: result.domains.map((d) => ({ ...d, violations: result.ledger[d.featureDir]?.counts ?? {} })),
    routes: result.routes,
    bridgeArms: result.bridgeArms,
    contractActions: result.contractActions,
    coreOnlyPrefixes: result.coreOnlyPrefixes,
    violations: result.violations,
    ledger: result.ledger,
    total: result.total,
    problems: result.problems,
  };
  if (argv.includes("--emit-baseline")) {
    console.log(JSON.stringify(result.ledger, null, 2));
    process.exit(result.problems.length > 0 ? 1 : 0);
  }
  if (argv.includes("--json")) {
    console.log(JSON.stringify(payload, null, 2));
    process.exit(result.problems.length > 0 ? 1 : 0);
  }
  for (const line of result.notes) console.log(`  · ${line}`);
  if (result.problems.length > 0) {
    console.error(`FAIL: scripts/check-core-transport.mjs 发现 ${result.problems.length} 个问题：`);
    for (const p of result.problems) console.error(`  ✗ ${p}`);
    process.exit(1);
  }
  const core = result.domains.filter((d) => d.state === "core").map((d) => d.id);
  console.log(
    `OK: 已迁移域（${core.join("/") || "无"}）零遗留 HTTP；未迁移域债台账未增长；四份 action 名单同源`,
  );
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main();
}
