import { useEffect, useState } from "react";
import { selectCore, type CoreSelection } from "./shell/adapters";
import { detectPlatform } from "./shell/platform/detect";
import type { PlatformPort } from "./shell/platform/port";
import type { ChatMessage } from "./shell/models";
import { AppProvider, useApp, useAppState, useToast } from "./shell/store";
import { guardedRoute, useRoute, type Route } from "./shell/router";
import { RAIL_ITEMS, ShellFrame, routeTitle, useThemeSync } from "./shell/app-shell";
import { ErrorState, LoadingState, Toasts } from "./shell/ui";
import { LoginPage } from "./features/auth/login-page";
import { QrLoginPage } from "./features/auth/qr-login-page";
import { LockScreen } from "./features/auth/lock-screen";
import { RestorePage } from "./features/auth/restore-page";
import { ChatsPage } from "./features/chat/chat-page";
import { ContactsPage } from "./features/contacts/contacts-page";
import { NewFriendsPage } from "./features/contacts/new-friends";
import { GroupPage } from "./features/contacts/group-page";
import { SearchPage } from "./features/search/search-page";
import { SearchOverlay } from "./features/search/search-overlay";
import { SettingsPage } from "./features/settings/settings-page";
import { AboutPage } from "./features/settings/about-page";
import { ChannelsPage } from "./features/video/ChannelsPage";
import { VideoApi } from "./features/video/api";
import { WalletPage } from "./features/commerce/WalletPage";
import { FilesPage } from "./features/files/FilesPage";
import { FilesApi } from "./features/files/api";
import { MiniAppsPage } from "./features/mini/MiniAppsPage";
import { ServicesPage } from "./features/discover/ServicesPage";
import { MomentsPage } from "./features/content/MomentsPage";
import { ArticlesPage } from "./features/content/ArticlesPage";
import { FavoritesPage } from "./features/content/FavoritesPage";
import { StickerPanel } from "./features/content/StickerPanel";
import { ContentApi } from "./features/content/api";
import { CallPanel } from "./features/calls/CallPanel";
import { navigate } from "./shell/router";
import { DOMAIN_BY_ROUTE, type DomainStatus } from "./lib/domain-registry";

/** Module-level ports so their identity is stable across renders (effects that
 *  depend on `ports` would otherwise re-run every render). */
const VIDEO_PORTS = new VideoApi();
const FILES_PORTS = new FilesApi();
const CONTENT_PORTS = new ContentApi();

function assertNever(value: never): never {
  throw new Error(`unhandled route: ${JSON.stringify(value)}`);
}

function notificationBody(message: ChatMessage, preview: boolean): string {
  if (!preview) return "你收到一条新消息";
  switch (message.body.kind) {
    case "text":
    case "link":
      return message.body.text ?? "你收到一条新消息";
    case "image":
      return "[图片]";
    case "voice":
      return "[语音]";
    case "file":
      return `[文件] ${message.body.name ?? ""}`.trim();
    case "sticker":
      return "[表情]";
    default:
      return "你收到一条新消息";
  }
}

function SessionSplash() {
  return (
    <div className="flex h-full items-center justify-center bg-[var(--app-bg)]" data-testid="session-loading">
      <LoadingState label="正在读取会话投影…" />
    </div>
  );
}

/** 发现页顶部的功能入口：其余域（视频/钱包/文件/小程序/内容/通话）从这里进入。 */
const DISCOVER_LINKS: Array<{ route: Route; label: string; icon: string }> = [
  { route: { id: "moments" }, label: "朋友圈", icon: "🌿" },
  { route: { id: "video" }, label: "视频号", icon: "📺" },
  { route: { id: "wallet" }, label: "钱包", icon: "💰" },
  { route: { id: "files" }, label: "文件", icon: "📁" },
  { route: { id: "mini" }, label: "小程序", icon: "🧩" },
  { route: { id: "calls" }, label: "通话", icon: "📞" },
  { route: { id: "articles" }, label: "公众号", icon: "📰" },
  { route: { id: "favorites" }, label: "收藏", icon: "⭐" },
  { route: { id: "stickers" }, label: "表情", icon: "😀" },
];

