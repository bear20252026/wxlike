import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, describe, expect, it } from "vitest";

/**
 * 卡面第 5 条（#294）：静态检查生产业务模块里"绕过 Core 直接 fetch"与
 * "从 localStorage 读 token"。
 *
 * 判定逻辑只有一份实现：仓库根的 `scripts/check-core-transport.mjs`。这里用 vitest
 * 把它跑起来，让现有的 `npm test`（以及已经会 npm test 的 web-v2 CI 作业）自动把关，
 * 不必谁再记着加 CI 步骤——口径同 `host-config.test.ts` 跑 #158 守卫。
 *
 * 这组用例证明的是**检查不是空转**：
 * 1. 现存违例条数必须 > 0，且六个未迁移域逐域都被抓到；
 * 2. 已迁移域（钱包）额度为零：任何 bearer/base/fetch 用法都会红；
 * 3. 两条注入证明各红一次 ——「把允许名单写成包含全部七域」和「钱包仍从
 *    localStorage 取 token」。注入走脚本的 override 入口（内存里换一份文件），
 *    不改仓库文件：改了会被并行用例读到半态，那种绿不作数。
 */
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "..", "..", "..");
const CHECK = path.join(ROOT, "scripts", "check-core-transport.mjs");
const TMP = mkdtempSync(path.join(os.tmpdir(), "wxlike-294-"));

afterAll(() => rmSync(TMP, { recursive: true, force: true }));

interface CheckJson {
  domains: { id: string; featureDir: string; state: string; providerSha: string | null }[];
  routes: { method: string; pattern: string; action: string }[];
  bridgeArms: string[];
  contractActions: string[];
  coreOnlyPrefixes: string[];
  violations: { file: string; path: string; line: number; rule: string }[];
  ledger: Record<string, { files: string[]; counts: Record<string, number> }>;
  total: number;
  problems: string[];
}

function run(args: string[] = [], overrides?: Record<string, string>): { status: number; stdout: string; stderr: string } {
  const env = { ...process.env };
  if (overrides) {
    const file = path.join(TMP, `overrides-${Object.keys(overrides)[0].replace(/[^\w]/g, "_")}-${Math.random().toString(36).slice(2)}.json`);
    writeFileSync(file, JSON.stringify(overrides));
    env.WXLIKE_CORE_TRANSPORT_OVERRIDES = file;
  }
  const res = spawnSync(process.execPath, [CHECK, ...args], { cwd: ROOT, encoding: "utf8", env });
  return { status: res.status ?? -1, stdout: res.stdout, stderr: res.stderr };
}

function runJson(overrides?: Record<string, string>): CheckJson {
  const res = run(["--json"], overrides);
  try {
    return JSON.parse(res.stdout) as CheckJson;
  } catch {
    throw new Error(`守卫 --json 输出不可解析（status=${res.status}）：\n${res.stdout}\n${res.stderr}`);
  }
}

const REGISTRY_REL = "apps/desktop-web/src/lib/domain-registry.ts";
const COMMERCE_API_REL = "apps/desktop-web/src/features/commerce/api.ts";
const BRIDGE_REL = "ui/src-tauri/src/core_bridge.rs";

