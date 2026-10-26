import { describe, expect, it } from "vitest";
import type { SessionProjection } from "./models";
import { ROUTES, guardedRoute, parseHash, parseHashDetail, routeToHash, type Route } from "./router";

const session = (phase: SessionProjection["phase"]): SessionProjection =>
  phase === "authenticated"
    ? {
        phase,
        account: {
          user_id: "u1",
          name: "测试",
          avatar: "",
          device_id: "d1",
          session_id: "s1",
        },
      }
    : ({ phase } as SessionProjection);

const samples: Route[] = [
  { id: "login" },
  { id: "qr_login" },
  { id: "lock" },
  { id: "restore" },
  { id: "chats" },
  { id: "chat", convId: "conv-lin" },
  { id: "contacts" },
  { id: "contact", userId: "user-lin" },
  { id: "new_friends" },
  { id: "group", groupId: "group-spring" },
  { id: "search", query: "" },
  { id: "search", query: "设计 稿" },
  { id: "settings" },
  { id: "about" },
  { id: "video" },
  { id: "wallet" },
  { id: "files" },
  { id: "mini" },
  { id: "discover" },
  { id: "moments" },
  { id: "articles" },
  { id: "favorites" },
  { id: "stickers" },
  { id: "calls" },
  { id: "call", convId: "conv-lin" },
];

describe("parseHash / routeToHash 往返", () => {
  it("每条路由都能往返（含参数与中文/特殊字符）", () => {
    for (const route of samples) {
      const hash = routeToHash(route);
      expect(hash.startsWith("#/")).toBe(true);
      expect(parseHash(hash)).toEqual(route);
    }
  });

  it("带参数的路径会被编码再解码", () => {
    expect(parseHash(routeToHash({ id: "chat", convId: "a/b c?x" }))).toEqual({ id: "chat", convId: "a/b c?x" });
    expect(parseHash(routeToHash({ id: "contact", userId: "用户/一" }))).toEqual({ id: "contact", userId: "用户/一" });
    expect(parseHash(routeToHash({ id: "search", query: "春日 & 设计" }))).toEqual({ id: "search", query: "春日 & 设计" });
  });

  it("空哈希与裸 #/ 都落在聊天", () => {
    expect(parseHash("")).toEqual({ id: "chats" });
    expect(parseHash("#/")).toEqual({ id: "chats" });
    expect(parseHashDetail("").known).toBe(true);
  });

  it("未知路径解析为 chats 且标记未知（调用方应替换 URL）", () => {
    const parsed = parseHashDetail("#/nonsense/path");
    expect(parsed.route).toEqual({ id: "chats" });
    expect(parsed.known).toBe(false);
    expect(parseHashDetail("#/video/extra").known).toBe(false);
    expect(parseHashDetail("#/groups").known).toBe(false);
  });

  it("ROUTES 表里的每个 pattern 都能被 parseHash 识别", () => {
    for (const descriptor of ROUTES) {
      const route = parseHash(descriptor.pattern);
      expect(route.id).toBe(descriptor.id);
      expect(parseHashDetail(descriptor.pattern).known).toBe(true);
    }
  });
});

describe("guardedRoute", () => {
  it("未登录访问受保护路由回登录页", () => {
    expect(guardedRoute({ id: "chats" }, session("signed_out"))).toEqual({ id: "login" });
    expect(guardedRoute({ id: "video" }, session("signed_out"))).toEqual({ id: "login" });
    expect(guardedRoute({ id: "moments" }, session("signed_out"))).toEqual({ id: "login" });
  });

  it("锁定态只允许 lock/restore", () => {
    expect(guardedRoute({ id: "chats" }, session("locked"))).toEqual({ id: "lock" });
    expect(guardedRoute({ id: "calls" }, session("locked"))).toEqual({ id: "lock" });
    expect(guardedRoute({ id: "restore" }, session("locked"))).toEqual({ id: "restore" });
  });

  it("已登录访问登录页回到聊天", () => {
    expect(guardedRoute({ id: "login" }, session("authenticated"))).toEqual({ id: "chats" });
    expect(guardedRoute({ id: "qr_login" }, session("authenticated"))).toEqual({ id: "chats" });
    expect(guardedRoute({ id: "wallet" }, session("authenticated"))).toEqual({ id: "wallet" });
  });
});
