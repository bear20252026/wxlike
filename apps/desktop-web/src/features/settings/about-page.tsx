import { useEffect, useState } from "react";
import { Github, RefreshCw } from "lucide-react";
import { ACTIONS, newRequest } from "../../shell/core-contract";
import type { AppInfo } from "../../shell/models";
import { useApp, useAppState } from "../../shell/store";
import { useAsync } from "../../shell/hooks";
import { Button, ErrorState, LoadingState, PageHeader } from "../../shell/ui";

const BUILD_FALLBACK: AppInfo = {
  name: "wxlike",
  version: import.meta.env.VITE_APP_VERSION ?? "0.1.0",
  commit: import.meta.env.VITE_GIT_SHA ?? "unknown",
  built_at: import.meta.env.VITE_BUILD_TIME ?? "unknown",
  licenses: [
    { name: "React", license: "MIT", url: "https://react.dev" },
    { name: "Tauri", license: "MIT / Apache-2.0", url: "https://tauri.app" },
    { name: "Tailwind CSS", license: "MIT", url: "https://tailwindcss.com" },
    { name: "lucide", license: "ISC", url: "https://lucide.dev" },
  ],
};

export function AboutPage({ embedded = false }: { embedded?: boolean }) {
  const { core, platform } = useApp();
  const state = useAppState();
  const caps = platform.capabilities();
  const [info, setInfo] = useState<AppInfo | null>(null);

  const response = useAsync<AppInfo>(() => core.dispatch<AppInfo>(newRequest(ACTIONS.appInfo, {})), [core]);

  useEffect(() => {
    if (response.data) setInfo(response.data);
  }, [response.data]);

  const app = info ?? BUILD_FALLBACK;

  return (
    <div className={embedded ? "space-y-4" : "flex h-full min-h-0 flex-col"}>
      {embedded ? (
        <div className="flex justify-end">
          <Button size="sm" onClick={response.reload} pending={response.loading} data-testid="about-reload"><RefreshCw size={12} /> 重新读取</Button>
        </div>
      ) : (
        <PageHeader
          title="关于 wxlike"
          description="G10 桌面与网页核心界面。所有系统能力都经过 PlatformPort，业务数据只来自 Rust 核心投影。"
          actions={<Button size="sm" onClick={response.reload} pending={response.loading} data-testid="about-reload"><RefreshCw size={12} /> 重新读取</Button>}
        />
      )}
      <div className={embedded ? "" : "scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4"}>
        {response.error && !info ? (
          <ErrorState error={response.error} onRetry={response.reload} />
        ) : response.loading && !info ? (
          <LoadingState label="正在读取版本信息…" />
        ) : (
          <div className="space-y-4">
            <section className="flex items-center gap-4 rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
              <span className="grid h-12 w-12 place-items-center rounded-xl bg-[var(--accent)] text-[18px] font-bold text-white">W</span>
              <div className="min-w-0 flex-1">
                <div className="text-[14px] font-semibold text-[var(--ink)]" data-testid="about-name">{app.name}</div>
                <div className="mt-0.5 text-[10px] text-[var(--muted)]" data-testid="about-version">
                  版本 {app.version} · 提交 {app.commit.slice(0, 10)}
                </div>
                <div className="text-[9px] text-[var(--muted)]">构建时间 {app.built_at}</div>
              </div>
              <span className="rounded bg-[var(--surface-3)] px-1.5 py-0.5 text-[8.5px] text-[var(--muted)]">
                {caps.kind === "tauri" ? "桌面宿主" : "浏览器"}
              </span>
            </section>

            <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
              <h2 className="text-[11.5px] font-semibold text-[var(--ink)]">运行环境</h2>
              <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1.5 text-[10px]">
                <dt className="text-[var(--muted)]">核心传输</dt>
                <dd className="text-[var(--ink-2)]">{state.capabilities?.transport ?? "未报告"}</dd>
                <dt className="text-[var(--muted)]">契约版本</dt>
                <dd className="text-[var(--ink-2)]">{state.capabilities?.contract_version ?? "未报告"}</dd>
                <dt className="text-[var(--muted)]">动作数</dt>
                <dd className="text-[var(--ink-2)]">{state.capabilities?.actions.length ?? 0}</dd>
                <dt className="text-[var(--muted)]">账号</dt>
                <dd className="text-[var(--ink-2)]">{state.session.account?.name ?? "未登录"}</dd>
              </dl>
            </section>

            <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
              <h2 className="text-[11.5px] font-semibold text-[var(--ink)]">更新</h2>
              <p className="mt-1 text-[9.5px] leading-4 text-[var(--muted)]" data-testid="about-updater">
                桌面自动更新需要 Tauri updater 插件与签名公钥（由 G00 宿主接入）。当前界面没有“检查更新”按钮，
                以免把未挂载的能力伪装成可用；发行版本信息来自 <code>app.info</code> 或构建注入。
              </p>
            </section>

            <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
              <h2 className="flex items-center gap-1.5 text-[11.5px] font-semibold text-[var(--ink)]">
                <Github size={13} /> 第三方许可
              </h2>
              <ul className="mt-2 divide-y divide-[var(--line)]">
                {app.licenses.map((item) => (
                  <li key={item.name} className="flex items-center justify-between gap-3 py-1.5 text-[10px]">
                    <span className="text-[var(--ink-2)]">{item.name}</span>
                    <span className="text-[var(--muted)]">
                      {item.url ? (
                        <a href={item.url} target="_blank" rel="noreferrer" className="underline decoration-dotted underline-offset-2 hover:text-[var(--ink-2)]">
                          {item.license}
                        </a>
                      ) : item.license}
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          </div>
        )}
      </div>
    </div>
  );
}
