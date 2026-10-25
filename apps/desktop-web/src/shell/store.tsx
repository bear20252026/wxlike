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

export interface Toast {
  id: number;
  tone: "info" | "error";
  text: string;
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
};

function sortedConversations(rows: Conversation[]): Conversation[] {
  return [...rows].sort(
    (a, b) => Number(b.pinned) - Number(a.pinned) || b.last_at_ms.localeCompare(a.last_at_ms),
  );
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

  applyEvent(event: EventEnvelope): void {
    switch (event.event_type) {
      case "session.state": {
        const projection = event.payload as SessionProjection;
        if (projection.phase === "signed_out") {
          this.workspaceFor = null;
          this.set({
            session: projection,
            conversations: [],
            messages: {},
            contacts: [],
            requests: [],
            preferences: null,
            unreadTotal: 0,
            qr: null,
          });
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
    const [capabilities, projection] = await Promise.all([
      core.capabilities(),
      core.dispatch<SessionProjection>(newRequest(ACTIONS.sessionProjection, {})),
    ]);
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

  async loadWorkspace(core: CoreHandle, accountId?: string): Promise<void> {
    const target =
      accountId ??
      (this.state.session.phase === "authenticated" ? this.state.session.account?.user_id ?? null : null);
    const [preferences, conversations, contacts, requests] = await Promise.all([
      core.dispatch<Preferences>(newRequest(ACTIONS.preferencesGet, {})),
      core.dispatch<Page<Conversation>>(newRequest(ACTIONS.conversationsList, { filter: "all", limit: 100 })),
      core.dispatch<{ items: Contact[] }>(newRequest(ACTIONS.relationsList, {})),
      core.dispatch<FriendRequest[]>(newRequest(ACTIONS.requestsIncoming, {})),
    ]);
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
