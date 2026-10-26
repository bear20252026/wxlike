import { useCallback, useEffect, useState } from "react";
import type { SessionProjection } from "./models";

export type Route =
  | { id: "login" }
  | { id: "qr_login" }
  | { id: "lock" }
  | { id: "restore" }
  | { id: "chats" }
  | { id: "chat"; convId: string }
  | { id: "contacts" }
  | { id: "contact"; userId: string }
  | { id: "new_friends" }
  | { id: "group"; groupId: string }
  | { id: "search"; query: string }
  | { id: "settings" }
  | { id: "about" }
  | { id: "video" }
  | { id: "wallet" }
  | { id: "files" }
  | { id: "mini" }
  | { id: "discover" }
  | { id: "moments" }
  | { id: "articles" }
  | { id: "favorites" }
  | { id: "stickers" }
  | { id: "calls" }
  | { id: "call"; convId: string };

export interface RouteDescriptor {
  id: Route["id"];
  pattern: string;
  title: string;
  access: "public" | "authenticated";
}

/** Route table is the single source for navigation, guards and window titles. */
export const ROUTES: RouteDescriptor[] = [
  { id: "login", pattern: "#/login", title: "登录", access: "public" },
  { id: "qr_login", pattern: "#/login/qr", title: "扫码登录", access: "public" },
  { id: "lock", pattern: "#/lock", title: "已锁定", access: "authenticated" },
  { id: "restore", pattern: "#/restore", title: "恢复会话", access: "public" },
  { id: "chats", pattern: "#/chats", title: "聊天", access: "authenticated" },
  { id: "chat", pattern: "#/chats/:convId", title: "会话", access: "authenticated" },
  { id: "contacts", pattern: "#/contacts", title: "通讯录", access: "authenticated" },
  { id: "contact", pattern: "#/contacts/:userId", title: "联系人", access: "authenticated" },
  { id: "new_friends", pattern: "#/contacts/new", title: "新的朋友", access: "authenticated" },
  { id: "group", pattern: "#/groups/:groupId", title: "群聊", access: "authenticated" },
  { id: "search", pattern: "#/search", title: "搜索", access: "authenticated" },
  { id: "settings", pattern: "#/settings", title: "设置", access: "authenticated" },
  { id: "about", pattern: "#/about", title: "关于", access: "authenticated" },
  { id: "video", pattern: "#/video", title: "视频号", access: "authenticated" },
  { id: "wallet", pattern: "#/wallet", title: "钱包", access: "authenticated" },
  { id: "files", pattern: "#/files", title: "文件", access: "authenticated" },
  { id: "mini", pattern: "#/mini", title: "小程序", access: "authenticated" },
  { id: "discover", pattern: "#/discover", title: "发现", access: "authenticated" },
  { id: "moments", pattern: "#/moments", title: "朋友圈", access: "authenticated" },
  { id: "articles", pattern: "#/moments/articles", title: "公众号", access: "authenticated" },
  { id: "favorites", pattern: "#/moments/favorites", title: "收藏", access: "authenticated" },
  { id: "stickers", pattern: "#/moments/stickers", title: "表情", access: "authenticated" },
  { id: "calls", pattern: "#/calls", title: "通话", access: "authenticated" },
  { id: "call", pattern: "#/calls/:convId", title: "通话", access: "authenticated" },
];

export interface ParsedHash {
  route: Route;
  /** False when the hash matched no route and fell back to #/chats. */
  known: boolean;
}

function assertNever(value: never): never {
  throw new Error(`unhandled route: ${JSON.stringify(value)}`);
}

