import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import {
  ACTIONS,
  domainError,
  newRequest,
  type ActionResponse,
  type CoreCapabilities,
  type CoreHandle,
  type DomainError,
  type EventEnvelope,
} from "./core-contract";
import type {
  ChatMessage,
  Contact,
  Conversation,
  FriendRequest,
  Page,
  Preferences,
  QrChallenge,
  SessionProjection,
} from "./models";
import type { FixtureCore } from "./adapters/fixture/fixture-core";
import type { PlatformPort } from "./platform/port";
import { installCoreSession, resetCoreSession, uninstallCoreSession } from "../lib/core-transport";

export interface Toast {
  id: number;
  tone: "info" | "error";
  text: string;
}

/** 会话边界的种类：卡面要求的"服务器切换/账号切换/退出"三条，外加锁定这道复核边界。 */
export type SessionBoundaryReason = "logout" | "account-switch" | "lock" | "server-switch";

/** 一次清理做了哪些事——留在 state 里，好让界面与用例都能读到，不靠注释自证。 */
export interface SessionBoundaryReport {
  reason: SessionBoundaryReason;
  generation: number;
  /** 切换时还在途、因此被作废的旧会话请求条数。 */
  dropped: number;
  /** 实际从 localStorage 抹掉的遗留会话键。 */
  clearedKeys: string[];
  /** 被清掉的各域内存态条目数（会话列表/消息/联系人/申请/偏好）。 */
  clearedDomains: number;
}

export interface AppState {
  bootstrapped: boolean;
  session: SessionProjection;
  capabilities: CoreCapabilities | null;
  preferences: Preferences | null;
  conversations: Conversation[];
  unreadTotal: number;
  messages: Record<string, ChatMessage[]>;
  contacts: Contact[];
  requests: FriendRequest[];
  qr: QrChallenge | null;
  searchEpoch: number;
  toasts: Toast[];
  sessionBoundary: SessionBoundaryReport | null;
}

type Listener = () => void;

const initialState: AppState = {
  bootstrapped: false,
  session: { phase: "loading" },
  capabilities: null,
  preferences: null,
  conversations: [],
  unreadTotal: 0,
  messages: {},
  contacts: [],
  requests: [],
  qr: null,
  searchEpoch: 0,
  toasts: [],
  sessionBoundary: null,
};

function sortedConversations(rows: Conversation[]): Conversation[] {
  return [...rows].sort(
    (a, b) => Number(b.pinned) - Number(a.pinned) || b.last_at_ms.localeCompare(a.last_at_ms),
  );
}

/**
 * 投影里"当前是哪个人"。只有 authenticated 才算有身份——锁定/退出都不再是某个
 * 账号的活动会话，因此也不该继续消费上一个账号的缓存。
 */
function accountOf(projection: SessionProjection): string | null {
  return projection.phase === "authenticated" ? projection.account?.user_id ?? null : null;
}

function mergeById<T>(rows: T[], item: T, key: (row: T) => string): T[] {
  const index = rows.findIndex((row) => key(row) === key(item));
  if (index < 0) return [...rows, item];
  const copy = [...rows];
  copy[index] = item;
  return copy;
}

export class AppStore {
  private state: AppState = initialState;
  private listeners = new Set<Listener>();
  private toastSeq = 0;

  getSnapshot = (): AppState => this.state;

  subscribe = (listener: Listener): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  private set(patch: Partial<AppState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }

  toast(text: string, tone: Toast["tone"] = "info"): void {
    this.toastSeq += 1;
    const toast: Toast = { id: this.toastSeq, tone, text };
    this.set({ toasts: [...this.state.toasts.slice(-2), toast] });
    const id = toast.id;
    setTimeout(() => this.dismissToast(id), tone === "error" ? 6000 : 3500);
  }

  dismissToast(id: number): void {
    this.set({ toasts: this.state.toasts.filter((toast) => toast.id !== id) });
  }

  /**
   * 会话边界：退出 / 换账号 / 锁定 / 服务器切换。这条方法存在的全部意义是
   * "旧用户的状态不会带进新会话"，而且每一件都留了可比的读数：
   *
   * 1. `resetCoreSession()` —— 核心代次 +1，因此上一个账号还在途的钱包请求（转账、
   *    领红包）落地时被丢弃，并且顺手抹掉 localStorage 里残留的 bearer/base/身份；
   * 2. 各域内存态（会话列表/消息/联系人/好友申请/偏好/二维码）整片清空；
   * 3. `sessionBoundary` 读数写进 state，界面与用例都能读到"清了什么、清了几条"。
   *
   * 断言在 `src/shell/session-boundary.test.tsx`（真渲染 App 走退出/切号）与
   * `src/lib/core-transport.test.ts`（代次作废的行为）。注释不算验收。
   */
  sessionBoundary(reason: SessionBoundaryReason): SessionBoundaryReport {
    const current = this.state;
    const clearedDomains =
      (current.conversations.length > 0 ? 1 : 0) +
      (Object.keys(current.messages).length > 0 ? 1 : 0) +
      (current.contacts.length > 0 ? 1 : 0) +
      (current.requests.length > 0 ? 1 : 0) +
      (current.preferences ? 1 : 0);
    const reset = resetCoreSession(reason);
    this.workspaceFor = null;
    this.workspaceFailureNoted = false;
    const report: SessionBoundaryReport = {
      reason,
      generation: reset.generation,
      dropped: reset.dropped,
      clearedKeys: reset.clearedKeys,
      clearedDomains,
    };
    this.set({
      sessionBoundary: report,
      conversations: [],
      messages: {},
      contacts: [],
      requests: [],
      preferences: null,
      unreadTotal: 0,
      qr: null,
    });
    return report;
  }

