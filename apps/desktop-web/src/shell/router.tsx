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
  | { id: "about" };

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
];

export function parseHash(hash: string): Route {
  const raw = hash.replace(/^#/, "") || "/chats";
  const [pathPart, queryPart] = raw.split("?");
  const parts = pathPart.split("/").filter(Boolean);
  const query = new URLSearchParams(queryPart ?? "");

  if (parts.length === 0) return { id: "chats" };
  if (parts[0] === "login") {
    if (parts[1] === "qr") return { id: "qr_login" };
    return { id: "login" };
  }
  if (parts[0] === "lock") return { id: "lock" };
  if (parts[0] === "restore") return { id: "restore" };
  if (parts[0] === "chats") {
    if (parts[1]) return { id: "chat", convId: decodeURIComponent(parts[1]) };
    return { id: "chats" };
  }
  if (parts[0] === "contacts") {
    if (parts[1] === "new") return { id: "new_friends" };
    if (parts[1]) return { id: "contact", userId: decodeURIComponent(parts[1]) };
    return { id: "contacts" };
  }
  if (parts[0] === "groups" && parts[1]) {
    return { id: "group", groupId: decodeURIComponent(parts[1]) };
  }
  if (parts[0] === "search") return { id: "search", query: query.get("q") ?? "" };
  if (parts[0] === "settings") return { id: "settings" };
  if (parts[0] === "about") return { id: "about" };
  return { id: "chats" };
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
    const onChange = () => setRoute(parseHash(window.location.hash));
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
