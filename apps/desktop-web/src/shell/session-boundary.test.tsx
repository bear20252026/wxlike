/**
 * #294 卡面验收第 3 条：「服务器切换 / 账号切换 / 退出时，各域状态与缓存统一清理，
 * 旧用户任务不能泄露给新会话」。
 *
 * 这条验收的形状是"清理真的发生了"，而清理是否发生只有四种读数说得清：
 *
 * 1. `AppState.sessionBoundary` —— 组合根把每次边界都写成一份读数（reason /
 *    generation / dropped / clearedKeys / clearedDomains），断言因此有东西可比；
 * 2. `AppState` 里各域那一片缓存本身（会话列表 / 消息 / 联系人 / 申请 / 偏好 / 未读 / 二维码）；
 * 3. `localStorage` 里旧用户残留的 bearer / base / 身份（分裂会话的另一半来源）；
 * 4. `coreSessionSnapshot()` 的核心代次与在途台账（旧用户任务被作废的条数）。
 *
 * 这里刻意写成**能红**的样子：每个 `expect` 比的都是具体值（`toBe(5)`、`toEqual([])`、
 * `toEqual(["wxlike-server", …])`），而不是 `?? true` 那种"读不到就算过"。把
 * `store.tsx` 里任意一行清理删掉、或把 `resetCoreSession` 的 localStorage 抹除摘掉，
 * 本文件就红——注入证明见提交信息。
 */

import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ApiError, request, setTransport } from "../lib/http";
import { __resetCoreSessionForTests, coreSessionSnapshot, installCoreSession } from "../lib/core-transport";
import {
  ACTIONS,
  newRequest,
  type ActionRequest,
  type ActionResponse,
  type CoreCapabilities,
  type CoreHandle,
  type EventEnvelope,
} from "./core-contract";
import type { ChatMessage, Contact, Conversation, FriendRequest, Page, Preferences, SessionProjection } from "./models";
import { AppStore } from "./store";
import { createFixtureCore, type FixtureCore } from "./adapters/fixture/fixture-core";
import { loginAs, renderApp } from "../test/helpers";

/** 三条边界都要抹掉的遗留会话键（`lib/http.ts` 的 LEGACY_SESSION_KEYS）。 */
const LEGACY_KEYS = ["wxlike-server", "wxlike-web-token", "wxlike-web-user"] as const;

function seedLegacySession(who = "上一个用户"): void {
  localStorage.setItem("wxlike-web-token", `${who} 的 bearer`);
  localStorage.setItem("wxlike-web-user", who === "上一个用户" ? "user-alpha" : who);
  localStorage.setItem("wxlike-server", "http://127.0.0.1:8099");
}

function envelope(event_type: string, aggregate_id: string, payload: unknown): EventEnvelope {
  return {
    event_id: `evt-${event_type}-${aggregate_id}`,
    aggregate_id,
    aggregate_version: "1",
    event_type,
    server_time_ms: new Date().toISOString(),
    payload,
  };
}

const signedOut = (): EventEnvelope => envelope("session.state", "session", { phase: "signed_out" } satisfies SessionProjection);

/**
 * 用真夹具把 store 喂成一个"正在用某个账号"的样子：工作区四格都有数据，再补上
 * 只有事件流才会带的那两格（消息、好友申请），这样"清了几格"是个可比的整数（5 格）。
 */