function DiscoverPage() {
  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4">
      <section aria-label="功能入口" className="grid grid-cols-3 gap-2" data-testid="discover-links">
        {DISCOVER_LINKS.map((item) => (
          <button
            key={item.route.id}
            type="button"
            onClick={() => navigate(item.route)}
            data-testid={`discover-link-${item.route.id}`}
            className="rounded-xl border border-zinc-200 bg-white p-3 text-center transition hover:border-emerald-300 hover:bg-emerald-50"
          >
            <p className="text-2xl">{item.icon}</p>
            <p className="mt-1 truncate text-xs text-zinc-700">{item.label}</p>
          </button>
        ))}
      </section>
      <ServicesPage />
    </div>
  );
}

/** 通话页：输入会话/房间 ID 后进入真实通话面板（CallPanel 按该 ID 建房）。 */
function CallsPage({ convId }: { convId?: string }) {
  const [roomId, setRoomId] = useState(convId ?? "main");
  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4" data-testid="calls-page">
      <label className="text-xs text-zinc-500">
        会话 / 房间 ID
        <input
          value={roomId}
          onChange={(event) => setRoomId(event.target.value.trim() || "main")}
          aria-label="通话房间 ID"
          data-testid="calls-room-input"
          className="mt-1 w-full rounded-lg border border-zinc-200 px-3 py-2 font-mono text-sm outline-none focus:border-emerald-500"
        />
      </label>
      {roomId ? <CallPanel conv={roomId} /> : null}
    </div>
  );
}

/** 表情页：独立展示表情面板；选中动作以提示反馈（聊天内的选取走 composer）。 */
function StickersPage() {
  const toast = useToast();
  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4" data-testid="stickers-page">
      <StickerPanel ports={CONTENT_PORTS} onPick={(sticker) => toast(`已选择表情 ${sticker}`)} />
    </div>
  );
}

function PageContent({ route }: { route: Route }) {
  switch (route.id) {
    case "chats":
      return <ChatsPage />;
    case "chat":
      return <ChatsPage convId={route.convId} />;
    case "contacts":
      return <ContactsPage />;
    case "contact":
      return <ContactsPage userId={route.userId} />;
    case "new_friends":
      return <NewFriendsPage />;
    case "group":
      return <GroupPage groupId={route.groupId} />;
    case "search":
      return <SearchPage key={route.query} initialQuery={route.query} />;
    case "settings":
      return <SettingsPage />;
    case "about":
      return <AboutPage />;
    case "video":
      return <ChannelsPage ports={VIDEO_PORTS} />;
    case "wallet":
      return <WalletPage />;
    case "files":
      return <FilesPage ports={FILES_PORTS} />;
    case "mini":
      return <MiniAppsPage />;
    case "discover":
      return <DiscoverPage />;
    case "moments":
      return <MomentsPage ports={CONTENT_PORTS} />;
    case "articles":
      return <ArticlesPage ports={CONTENT_PORTS} />;
    case "favorites":
      return <FavoritesPage ports={CONTENT_PORTS} />;
    case "stickers":
      return <StickersPage />;
    case "calls":
      return <CallsPage />;
    case "call":
      return <CallsPage convId={route.convId} />;
    case "login":
    case "qr_login":
    case "lock":
    case "restore":
      // Public screens are rendered by AppView directly, never via PageContent.
      return null;
    default:
      return assertNever(route);
  }
}

