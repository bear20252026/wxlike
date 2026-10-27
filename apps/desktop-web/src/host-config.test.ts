import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * issue #158（G10）：默认 Tauri 宿主必须加载 desktop-web 的产物，并且打包后
 * `core_dispatch` 不能被双 CSP 交集拦掉。
 *
 * 断言逻辑只有一份实现：仓库根的 `scripts/check-tauri-host.mjs`。这里用 vitest
 * 把它跑起来，好让现有的 web-v2 CI 作业（已经会 `npm test`）自动把关——
 * 不需要谁再记着加 CI 步骤。
 *
 * 不带 `--require-built`：本文件跑在 `npm run build` 之前也必须是绿的
 * （产物在不在由 `npm run check:host` 在构建之后把关）。这两句顺序曾经反了，
 * 于是 web-v2 作业红过一次。
 */
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "..", "..", "..");
const CHECK = path.join(ROOT, "scripts", "check-tauri-host.mjs");
const CONFIG = path.join(ROOT, "ui", "src-tauri", "tauri.conf.json");
const OLD_UI_CONFIG = path.join(ROOT, "ui", "src-tauri", "tauri.conf.old-ui.json");

/** CSP 串里 connect-src 的 source 列表（排序后好做等值断言）。 */
function connectSrcOf(csp: string | undefined): string[] {
  return (
    (csp ?? "")
      .split(";")
      .map((part) => part.trim().split(/\s+/).filter(Boolean))
      .find((tokens) => tokens[0]?.toLowerCase() === "connect-src")
      ?.slice(1)
      .map((source) => source.toLowerCase())
      .sort() ?? []
  );
}

function runCheck(): string {
  try {
    return execFileSync(process.execPath, [CHECK], {
      cwd: ROOT,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (err) {
    const out = `${(err as { stdout?: string }).stdout ?? ""}${(err as { stderr?: string }).stderr ?? ""}`;
    throw new Error(`scripts/check-tauri-host.mjs 失败：\n${out}`, { cause: err });
  }
}

describe("桌面宿主默认配置（issue #158）", () => {
  it("默认 frontendDist 指向 desktop-web 产物，不是旧 UI 的 ui/dist", () => {
    const conf = JSON.parse(readFileSync(CONFIG, "utf8")) as {
      build?: { frontendDist?: string };
      app?: { security?: { csp?: string } };
    };
    expect(conf.build?.frontendDist).toBe("../../apps/desktop-web/dist");
    // 桥独占：connect-src 必须恰好是 ipc: 与 http://ipc.localhost，一个野口子都不多。
    // 七个 HTTP 域要迁到 core_dispatch（#293/#294），不是靠放宽这里。
    expect(connectSrcOf(conf.app?.security?.csp)).toEqual(["http://ipc.localhost", "ipc:"]);
  });

  it("旧 UI 仍可从 tauri.conf.old-ui.json 打包（逃生阀没被拆掉）", () => {
    const conf = JSON.parse(readFileSync(OLD_UI_CONFIG, "utf8")) as { build?: { frontendDist?: string } };
    expect(conf.build?.frontendDist).toBe("../dist");
  });

  it("守卫脚本判定宿主/前端双 CSP 交集仍允许 Rust 桥", () => {
    expect(runCheck()).toContain("OK:");
  });

  /**
   * 守卫必须把「打包时真正生效的每份宿主配置」都算进去。两个桌面作业都是
   * `tauri build --config <override.json>`，Tauri 对 override 做深合并，
   * 所以只看 tauri.conf.json 时，往 override 里塞一句 `app.security.csp`
   * 就能换掉线上策略而让守卫照旧报绿。这条用例钉住"核对份数"这个输出，
   * 少了任何一份 override 都会红。
   */
  it("守卫逐个核对打包时会生效的宿主配置（含两个作业的 --config override）", () => {
    const out = runCheck();
    expect(out).toContain("本次核对：3 份宿主配置");
    for (const rel of ["tauri.bundle.override.json", "tauri.release.override.json"]) {
      expect(out).toContain(rel);
    }
  });

  it("未知参数不再被静默忽略（打错 flag 不能等于把门关掉）", () => {
    const res = spawnSync(process.execPath, [CHECK, "--config", "whatever.json"], {
      cwd: ROOT,
      encoding: "utf8",
    });
    expect(res.status).toBe(2);
    expect(`${res.stderr}${res.stdout}`).toContain("未知参数");
  });
});