async function fillWorkspace(store: AppStore, fixture: FixtureCore): Promise<string[]> {
  await store.loadWorkspace(fixture);
  const conversations = store.getSnapshot().conversations;
  const first = conversations[0];
  expect(first, "夹具里该有会话，否则这条用例在看空集合").toBeTruthy();
  store.applyEvent(
    envelope("message.created", first!.conv_id, {
      msg_id: "msg-legacy-1",
      conv_id: first!.conv_id,
      seq: "999999",
      sender: { user_id: "user-alpha", name: "苏禾" },
      direction: "out",
      body: { kind: "text", text: "上一个用户还没发完的消息" },
      state: "SERVER_ACCEPTED",
      created_at_ms: "2026-10-27T00:00:00.000Z",
    } satisfies ChatMessage),
  );
  store.applyEvent(
    envelope("friend_request.updated", "req-legacy-1", {
      request_id: "req-legacy-1",
      from: { user_id: "user-stranger", name: "陌生人" },
      message: "上一个账号收到的申请",
      status: "PENDING",
      created_at_ms: "2026-10-27T00:00:00.000Z",
    } satisfies FriendRequest),
  );
  store.applyEvent(
    envelope("contact.updated", "user-injected", {
      user_id: "user-injected",
      name: "注入的联系人",
      tags: [],
      star: false,
      online: false,
      blocked: false,
    } satisfies Contact),
  );
  store.applyEvent(
    envelope("preferences.updated", "user-alpha", {
      theme: "dark",
      compact: true,
      notification_preview: false,
      enter_to_send: true,
      sync_range_days: 7,
      auto_login: false,
    } satisfies Preferences),
  );
  return conversations.map((row) => row.conv_id);
}

function expectEverythingCleared(store: AppStore): void {
  const state = store.getSnapshot();
  expect(state.conversations).toEqual([]);
  expect(state.messages).toEqual({});
  expect(state.contacts).toEqual([]);
  expect(state.requests).toEqual([]);
  expect(state.preferences).toBeNull();
  expect(state.unreadTotal).toBe(0);
  expect(state.qr).toBeNull();
}

function expectLegacyStorageGone(): void {
  for (const key of LEGACY_KEYS) expect(localStorage.getItem(key), key).toBeNull();
}

describe("会话边界 — 真渲染 App 走退出：旧用户的痕迹一条都不剩", () => {
  it("设置里退出：回到登录页、遗留 bearer/base/身份被抹掉、核心代次 +1", async () => {
    const { unmount } = renderApp({ fixture: createFixtureCore({ latencyMs: 0 }) });
    await loginAs("user-alpha", "demo1234");
    const user = userEvent.setup();
    await user.click(screen.getByTestId("rail-settings"));
    seedLegacySession();
    const generationBefore = coreSessionSnapshot().generation;

    await user.click(await screen.findByTestId("settings-logout"));
    await user.click(await screen.findByTestId("confirm-action"));

    await waitFor(() => expect(screen.getByTestId("login-username")).toBeInTheDocument());
    // 旧用户的导航与账号信息都不在了（"页面能点"不等于没泄露）
    expect(screen.queryByTestId("rail-chats")).not.toBeInTheDocument();
    expect(screen.queryByTestId("rail-settings")).not.toBeInTheDocument();
    expect(screen.queryByTestId("settings-logout")).not.toBeInTheDocument();
    expect(screen.queryByTestId("conv-conv-lin")).not.toBeInTheDocument();
    expectLegacyStorageGone();
    expect(coreSessionSnapshot().generation).toBe(generationBefore + 1);
    expect(coreSessionSnapshot().pending).toEqual([]);
    unmount();
  });
});