/**
 * 域会话状态门（#294）。
 *
 * 卡面写得很硬：**"页面能点"不等于"完成"**。所以七域里每一域的活动页顶部都必须
 * 自己说清"我现在的会话来源是什么"：
 *
 * - 已迁移（钱包）：说明它经认证 Rust 核心，并按当前宿主来源（真核心 / 开发夹具 /
 *   没有核心）如实区分——夹具绝不能伪装成"已接通"；
 * - 未迁移（内容/视频/通话/文件/小程序/发现）：**显式阻塞**，写清缺 provider、
 *   数据不是服务端真值、打包宿主里遗留出口还会被 CSP 拦掉。
 *
 * 文案与 `lib/domain-registry.ts` 同源；用例读的是 `data-testid`，不是截图。
 */
function DomainSessionGate({
  route,
  source,
}: {
  route: Route;
  source: "tauri" | "fixture" | "unavailable";
}) {
  const domain: DomainStatus | undefined = DOMAIN_BY_ROUTE[route.id];
  if (!domain) return null;

  if (domain.state === "core") {
    const text =
      source === "tauri"
        ? `钱包：读写经认证 Rust 核心会话（provider #293 @ ${domain.providerSha}）；Bearer 与服务器地址不进界面`
        : source === "fixture"
          ? `钱包：当前是开发夹具核心（${domain.providerSha ?? "?"} 的桥未挂载），页面数据不连服务端，不作为验收证据`
          : "钱包：本宿主没有 Rust 核心，所有钱包请求都会显式阻塞，不会回落到旧 localStorage 会话";
    return (
      <p
        role="status"
        data-testid={`domain-state-${domain.id}`}
        className={`mx-auto w-full max-w-2xl rounded-lg border px-3 py-2 text-xs ${
          source === "tauri"
            ? "border-emerald-200 bg-emerald-50 text-emerald-800"
            : "border-amber-200 bg-amber-50 text-amber-800"
        }`}
      >
        {text}
      </p>
    );
  }

  return (
    <p
      role="alert"
      data-testid={`domain-blocked-${domain.id}`}
      className="mx-auto w-full max-w-2xl rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-800"
    >
      {domain.label}：未接入认证 Rust 核心（阻塞，非成功）。{domain.reason}；打包宿主里这条遗留 HTTP
      出口还会被宿主 CSP（connect-src 只有 ipc:）拦掉。本页显示的不是服务端真值，不能当验收依据。
    </p>
  );
}

function AuthenticatedShell({
  route,
  onOpenSearch,
  note,
  source,
}: {
  route: Route;
  onOpenSearch: () => void;
  note?: string;
  source: "tauri" | "fixture" | "unavailable";
}) {
  return (
    <ShellFrame route={route} title={routeTitle(route)} onOpenSearch={onOpenSearch} note={note}>
      <>
        <DomainSessionGate route={route} source={source} />
        <PageContent route={route} />
      </>
    </ShellFrame>
  );
}

