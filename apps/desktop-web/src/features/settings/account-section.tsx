import { useEffect, useState, type ReactNode } from "react";
import { LogOut, LockKeyhole, Monitor, Moon, Sun } from "lucide-react";
import { ACTIONS } from "../../shell/core-contract";
import type { Preferences, ThemePreference } from "../../shell/models";
import { useApp, useAppState, useCoreAction, useToast } from "../../shell/store";
import { usePending } from "../../shell/hooks";
import { Avatar, Button, ConfirmDialog, Switch, cn } from "../../shell/ui";

const THEMES: { id: ThemePreference; label: string; icon: typeof Sun }[] = [
  { id: "light", label: "浅色", icon: Sun },
  { id: "dark", label: "深色", icon: Moon },
  { id: "system", label: "跟随系统", icon: Monitor },
];

function Row({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-[var(--line)] py-2.5 last:border-0">
      <div className="min-w-0">
        <div className="text-[11.5px] text-[var(--ink)]">{title}</div>
        {description ? <div className="mt-0.5 text-[9.5px] leading-4 text-[var(--muted)]">{description}</div> : null}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

export function AccountSection() {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const { platform } = useApp();
  const { pending, run } = usePending();
  const [logoutOpen, setLogoutOpen] = useState(false);
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const caps = platform.capabilities();
  const preferences = state.preferences;

  useEffect(() => {
    if (preferences) document.documentElement.dataset.compact = preferences.compact ? "1" : "0";
  }, [preferences]);

  const setPreference = async <K extends keyof Preferences>(key: K, value: Preferences[K]) => {
    if (busyKey) return;
    setBusyKey(String(key));
    const response = await dispatch(ACTIONS.preferencesSet, { patch: { [key]: value } }, {
      idempotencyKey: `pref:${String(key)}:${JSON.stringify(value)}`,
    });
    setBusyKey(null);
    if (response.ok) toast("设置已保存");
  };

  const logout = async () => {
    setLogoutOpen(false);
    await run(async () => {
      const response = await dispatch(ACTIONS.logout, {});
      if (response.ok) toast("已注销，核心已撤销会话");
    });
  };

  return (
    <div className="space-y-4">
      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
        <div className="flex items-center gap-3">
          <Avatar src={state.session.account?.avatar} name={state.session.account?.name ?? "我"} size={48} rounded="rounded-[10px]" />
          <div className="min-w-0 flex-1">
            <div className="text-[13px] font-semibold text-[var(--ink)]">{state.session.account?.name}</div>
            <div className="mt-0.5 text-[9.5px] text-[var(--muted)]">
              {state.session.account?.user_id} · 设备 {state.session.account?.device_id}
            </div>
            <div className="text-[9px] text-[var(--muted)]">会话 {state.session.account?.session_id}</div>
          </div>
          <div className="flex items-center gap-1.5">
            <Button size="sm" onClick={async () => { await dispatch(ACTIONS.lock, {}); }} data-testid="settings-lock">
              <LockKeyhole size={12} /> 立即锁定
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setLogoutOpen(true)} data-testid="settings-logout">
              <LogOut size={12} /> 退出登录
            </Button>
          </div>
        </div>
      </section>

      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] px-4 py-2">
        <h2 className="py-2 text-[11px] font-semibold text-[var(--ink)]">外观</h2>
        <div className="flex items-center gap-1.5 pb-3">
          {THEMES.map((theme) => (
            <button
              key={theme.id}
              type="button"
              onClick={() => void setPreference("theme", theme.id)}
              className={cn(
                "flex flex-1 items-center justify-center gap-1.5 rounded-md border px-2 py-2 text-[10.5px] transition",
                preferences?.theme === theme.id
                  ? "border-[var(--accent)] bg-[var(--accent-soft)] text-[var(--accent-ink)]"
                  : "border-[var(--line-strong)] text-[var(--ink-2)] hover:bg-[var(--surface-3)]",
              )}
              data-testid={`theme-${theme.id}`}
            >
              <theme.icon size={12} /> {theme.label}
            </button>
          ))}
        </div>
        <Row title="紧凑模式" description="缩小消息间距，适合小窗口。">
          <Switch
            label="紧凑模式"
            checked={Boolean(preferences?.compact)}
            pending={busyKey === "compact"}
            onChange={(value) => void setPreference("compact", value)}
          />
        </Row>
        <Row title="Enter 发送消息" description="关闭后使用 Shift+Enter 发送，Enter 换行。">
          <Switch
            label="Enter 发送消息"
            checked={preferences?.enter_to_send ?? true}
            pending={busyKey === "enter_to_send"}
            onChange={(value) => void setPreference("enter_to_send", value)}
          />
        </Row>
        <Row title="在此设备自动登录" description={caps.secrets.storage === "os_keychain" ? "令牌写入系统密钥库；注销时撤销。" : "当前平台没有系统密钥库，自动登录只能使用会话级策略。"}>
          <Switch
            label="自动登录"
            checked={Boolean(preferences?.auto_login)}
            disabled={caps.secrets.storage !== "os_keychain"}
            pending={busyKey === "auto_login"}
            onChange={(value) => void setPreference("auto_login", value)}
          />
        </Row>
      </section>

      <ConfirmDialog
        open={logoutOpen}
        title="退出登录？"
        description="核心会撤销会话与自动登录令牌；本地 outbox 中未送达的消息会保留到下次登录。"
        confirmLabel="退出登录"
        danger
        pending={pending}
        onConfirm={() => void logout()}
        onClose={() => setLogoutOpen(false)}
      />
    </div>
  );
}