  applyEvent(event: EventEnvelope): void {
    switch (event.event_type) {
      case "session.state": {
        const projection = event.payload as SessionProjection;
        const previous = accountOf(this.state.session);
        const next = accountOf(projection);
        if (projection.phase === "signed_out") {
          // 退出：先做完整边界清理（核心代次 + 遗留 storage + 各域缓存），再落投影。
          this.sessionBoundary("logout");
          this.set({ session: projection });
        } else if (projection.phase === "locked" && this.state.session.phase === "authenticated") {
          this.sessionBoundary("lock");
          this.set({ session: projection });
        } else if (previous && next && previous !== next) {
          // 同进程里换了一个人：上一个账号的会话/联系人/草稿缓存一条都不能留。
          this.sessionBoundary("account-switch");
          this.set({ session: projection });
        } else {
          this.set({ session: projection });
        }
        return;
      }
      case "preferences.updated":
        this.set({ preferences: event.payload as Preferences });
        return;
      case "conversation.upsert": {
        const conversation = event.payload as Conversation;
        this.set({ conversations: sortedConversations(mergeById(this.state.conversations, conversation, (row) => row.conv_id)) });
        return;
      }
      case "message.created":
      case "message.updated": {
        const message = event.payload as ChatMessage;
        const rows = this.state.messages[message.conv_id] ?? [];
        const next = mergeById(rows, message, (row) => row.msg_id).sort(
          (a, b) => Number(a.seq) - Number(b.seq),
        );
        this.set({ messages: { ...this.state.messages, [message.conv_id]: next } });
        return;
      }
      case "unread.updated": {
        const payload = event.payload as { total?: number };
        this.set({ unreadTotal: payload.total ?? 0 });
        return;
      }
      case "contact.updated": {
        const contact = event.payload as Partial<Contact> & { imported?: boolean };
        if (contact.imported || !contact.user_id) return;
        this.set({ contacts: mergeById(this.state.contacts, contact as Contact, (row) => row.user_id) });
        return;
      }
      case "friend_request.updated": {
        const request = event.payload as FriendRequest;
        this.set({ requests: mergeById(this.state.requests, request, (row) => row.request_id) });
        return;
      }
      case "group.members.updated":
      case "group.updated":
      case "search.invalidated":
        this.set({ searchEpoch: this.state.searchEpoch + 1 });
        return;
      case "qr.state":
        this.set({ qr: event.payload as QrChallenge });
        return;
      default:
        return;
    }
  }

  async bootstrap(core: CoreHandle): Promise<void> {
    let capabilities: CoreCapabilities;
    let projection: ActionResponse<SessionProjection>;
    try {
      [capabilities, projection] = await Promise.all([
        core.capabilities(),
        core.dispatch<SessionProjection>(newRequest(ACTIONS.sessionProjection, {})),
      ]);
    } catch (err) {
      // An unexpected transport/host failure must land on the error screen —
      // an eternal splash (phase "loading") tells the user nothing.
      this.set({
        bootstrapped: true,
        capabilities: null,
        session: {
          phase: "error",
          error: domainError(
            "DEPENDENCY_UNAVAILABLE",
            `会话投影初始化失败：${err instanceof Error ? err.message : String(err)}`,
            "bootstrap",
          ),
        },
      });
      return;
    }
    if (!projection.ok) {
      this.set({
        bootstrapped: true,
        capabilities,
        session: { phase: "error", error: projection.error },
      });
      return;
    }
    this.set({ capabilities, session: projection.data, bootstrapped: true });
    if (projection.data.phase === "authenticated" || projection.data.phase === "locked") {
      await this.loadWorkspace(core);
    }
  }

  async refreshSession(core: CoreHandle): Promise<void> {
    const projection = await core.dispatch<SessionProjection>(newRequest(ACTIONS.sessionProjection, {}));
    if (projection.ok) this.set({ session: projection.data });
  }

  /** Account whose workspace projection is currently loaded; login/logout invalidates it. */
  workspaceFor: string | null = null;

  private workspaceFailureNoted = false;