describe("静态检查：绕过 Core 的 fetch / localStorage 会话（#294 DoD 第 5 条）", () => {
  it("今天判定为绿，并把现存违例的条数如实打印出来", () => {
    const res = run();
    expect(res.stdout + res.stderr).toContain("OK: 已迁移域（wallet）零遗留 HTTP");
    expect(res.status).toBe(0);
    const count = /现存违例：(\d+) 条，分布在 (\d+) 个未迁移域/.exec(res.stdout);
    expect(count, `输出里必须自带现存违例计数，实际：${res.stdout}`).toBeTruthy();
    expect(Number(count?.[1]), "违例条数为 0 就说明这条检查在空转").toBeGreaterThan(0);
    expect(Number(count?.[2])).toBe(6);
  });

  it("六个未迁移域逐域都有现存违例（规则抓到的是真的，不是永远为真）", () => {
    const json = runJson();
    expect(json.problems).toEqual([]);
    expect(json.total).toBeGreaterThan(0);
    const blocked = json.domains.filter((d) => d.state === "blocked").map((d) => d.featureDir).sort();
    expect(blocked).toEqual(["calls", "content", "discover", "files", "mini", "video"]);
    for (const dir of blocked) {
      const entry = json.ledger[dir];
      expect(entry, `债台账里应当有 features/${dir} 这一格`).toBeTruthy();
      expect(Object.keys(entry?.counts ?? {}).length, `features/${dir} 应该至少有一条现存违例`).toBeGreaterThan(0);
    }
    // 钱包是唯一的已迁移域，它的违例只许是"经公共 request() 走核心"这一种
    const commerceHits = json.violations.filter((v) => v.file === "commerce");
    expect(commerceHits.every((v) => v.rule === "legacy_transport")).toBe(true);
    expect(commerceHits.filter((v) => v.rule !== "legacy_transport")).toEqual([]);
  });

  it("四份 action 名单同源：路由表 ↔ core-contract ↔ Rust 桥（七条，不多不少）", () => {
    const json = runJson();
    const routed = [...new Set(json.routes.map((r) => r.action))].sort();
    expect(routed).toEqual([...new Set(json.contractActions)].sort());
    expect(routed).toEqual([...new Set(json.bridgeArms)].sort());
    expect(routed).toHaveLength(7);
  });

  it("注入证明①：允许名单写成包含全部七域 → 判红", () => {
    const registry = readFileSync(path.join(ROOT, REGISTRY_REL), "utf8");
    const claimAllSeven = registry.replace(/state: "blocked"/g, 'state: "core"');
    expect(claimAllSeven).not.toBe(registry);
    const res = run([], { [REGISTRY_REL]: claimAllSeven });
    expect(res.status).toBe(1);
    expect(`${res.stderr}${res.stdout}`).toContain("FAIL");
    // 报的不是"名单长了"，而是这些域根本没有桥、还在拿遗留会话
    expect(res.stderr).toContain("里仍有遗留会话用法");
    expect(res.stderr).toContain("标成 core 却没有可解析的 providerSha");
    const json = runJson({ [REGISTRY_REL]: claimAllSeven });
    expect(json.problems.length).toBeGreaterThan(10);
  });

  it("注入证明②：钱包改回从 localStorage 取 token → 判红", () => {
    const api = readFileSync(path.join(ROOT, COMMERCE_API_REL), "utf8");
    const leaky = api
      .replace('import { ApiError, idemKey, request } from "../../lib/http";', 'import { ApiError, idemKey, request, token } from "../../lib/http";')
      .concat('\n/** 注入证明用：钱包又去读独立 localStorage 里的 bearer。 */\nexport const _legacyBearerLeak = (): string | null => token();\n');
    expect(leaky).toContain(", token }");
    const res = run([], { [COMMERCE_API_REL]: leaky });
    expect(res.status).toBe(1);
    expect(res.stderr).toContain("已迁移域 features/commerce");
    expect(res.stderr).toContain("legacy_token");
  });

  it("注入证明③：桥少注册一条钱包臂（UI 声称接了但桥没有）→ 判红", () => {
    const bridge = readFileSync(path.join(ROOT, BRIDGE_REL), "utf8");
    const missing = bridge.replace('        "wallet.redpacket.list" => act_wallet_redpacket_list(app, state, core, req),\n', "");
    expect(missing).not.toBe(bridge);
    const res = run([], { [BRIDGE_REL]: missing });
    expect(res.status).toBe(1);
    expect(res.stderr).toContain("与 core_bridge.rs 的 wallet_router 不一致");
  });

  it("注入证明④：债台账里加一条新违例（未迁移域也不能悄悄变差）→ 判红", () => {
    const calls = readFileSync(path.join(ROOT, "apps/desktop-web/src/features/calls/api.ts"), "utf8");
    const worse = calls.concat("\n/** 注入证明用：通话域自己发了一次 HTTP。 */\nexport const _extraFetch = (url: string) => fetch(url);\n");
    const res = run([], { "apps/desktop-web/src/features/calls/api.ts": worse });
    expect(res.status).toBe(1);
    expect(res.stderr).toContain("债台账漂移：features/calls");
  });

  it("未知参数不再被静默忽略（打错 flag 不能等于把门关掉）", () => {
    const res = run(["--config", "whatever.json"]);
    expect(res.status).toBe(2);
    expect(`${res.stderr}${res.stdout}`).toContain("未知参数");
  });

  it("override 入口指到不存在的文件时报错而不是静默按默认跑", () => {
    const res = spawnSync(process.execPath, [CHECK], {
      cwd: ROOT,
      encoding: "utf8",
      env: { ...process.env, WXLIKE_CORE_TRANSPORT_OVERRIDES: path.join(TMP, "nope.json") },
    });
    expect(res.status).not.toBe(0);
    expect(`${res.stderr}${res.stdout}`).toContain("WXLIKE_CORE_TRANSPORT_OVERRIDES");
  });
});