describe("会话边界 — AppStore 的清理读数（能红，不是注释）", () => {
  it("账号切换：五格缓存一起清、遗留 storage 抹掉、代次 +1，重载后只剩新账号的清单", async () => {
    const fixture = createFixtureCore({ latencyMs: 0 });
    const store = new AppStore();
    const unsubscribe = fixture.subscribe(null, (event) => store.applyEvent(event));
    await store.bootstrap(fixture);
    await fixture.dispatch(newRequest(ACTIONS.login, { username: "user-alpha", password: "demo1234" }));
    const alphaConvIds = await fillWorkspace(store, fixture);
    seedLegacySession();
    const generationBefore = coreSessionSnapshot().generation;
    expect(store.getSnapshot().session.account?.user_id).toBe("user-alpha");

    // 同一个进程里换人：核心发的是 session.state（authenticated user-beta）
    await fixture.dispatch(newRequest(ACTIONS.login, { username: "user-beta", password: "demo1234" }));

    const report = store.getSnapshot().sessionBoundary;
    expect(report, "账号切换必须留下一份清理读数，否则界面与用例都无法证明清过").toBeTruthy();
    expect(report!.reason).toBe("account-switch");
    expect(report!.clearedDomains).toBe(5);
    expect(report!.clearedKeys.sort()).toEqual([...LEGACY_KEYS].sort());
    expect(report!.generation).toBe(generationBefore + 1);
    expectEverythingCleared(store);
    expectLegacyStorageGone();
    // 工作区归属被作废：不重置的话，新账号那一格根本不会被重新装载
    expect(store.workspaceFor).toBeNull();

    await store.loadWorkspace(fixture);
    expect(store.workspaceFor).toBe("user-beta");
    const fresh = await fixture.dispatch<Page<Conversation>>(
      newRequest(ACTIONS.conversationsList, { filter: "all", limit: 100 }),
    );
    expect(fresh.ok).toBe(true);
    const betaIds = fresh.ok ? fresh.data.items.map((row) => row.conv_id).sort() : [];
    const cacheIds = store.getSnapshot().conversations.map((row) => row.conv_id).sort();
    // 新会话的缓存 = 服务端给新账号的那一份，旧账号的会话一条都没混进来
    expect(cacheIds).toEqual(betaIds);
    for (const id of alphaConvIds) {
      if (betaIds.includes(id)) continue;
      expect(cacheIds, `旧账号的 ${id} 泄露进了新会话`).not.toContain(id);
    }
    unsubscribe();
  });

  it("退出登录（事件流）：读数与账号切换做满同三件事", async () => {
    const fixture = createFixtureCore({ latencyMs: 0 });
    const store = new AppStore();
    const unsubscribe = fixture.subscribe(null, (event) => store.applyEvent(event));
    await store.bootstrap(fixture);
    await fixture.dispatch(newRequest(ACTIONS.login, { username: "user-alpha", password: "demo1234" }));
    await fillWorkspace(store, fixture);
    seedLegacySession();

    store.applyEvent(signedOut());

    const report = store.getSnapshot().sessionBoundary;
    expect(report!.reason).toBe("logout");
    expect(report!.clearedDomains).toBe(5);
    expect(report!.clearedKeys.sort()).toEqual([...LEGACY_KEYS].sort());
    expectEverythingCleared(store);
    expectLegacyStorageGone();
    expect(store.workspaceFor).toBeNull();
    unsubscribe();
  });

  it("服务器切换：只能走这一条统一入口，清的东西必须一模一样", async () => {
    const fixture = createFixtureCore({ latencyMs: 0 });
    const store = new AppStore();
    const unsubscribe = fixture.subscribe(null, (event) => store.applyEvent(event));
    await store.bootstrap(fixture);
    await fixture.dispatch(newRequest(ACTIONS.login, { username: "user-alpha", password: "demo1234" }));
    await fillWorkspace(store, fixture);
    seedLegacySession();
    const generationBefore = coreSessionSnapshot().generation;

    const report = store.sessionBoundary("server-switch");

    expect(report.reason).toBe("server-switch");
    expect(report.clearedDomains).toBe(5);
    expect(report.clearedKeys.sort()).toEqual([...LEGACY_KEYS].sort());
    expect(report.generation).toBe(generationBefore + 1);
    expect(store.getSnapshot().sessionBoundary).toEqual(report);
    expectEverythingCleared(store);
    expectLegacyStorageGone();
    expect(store.workspaceFor).toBeNull();
    unsubscribe();
  });

  it("锁定：不再是一个账号的活动会话，缓存与身份一起收", async () => {
    const fixture = createFixtureCore({ latencyMs: 0 });
    const store = new AppStore();
    const unsubscribe = fixture.subscribe(null, (event) => store.applyEvent(event));
    await store.bootstrap(fixture);
    await fixture.dispatch(newRequest(ACTIONS.login, { username: "user-alpha", password: "demo1234" }));
    await fillWorkspace(store, fixture);
    seedLegacySession();

    store.applyEvent(envelope("session.state", "user-alpha", { phase: "locked", lock_reason: "manual" } satisfies SessionProjection));

    expect(store.getSnapshot().sessionBoundary?.reason).toBe("lock");
    expect(store.getSnapshot().sessionBoundary?.clearedDomains).toBe(5);
    expectEverythingCleared(store);
    expectLegacyStorageGone();
    unsubscribe();
  });

  it("在途的旧用户任务：被记进 dropped，旧账号的余额一个数字都落不进新会话", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const calls: ActionRequest[] = [];
    const capabilities: CoreCapabilities = {
      contract_version: "test",
      transport: "fixture",
      actions: [ACTIONS.walletBalanceGet],
      qr_login: false,
      secrets_os_backed: false,
      sync_range: false,
    };
    const core: CoreHandle = {
      async dispatch<T>(req: ActionRequest): Promise<ActionResponse<T>> {
        calls.push(req);
        await gate;
        return { ok: true, data: { balance_minor: 999_999, currency: "CNY" } as unknown as T };
      },
      subscribe: () => () => undefined,
      capabilities: async () => capabilities,
    };
    const store = new AppStore();
    installCoreSession(core);
    seedLegacySession();

    const inflight = request<{ balance: number }>("GET", "/api/wallet/balance");
    await Promise.resolve();
    expect(coreSessionSnapshot().pending).toHaveLength(1);

    store.applyEvent(signedOut());

    expect(calls.map((call) => call.action_id)).toEqual([ACTIONS.walletBalanceGet]);
    expect(store.getSnapshot().sessionBoundary?.dropped).toBe(1);
    expect(coreSessionSnapshot().pending).toEqual([]);
    expect(store.getSnapshot().session.phase).toBe("signed_out");

    release();
    const error = (await inflight.catch((err) => err)) as ApiError;
    expect(error).toBeInstanceOf(ApiError);
    expect(error.code).toBe("cancelled");
    expect(error.message).toContain("会话已切换");
    // 泄露的判据：旧账号那笔余额没出现在错误文案里，也没出现在任何状态里
    expect(error.message).not.toContain("999999");
    expect(JSON.stringify(store.getSnapshot())).not.toContain("999999");
    expect(JSON.stringify(coreSessionSnapshot())).not.toContain("999999");
  });

  it("没有边界事件时不许悄悄清：读数只在真的跨过边界时出现", async () => {
    const fixture = createFixtureCore({ latencyMs: 0 });
    const store = new AppStore();
    const unsubscribe = fixture.subscribe(null, (event) => store.applyEvent(event));
    await store.bootstrap(fixture);
    await fixture.dispatch(newRequest(ACTIONS.login, { username: "user-alpha", password: "demo1234" }));
    await fillWorkspace(store, fixture);

    // 同一个人重复投影（登录页刷新、重连）不是边界：缓存不该被清，读数不该出现
    store.applyEvent(
      envelope("session.state", "user-alpha", {
        phase: "authenticated",
        account: { user_id: "user-alpha", name: "苏禾", avatar: "", device_id: "d", session_id: "s" },
      } satisfies SessionProjection),
    );
    expect(store.getSnapshot().sessionBoundary).toBeNull();
    expect(store.getSnapshot().conversations.length).toBeGreaterThan(0);
    unsubscribe();
  });
});

afterEach(() => {
  __resetCoreSessionForTests();
  setTransport(null);
  localStorage.clear();
});

beforeEach(() => {
  __resetCoreSessionForTests();
  setTransport(null);
  localStorage.clear();
});
