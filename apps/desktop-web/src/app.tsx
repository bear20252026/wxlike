import { useEffect, useState } from "react";
import { selectCore, type CoreSelection } from "./shell/adapters";
import { detectPlatform } from "./shell/platform/detect";
import type { PlatformPort } from "./shell/platform/port";
import type { ChatMessage } from "./shell/models";
import { AppProvider, useApp, useAppState, useToast } from "./shell/store";
import { guardedRoute, useRoute, type Route } from "./shell/router";
import { ShellFrame, routeTitle, useThemeSync } from "./shell/app-shell";
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
    default:
      return <ChatsPage />;
  }
}

function AuthenticatedShell({
  route,
  onOpenSearch,
  note,
}: {
  route: Route;
  onOpenSearch: () => void;
  note?: string;
}) {
  return (
    <ShellFrame route={route} title={routeTitle(route)} onOpenSearch={onOpenSearch} note={note}>
      <PageContent route={route} />
    </ShellFrame>
  );
}

function AppView() {
  const { core, platform, store, note } = useApp();
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
      <AuthenticatedShell route={target} note={note} onOpenSearch={() => setSearchOpen(true)} />
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