export function parseHashDetail(hash: string): ParsedHash {
  const raw = hash.replace(/^#/, "") || "/chats";
  const [pathPart, queryPart] = raw.split("?");
  const parts = pathPart.split("/").filter(Boolean);
  const query = new URLSearchParams(queryPart ?? "");

  if (parts.length === 0) return { route: { id: "chats" }, known: true };
  switch (parts[0]) {
    case "login":
      if (parts[1] === "qr") return { route: { id: "qr_login" }, known: true };
      return { route: { id: "login" }, known: parts.length === 1 };
    case "lock":
      return { route: { id: "lock" }, known: parts.length === 1 };
    case "restore":
      return { route: { id: "restore" }, known: parts.length === 1 };
    case "chats":
      if (parts[1]) return { route: { id: "chat", convId: decodeURIComponent(parts[1]) }, known: true };
      return { route: { id: "chats" }, known: true };
    case "contacts":
      if (parts[1] === "new") return { route: { id: "new_friends" }, known: true };
      if (parts[1]) return { route: { id: "contact", userId: decodeURIComponent(parts[1]) }, known: true };
      return { route: { id: "contacts" }, known: true };
    case "groups":
      if (parts[1]) return { route: { id: "group", groupId: decodeURIComponent(parts[1]) }, known: true };
      return { route: { id: "chats" }, known: false };
    case "search":
      return { route: { id: "search", query: query.get("q") ?? "" }, known: parts.length === 1 };
    case "settings":
      return { route: { id: "settings" }, known: parts.length === 1 };
    case "about":
      return { route: { id: "about" }, known: parts.length === 1 };
    case "video":
      return { route: { id: "video" }, known: parts.length === 1 };
    case "wallet":
      return { route: { id: "wallet" }, known: parts.length === 1 };
    case "files":
      return { route: { id: "files" }, known: parts.length === 1 };
    case "mini":
      return { route: { id: "mini" }, known: parts.length === 1 };
    case "discover":
      return { route: { id: "discover" }, known: parts.length === 1 };
    case "moments":
      if (parts[1] === "articles") return { route: { id: "articles" }, known: true };
      if (parts[1] === "favorites") return { route: { id: "favorites" }, known: true };
      if (parts[1] === "stickers") return { route: { id: "stickers" }, known: true };
      return { route: { id: "moments" }, known: parts.length === 1 };
    case "calls":
      if (parts[1]) return { route: { id: "call", convId: decodeURIComponent(parts[1]) }, known: true };
      return { route: { id: "calls" }, known: true };
    default:
      // Unknown hash: resolve to the default route; the caller replaces the
      // stale URL with the canonical hash instead of silently rendering chats.
      return { route: { id: "chats" }, known: false };
  }
}

export function parseHash(hash: string): Route {
  return parseHashDetail(hash).route;
}

export function routeToHash(route: Route): string {
  switch (route.id) {
    case "login":
      return "#/login";
    case "qr_login":
      return "#/login/qr";
    case "lock":
      return "#/lock";
    case "restore":
      return "#/restore";
    case "chats":
      return "#/chats";
    case "chat":
      return `#/chats/${encodeURIComponent(route.convId)}`;
    case "contacts":
      return "#/contacts";
    case "contact":
      return `#/contacts/${encodeURIComponent(route.userId)}`;
    case "new_friends":
      return "#/contacts/new";
    case "group":
      return `#/groups/${encodeURIComponent(route.groupId)}`;
    case "search":
      return route.query ? `#/search?q=${encodeURIComponent(route.query)}` : "#/search";
    case "settings":
      return "#/settings";
    case "about":
      return "#/about";
    case "video":
      return "#/video";
    case "wallet":
      return "#/wallet";
    case "files":
      return "#/files";
    case "mini":
      return "#/mini";
    case "discover":
      return "#/discover";
    case "moments":
      return "#/moments";
    case "articles":
      return "#/moments/articles";
    case "favorites":
      return "#/moments/favorites";
    case "stickers":
      return "#/moments/stickers";
    case "calls":
      return "#/calls";
    case "call":
      return `#/calls/${encodeURIComponent(route.convId)}`;
    default:
      return assertNever(route);
  }
}

export function navigate(route: Route, options: { replace?: boolean } = {}): void {
  const target = routeToHash(route);
  if (options.replace) {
    const url = `${window.location.pathname}${window.location.search}${target}`;
    window.history.replaceState(null, "", url);
    window.dispatchEvent(new HashChangeEvent("hashchange"));
    return;
  }
  window.location.hash = target;
}

export function useRoute(): { route: Route; go: (route: Route, options?: { replace?: boolean }) => void } {
  const [route, setRoute] = useState<Route>(() => parseHash(window.location.hash));
  useEffect(() => {
    const onChange = () => {
      const parsed = parseHashDetail(window.location.hash);
      setRoute(parsed.route);
      if (!parsed.known) {
        // Correct the URL (replace, no history entry) so reloads and shares
        // land on the canonical route rather than an opaque path.
        navigate(parsed.route, { replace: true });
      }
    };
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  const go = useCallback((next: Route, options?: { replace?: boolean }) => {
    setRoute(next);
    navigate(next, options);
  }, []);
  useEffect(() => {
    const descriptor = ROUTES.find((item) => item.id === route.id);
    if (descriptor) document.title = `${descriptor.title} · wxlike`;
  }, [route.id]);
  return { route, go };
}

/** Guard: returns the route the shell must show for the current session. */
export function guardedRoute(route: Route, session: SessionProjection): Route {
  const descriptor = ROUTES.find((item) => item.id === route.id);
  if (session.phase === "loading" || session.phase === "restoring") return route;
  if (session.phase === "signed_out" && descriptor?.access === "authenticated") {
    return { id: "login" };
  }
  if (session.phase === "locked" && route.id !== "lock" && route.id !== "restore") {
    return { id: "lock" };
  }
  if (session.phase === "authenticated" && (route.id === "login" || route.id === "qr_login")) {
    return { id: "chats" };
  }
  return route;
}
