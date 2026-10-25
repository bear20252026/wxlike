import { useEffect, useMemo, type ReactNode } from "react";
import {
  ChevronRight,
  Lock,
  MessageCircle,
  Minus,
  Moon,
  Search,
  Settings,
  Square,
  Sun,
  UserPlus,
  Users,
  X,
} from "lucide-react";
import type { Route } from "./router";
import { ROUTES } from "./router";
import { useApp, useAppState, useCoreAction, useToast } from "./store";
import { ACTIONS } from "./core-contract";
import { Avatar, Badge, IconButton, cn } from "./ui";
import { navigate } from "./router";

const railItems = [
  { id: "chats", label: "聊天", icon: MessageCircle, key: "Alt+1" },
  { id: "contacts", label: "通讯录", icon: Users, key: "Alt+2" },
  { id: "new_friends", label: "新的朋友", icon: UserPlus, key: "Alt+3" },
  { id: "search", label: "搜索", icon: Search, key: "Alt+4" },
  { id: "settings", label: "设置", icon: Settings, key: "Alt+5" },
] as const;

function isRailRoute(id: string): boolean {
  return ["chats", "chat", "contacts", "contact", "new_friends", "group", "search", "settings", "about"].includes(id);
}

function railActive(itemId: string, route: Route): boolean {
  if (itemId === "chats") return route.id === "chats" || route.id === "chat" || route.id === "group";
  if (itemId === "contacts") return route.id === "contacts" || route.id === "contact";
  return route.id === itemId;
}

export function WindowControls() {
  const { platform } = useApp();
  const toast = useToast();
  const caps = platform.capabilities();

  const report = async (promise: Promise<{ ok: boolean; message?: string }>) => {
    const result = await promise;
    if (!result.ok) toast(result.message ?? "系统未执行该操作", "error");
  };

  return (
    <div className="group flex items-center gap-[7px]" role="group" aria-label="窗口控制">
      <button
        type="button"
        aria-label="最小化"
        title={caps.window_controls.supported ? "最小化" : caps.window_controls.reason}
        onClick={() => void report(platform.window.minimize())}
        className="grid h-[11px] w-[11px] place-items-center rounded-full bg-[#FF5F57] shadow-[inset_0_0_2px_rgba(0,0,0,0.15)]"
      >
        <Minus size={7} className="text-black/50 opacity-0 transition group-hover:opacity-100" strokeWidth={3} />
      </button>
      <button
        type="button"
        aria-label="最大化"
        title={caps.window_controls.supported ? "最大化 / 还原" : caps.window_controls.reason}
        onClick={() => void report(platform.window.toggleMaximize())}
        className="grid h-[11px] w-[11px] place-items-center rounded-full bg-[#FEBC2E] shadow-[inset_0_0_2px_rgba(0,0,0,0.15)]"
      >
        <Square size={6} className="text-black/50 opacity-0 transition group-hover:opacity-100" strokeWidth={3} />
      </button>
      <button
        type="button"
        aria-label="关闭"
        title={caps.window_controls.supported ? "关闭" : caps.window_controls.reason}
        onClick={() => void report(platform.window.close())}
        className="grid h-[11px] w-[11px] place-items-center rounded-full bg-[#28C840] shadow-[inset_0_0_2px_rgba(0,0,0,0.15)]"
      >
        <X size={7} className="text-black/50 opacity-0 transition group-hover:opacity-100" strokeWidth={3} />
      </button>
    </div>
  );
}

export function TitleBar({
  title,
  onOpenSearch,
}: {
  title: string;
  onOpenSearch: () => void;
}) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const theme = state.preferences?.theme ?? "system";
  const nextTheme = theme === "light" ? "dark" : theme === "dark" ? "system" : "light";
  const themeLabel = theme === "light" ? "浅色" : theme === "dark" ? "深色" : "跟随系统";

  return (
    <header className="flex h-[36px] shrink-0 items-center gap-3 border-b border-[var(--line)] bg-[var(--surface-2)] px-3.5">
      <WindowControls />
      <div className="mx-auto flex items-center gap-1.5 text-[10px] text-[var(--muted)]">
        <span className="grid h-4 w-4 place-items-center rounded-[4px] bg-[var(--accent)] text-[9px] font-bold text-white">
          W
        </span>
        <span className="font-medium text-[var(--ink-2)]">wxlike</span>
        <ChevronRight size={9} />
        <span>{title}</span>
        {state.session.account ? (
          <>
            <ChevronRight size={9} />
            <span>{state.session.account.name}</span>
          </>
        ) : null}
      </div>
      <div className="flex items-center gap-1.5">
        <IconButton
          label={`主题：${themeLabel}（切换为${nextTheme === "light" ? "浅色" : nextTheme === "dark" ? "深色" : "跟随系统"}）`}
          onClick={() => void dispatch(ACTIONS.preferencesSet, { patch: { theme: nextTheme } })}
          data-testid="theme-toggle"
        >
          {theme === "dark" ? <Moon size={14} /> : theme === "light" ? <Sun size={14} /> : <Sun size={14} className="opacity-60" />}
        </IconButton>
        <button
          type="button"
          onClick={onOpenSearch}
          className="flex h-6 items-center gap-1.5 rounded-md border border-[var(--line-strong)] bg-[var(--surface)] px-2 text-[9.5px] text-[var(--muted)] transition hover:bg-[var(--surface-3)] hover:text-[var(--ink-2)]"
          data-testid="quick-search"
        >
          <Search size={11} />
          搜索
          <kbd className="rounded bg-[var(--surface-3)] px-1 text-[8px]">⌘K</kbd>
        </button>
      </div>
    </header>
  );
}