function AppView() {
  const { core, platform, store, note, source } = useApp();
  const state = useAppState();
  const toast = useToast();
  const { route, go } = useRoute();
  const [searchOpen, setSearchOpen] = useState(false);
  const target = guardedRoute(route, state.session);
  const authenticated = state.session.phase === "authenticated";
  const preview = state.preferences?.notification_preview ?? true;

  useThemeSync();

  useEffect(() => {
    if (target !== route) go(target, { replace: true });
  }, [target, route, go]);

  useEffect(() => {
    if (!authenticated) setSearchOpen(false);
  }, [authenticated]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        if (!authenticated) return;
        event.preventDefault();
        setSearchOpen((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [authenticated]);

  /** Alt+1..N navigates to the rail items, matching the advertised tooltips. */
  useEffect(() => {
    if (!authenticated) return;
    const onKey = (event: KeyboardEvent) => {
      if (!event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
      const code = event.code;
      const match = /^(?:Digit|Numpad)([1-9])$/.exec(code);
      if (!match) return;
      const index = Number(match[1]) - 1;
      const item = RAIL_ITEMS[index];
      if (!item) return;
      event.preventDefault();
      navigate(item.route);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // RAIL_ITEMS is a module constant; navigate is imported and stable.
  }, [authenticated]);

  useEffect(() => {
    if (!authenticated || !platform.capabilities().global_shortcuts.supported) return;
    let disposed = false;
    void platform.shortcuts
      .register("CommandOrControl+K", () => setSearchOpen(true))
      .then((result) => {
        if (!result.ok && !disposed) toast(`全局快捷键未注册：${result.message}`, "error");
      });
    return () => {
      disposed = true;
      void platform.shortcuts.unregister("CommandOrControl+K");
    };
  }, [authenticated, platform, toast]);

  /** Incoming messages notify only when the window is not in the foreground. */
  useEffect(() => {
    if (!authenticated) return;
    const unsubscribe = core.subscribe(null, (event) => {
      if (event.event_type !== "message.created") return;
      const message = event.payload as ChatMessage;
      if (message.direction !== "in" || message.recalled) return;
      const foreground =
        typeof document !== "undefined" && document.visibilityState === "visible" && document.hasFocus();
      if (foreground) return;
      void platform.notifications.show({
        title: message.sender.name,
        body: notificationBody(message, preview),
        silent: false,
      });
    });
    return unsubscribe;
  }, [authenticated, core, platform, preview]);

  useEffect(() => {
    if (!authenticated || !platform.capabilities().tray.supported) return;
    void platform.tray.setBadge(state.unreadTotal);
  }, [authenticated, platform, state.unreadTotal]);

  if (!state.bootstrapped || state.session.phase === "loading" || state.session.phase === "restoring") {
    return <SessionSplash />;
  }

  if (state.session.phase === "error") {
    return (
      <div className="flex h-full flex-col items-center justify-center bg-[var(--app-bg)]" data-testid="session-error">
        <ErrorState
          error={state.session.error ?? {
            code: "DEPENDENCY_UNAVAILABLE",
            message: "核心没有返回会话投影",
            retryable: true,
            request_id: "ui",
          }}
          onRetry={() => void store.bootstrap(core)}
        />
        <p className="max-w-[420px] px-6 text-center text-[9.5px] leading-4 text-[var(--muted)]">
          {note ?? "界面不会在没有会话投影时进入聊天；请确认 Rust 核心与 Tauri 宿主已挂载。"}
        </p>
      </div>
    );
  }

  if (state.session.phase === "locked") {
    return (
      <ShellFrame route={{ id: "lock" }} title="已锁定" onOpenSearch={() => undefined} note={note}>
        <LockScreen />
      </ShellFrame>
    );
  }

  if (state.session.phase === "signed_out") {
    const publicRoute: Route =
      target.id === "qr_login" ? { id: "qr_login" } : target.id === "restore" ? { id: "restore" } : { id: "login" };
    return (
      <ShellFrame route={publicRoute} title={routeTitle(publicRoute)} onOpenSearch={() => undefined} note={note}>
        {publicRoute.id === "qr_login" ? <QrLoginPage /> : publicRoute.id === "restore" ? <RestorePage /> : <LoginPage />}
      </ShellFrame>
    );
  }

  return (
    <>
      <AuthenticatedShell route={target} note={note} source={source} onOpenSearch={() => setSearchOpen(true)} />
      <SearchOverlay open={searchOpen} onClose={() => setSearchOpen(false)} />
    </>
  );
}

export function App({
  selection,
  platform,
}: {
  selection?: CoreSelection;
  platform?: PlatformPort;
} = {}) {
  const [coreSelection] = useState(() => selection ?? selectCore());
  const [platformPort] = useState(() => platform ?? detectPlatform());

  return (
    <AppProvider
      core={coreSelection.core}
      platform={platformPort}
      fixture={coreSelection.fixture}
      source={coreSelection.source}
      note={coreSelection.note}
    >
      <AppView />
      <Toasts />
    </AppProvider>
  );
}
