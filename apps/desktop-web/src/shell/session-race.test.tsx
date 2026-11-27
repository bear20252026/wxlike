/**
 * H-23：桌面工作区加载/刷新会话的代次竞态。
 *
 * 两条必须能红的验收：
 * 1. `loadWorkspace` 的四路请求返回前账号已切换，旧结果不能写进新账号；
 * 2. `refreshSession` 刷新出另一个账号时，旧工作区必须走 `sessionBoundary` 清空。
 */

import { beforeEach, describe, expect, it } from "vitest";
import {
  ACTIONS,
  type ActionRequest,
  type ActionResponse,
  type CoreCapabilities,
  type CoreHandle,
  type EventEnvelope,
} from "./core-contract";
import type { Conversation, Page, Preferences, SessionProjection } from "./models";
import { AppStore } from "./store";
import { __resetCoreSessionForTests } from "../lib/core-transport";

type Deferred = {
  resolve: (value: ActionResponse<unknown>) => void;
};

class DeferredCore implements CoreHandle {
  private pending = new Map<string, Deferred[]>();

  dispatch<T = unknown>(request: ActionRequest): Promise<ActionResponse<T>> {
    return new Promise<ActionResponse<T>>((resolve) => {
      const queue = this.pending.get(request.action_id) ?? [];
      queue.push({ resolve: resolve as (value: ActionResponse<unknown>) => void });
      this.pending.set(request.action_id, queue);
    });
  }

  resolve<T>(action: string, data: T): void {
    const queue = this.pending.get(action);
    const next = queue?.shift();
    if (!next) throw new Error(`no pending ${action}`);
    next.resolve({ ok: true, data });
  }

  pendingCount(action: string): number {
    return this.pending.get(action)?.length ?? 0;
  }

  subscribe(): () => void {
    return () => undefined;
  }

  async capabilities(): Promise<CoreCapabilities> {
    return {
      contract_version: "1",
      transport: "fixture",
      actions: [],
      qr_login: true,
      secrets_os_backed: false,
      sync_range: true,
    };
  }
}

function sessionEvent(projection: SessionProjection): EventEnvelope {
  return {
    event_id: `evt-session-${projection.phase}-${projection.account?.user_id ?? "none"}`,
    aggregate_id: "session",
    aggregate_version: "1",
    event_type: "session.state",
    server_time_ms: "2026-11-27T00:00:00.000Z",
    payload: projection,
  };
}

function conversationEvent(convId: string): EventEnvelope {
  return {
    event_id: `evt-conv-${convId}`,
    aggregate_id: convId,
    aggregate_version: "1",
    event_type: "conversation.upsert",
    server_time_ms: "2026-11-27T00:00:00.000Z",
    payload: {
      conv_id: convId,
      kind: "direct",
      title: "旧账号的会话",
      unread: 0,
      pinned: false,
      muted: false,
      last_preview: "",
      last_at_ms: "2026-11-27T00:00:00.000Z",
      draft: "",
    } satisfies Conversation,
  };
}

function account(user: string): SessionProjection {
  return {
    phase: "authenticated",
    account: {
      user_id: user,
      name: user,
      device_id: `dev-${user}`,
      session_id: `sess-${user}`,
    },
  };
}

function emptyWorkspace(core: DeferredCore): void {
  core.resolve<Page<Conversation>>(ACTIONS.conversationsList, {
    items: [],
    next_cursor: null,
    has_more: false,
  });
  core.resolve<{ items: [] }>(ACTIONS.relationsList, { items: [] });
  core.resolve<unknown[]>(ACTIONS.requestsIncoming, []);
  core.resolve<Preferences | null>(ACTIONS.preferencesGet, null);
}

function alphaConversation(): Page<Conversation> {
  return {
    items: [
      {
        conv_id: "conv-alpha",
        kind: "direct",
        title: "旧账号的会话",
        unread: 0,
        pinned: false,
        muted: false,
        last_preview: "旧数据",
        last_at_ms: "2026-11-27T00:00:00.000Z",
        draft: "",
      },
    ],
    next_cursor: null,
    has_more: false,
  };
}

describe("desktop workspace generation races (H-23)", () => {
  beforeEach(() => {
    __resetCoreSessionForTests();
  });

  it("drops a workspace response that lands after an account boundary", async () => {
    const store = new AppStore();
    const core = new DeferredCore();
    store.applyEvent(sessionEvent(account("user-alpha")));

    const load = store.loadWorkspace(core, "user-alpha");
    expect(core.pendingCount(ACTIONS.conversationsList)).toBe(1);

    // 四路请求还挂着时，会话边界已经切代（模拟另一个账号登录/退出）。
    store.sessionBoundary("account-switch");
    core.resolve<Page<Conversation>>(ACTIONS.conversationsList, alphaConversation());
    core.resolve<{ items: [] }>(ACTIONS.relationsList, { items: [] });
    core.resolve<unknown[]>(ACTIONS.requestsIncoming, []);
    core.resolve<Preferences | null>(ACTIONS.preferencesGet, null);

    await load;
    expect(store.getSnapshot().conversations).toEqual([]);
    expect(store.workspaceFor).toBeNull();
  });

  it("refreshSession clears the old workspace when the projection changes account", async () => {
    const store = new AppStore();
    const core = new DeferredCore();
    store.applyEvent(sessionEvent(account("user-alpha")));
    store.applyEvent(conversationEvent("conv-alpha"));

    const refresh = store.refreshSession(core);
    core.resolve<SessionProjection>(ACTIONS.sessionProjection, account("user-beta"));
    // refreshSession 拿到投影后会进入 loadWorkspace；先让四路请求挂上再喂空数据。
    await Promise.resolve();
    for (const action of [
      ACTIONS.conversationsList,
      ACTIONS.relationsList,
      ACTIONS.requestsIncoming,
      ACTIONS.preferencesGet,
    ]) {
      await Promise.resolve();
      expect(core.pendingCount(action)).toBe(1);
    }
    emptyWorkspace(core);
    await refresh;

    const state = store.getSnapshot();
    expect(state.session.account?.user_id).toBe("user-beta");
    expect(state.sessionBoundary?.reason).toBe("account-switch");
    expect(state.conversations).toEqual([]);
  });
});