export function Rail({ route }: { route: Route }) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const pendingRequests = state.requests.filter((item) => item.status === "PENDING").length;

  const lock = async () => {
    const result = await dispatch(ACTIONS.lock, {});
    if (result.ok) navigate({ id: "lock" });
    else toast("锁定失败，请稍后再试", "error");
  };

  return (
    <nav className="flex w-[64px] shrink-0 flex-col items-center bg-[var(--rail)] py-3" aria-label="主导航">
      <button
        type="button"
        title={state.session.account ? `${state.session.account.name} · 打开设置` : "账号"}
        onClick={() => navigate({ id: "settings" })}
        className="mb-4 rounded-[9px] ring-1 ring-white/15 transition hover:ring-white/35"
        data-testid="rail-account"
      >
        <Avatar src={state.session.account?.avatar} name={state.session.account?.name ?? "我"} size={36} rounded="rounded-[9px]" />
      </button>
      <div className="flex w-full flex-col items-center gap-1">
        {railItems.map((item) => {
          const active = railActive(item.id, route);
          const count = item.id === "chats" ? state.unreadTotal : item.id === "new_friends" ? pendingRequests : 0;
          return (
            <button
              key={item.id}
              type="button"
              title={`${item.label}（${item.key}）`}
              aria-current={active ? "page" : undefined}
              onClick={() => navigate({ id: item.id } as Route)}
              className={cn(
                "group relative grid h-10 w-10 place-items-center rounded-[11px] transition",
                active
                  ? "bg-white/[0.14] text-[var(--accent)]"
                  : "text-white/45 hover:bg-white/[0.07] hover:text-white/85",
              )}
              data-testid={`rail-${item.id}`}
            >
              {active ? (
                <span className="absolute -left-[12px] h-5 w-[3px] rounded-r-full bg-[var(--accent)]" />
              ) : null}
              <item.icon size={19} strokeWidth={active ? 1.95 : 1.65} />
              {count > 0 ? (
                <span className="absolute right-[4px] top-[4px]">
                  <Badge count={count} className="h-[15px] min-w-[15px] text-[8px] ring-2 ring-[#282828]" />
                </span>
              ) : null}
            </button>
          );
        })}
      </div>
      <div className="mt-auto flex flex-col items-center gap-1">
        <span className="mb-1 h-px w-8 bg-white/10" />
        <button
          type="button"
          title="锁定并清除内存中的凭据"
          onClick={() => void lock()}
          className="grid h-9 w-9 place-items-center rounded-[10px] text-white/40 transition hover:bg-white/[0.07] hover:text-white/80"
          data-testid="rail-lock"
        >
          <Lock size={16} />
        </button>
      </div>
    </nav>
  );
}

export function StatusBar({ note }: { note?: string }) {
  const state = useAppState();
  const { platform } = useApp();
  const caps = useMemo(() => platform.capabilities(), [platform]);
  const days = state.preferences?.sync_range_days ?? 7;
  const range = days === 0 ? "全部历史" : `近 ${days} 天`;
  const platformText = caps.kind === "tauri" ? "桌面宿主" : "浏览器";

  return (
    <footer className="flex h-[24px] shrink-0 items-center gap-3 border-t border-[var(--line)] bg-[var(--surface-2)] px-3 text-[9px] text-[var(--muted)]">
      <span className="flex items-center gap-1.5">
        <span className={cn("h-1.5 w-1.5 rounded-full", note ? "bg-[var(--warn)]" : "bg-[var(--accent)]")} />
        {note ?? "Rust 核心投影"}
      </span>
      <span className="hidden sm:inline">{platformText}</span>
      <span>同步范围：{range}</span>
      <span className="ml-auto tabular-nums">
        {state.session.account ? state.session.account.name : "未登录"}
        {state.session.account ? ` · ${state.session.account.device_id}` : ""}
      </span>
    </footer>
  );
}

/** Applies the persisted theme preference to <html data-theme>. */
export function useThemeSync(): void {
  const state = useAppState();
  const preference = state.preferences?.theme ?? "system";
  useEffect(() => {
    const media = window.matchMedia?.("(prefers-color-scheme: dark)");
    const apply = () => {
      const resolved = preference === "system" ? (media?.matches ? "dark" : "light") : preference;
      document.documentElement.dataset.theme = resolved;
    };
    apply();
    media?.addEventListener?.("change", apply);
    return () => media?.removeEventListener?.("change", apply);
  }, [preference]);
}

/** Hides the rail on the public auth screens, keeps the shell geometry. */
export function ShellFrame({
  route,
  title,
  onOpenSearch,
  note,
  children,
}: {
  route: Route;
  title: string;
  onOpenSearch: () => void;
  note?: string;
  children: ReactNode;
}) {
  const state = useAppState();
  const showRail = state.session.phase === "authenticated" && isRailRoute(route.id);
  return (
    <div className="flex h-full min-h-0 flex-col bg-[var(--app-bg)] text-[var(--ink)]">
      {note ? (
        <div
          className="flex h-[22px] shrink-0 items-center justify-center gap-2 bg-[var(--warn)]/12 text-[9px] text-[var(--warn)]"
          data-testid="source-banner"
          role="note"
        >
          {note}
        </div>
      ) : null}
      <TitleBar title={title} onOpenSearch={onOpenSearch} />
      <div className="flex min-h-0 flex-1">
        {showRail ? <Rail route={route} /> : null}
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {children}
          <StatusBar note={note} />
        </div>
      </div>
    </div>
  );
}

export function routeTitle(route: Route): string {
  return ROUTES.find((item) => item.id === route.id)?.title ?? "wxlike";
}