  async loadWorkspace(core: CoreHandle, accountId?: string): Promise<void> {
    const target =
      accountId ??
      (this.state.session.phase === "authenticated" ? this.state.session.account?.user_id ?? null : null);
    // 投影没有经过 session.state（例如刷新后直接恢复成另一个人）时，这里兜第二道：
    // 要加载的工作区属于另一个账号，先把上一个账号的全部缓存与核心在途请求清掉。
    if (this.workspaceFor !== null && target !== null && this.workspaceFor !== target) {
      this.sessionBoundary("account-switch");
    }
    const [preferences, conversations, contacts, requests] = await Promise.all([
      core.dispatch<Preferences>(newRequest(ACTIONS.preferencesGet, {})),
      core.dispatch<Page<Conversation>>(newRequest(ACTIONS.conversationsList, { filter: "all", limit: 100 })),
      core.dispatch<{ items: Contact[] }>(newRequest(ACTIONS.relationsList, {})),
      core.dispatch<FriendRequest[]>(newRequest(ACTIONS.requestsIncoming, {})),
    ]);
    const failed = [preferences, conversations, contacts, requests].filter((item) => !item.ok);
    // Silent data staleness is not acceptable: surface the FIRST failure once
    // per workspace load instead of pretending the projection is complete.
    if (failed.length > 0 && !this.workspaceFailureNoted) {
      this.workspaceFailureNoted = true;
      const first = failed[0]!;
      this.toast(`工作区数据加载不完整：${first.error.message}`, "error");
    }
    if (failed.length === 0) this.workspaceFailureNoted = false;
    const unreadTotal = conversations.ok
      ? conversations.data.items.reduce((sum, conv) => sum + conv.unread, 0)
      : 0;
    this.set({
      preferences: preferences.ok ? preferences.data : this.state.preferences,
      conversations: conversations.ok ? sortedConversations(conversations.data.items) : this.state.conversations,
      contacts: contacts.ok ? contacts.data.items : this.state.contacts,
      requests: requests.ok ? requests.data : this.state.requests,
      unreadTotal,
    });
    this.workspaceFor = target;
  }

  /** Central error policy: expiry re-checks the session, everything else is surfaced. */
  async handleError(core: CoreHandle, error: DomainError, options: { silent?: boolean } = {}): Promise<void> {
    if (error.code === "UNAUTHENTICATED") {
      await this.refreshSession(core);
      if (!options.silent) this.toast("登录已失效，请重新登录", "error");
      return;
    }
    if (!options.silent) this.toast(error.message, "error");
  }
}

interface AppContextValue {
  store: AppStore;
  core: CoreHandle;
  platform: PlatformPort;
  fixture: FixtureCore | null;
  source: "tauri" | "fixture" | "unavailable";
  note?: string;
}

const AppContext = createContext<AppContextValue | null>(null);

export function AppProvider({
  core,
  platform,
  fixture,
  source,
  note,
  children,
}: {
  core: CoreHandle;
  platform: PlatformPort;
  fixture: FixtureCore | null;
  source: "tauri" | "fixture" | "unavailable";
  note?: string;
  children: ReactNode;
}) {
  const [store] = useState(() => new AppStore());
  const coreRef = useRef(core);
  coreRef.current = core;
  const session = useSyncExternalStore(store.subscribe, () => store.getSnapshot().session);

  useEffect(() => {
    // 组合根是"核心会话"装进公共 transport 的唯一入口：装了之后，已迁移域
    // （钱包）就只能经 CoreHandle 出去；卸载（宿主没了/测试收尾）就退回遗留出口，
    // 而遗留出口对已迁移域本身就是拒绝的（`http.ts` 的 `isCoreOnlyPath`）。
    installCoreSession(core);
    return () => uninstallCoreSession();
  }, [core]);

  useEffect(() => {
    const unsubscribe = core.subscribe(null, (event) => store.applyEvent(event));
    void store.bootstrap(core);
    return unsubscribe;
  }, [core, store]);

  useEffect(() => {
    const accountId = session.phase === "authenticated" ? session.account?.user_id ?? null : null;
    if (!accountId || store.workspaceFor === accountId) return;
    void store.loadWorkspace(core, accountId);
  }, [session, core, store]);

  const value = useMemo<AppContextValue>(
    () => ({ store, core, platform, fixture, source, note }),
    [store, core, platform, fixture, source, note],
  );

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp(): AppContextValue {
  const value = useContext(AppContext);
  if (!value) throw new Error("useApp must be used inside AppProvider");
  return value;
}

export function useAppState(): AppState {
  const { store } = useApp();
  return useSyncExternalStore(store.subscribe, store.getSnapshot);
}

/** Dispatch helper used by every screen; errors are reported once, never swallowed. */
export function useCoreAction() {
  const { core, store } = useApp();
  return useCallback(
    async <T,>(actionId: string, params: unknown, options: { idempotencyKey?: string; silent?: boolean } = {}) => {
      const response = await core.dispatch<T>(newRequest(actionId, params, options.idempotencyKey));
      if (!response.ok) {
        await store.handleError(core, response.error, { silent: options.silent });
      }
      return response;
    },
    [core, store],
  );
}

export function useToast() {
  const { store } = useApp();
  return useCallback((text: string, tone: Toast["tone"] = "info") => store.toast(text, tone), [store]);
}

export function useDismissToast() {
  const { store } = useApp();
  return store.dismissToast.bind(store);
}

export { domainError };
