import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BellOff, Check, MessageSquarePlus, MoreHorizontal, Pin, PinOff, Search, X } from "lucide-react";
import { ACTIONS, type DomainError } from "../../shell/core-contract";
import type { Conversation } from "../../shell/models";
import { useAppState, useCoreAction, useToast } from "../../shell/store";
import { useDebounced } from "../../shell/hooks";
import { Avatar, Badge, Button, Dialog, EmptyState, ErrorState, LoadingState, cn } from "../../shell/ui";
import { formatRelative } from "./format";

const ROW_HEIGHT = 64;
const PAGE_SIZE = 30;

type Filter = "all" | "unread" | "groups";

const FILTER_LABEL: Record<Filter, string> = { all: "全部", unread: "未读", groups: "群聊" };

function mergeRows(rows: Conversation[], incoming: Conversation[]): Conversation[] {
  const map = new Map(rows.map((row) => [row.conv_id, row]));
  for (const row of incoming) map.set(row.conv_id, { ...map.get(row.conv_id), ...row });
  return [...map.values()].sort(
    (a, b) => Number(b.pinned) - Number(a.pinned) || b.last_at_ms.localeCompare(a.last_at_ms),
  );
}

function RowMenu({
  conv,
  onClose,
  onDone,
}: {
  conv: Conversation;
  onClose: () => void;
  onDone: () => void;
}) {
  const dispatch = useCoreAction();
  const toast = useToast();
  const [pending, setPending] = useState(false);

  const run = async (action: string, params: unknown, label: string) => {
    if (pending) return;
    setPending(true);
    const response = await dispatch(action, params, { idempotencyKey: `${action}:${conv.conv_id}:${JSON.stringify(params)}` });
    setPending(false);
    onClose();
    if (response.ok) {
      toast(label);
      onDone();
    }
  };

  return (
    <>
      <div className="fixed inset-0 z-20" onClick={onClose} />
      <div className="absolute right-2 top-11 z-30 w-[148px] overflow-hidden rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] py-1 shadow-[var(--shadow-pop)]">
        <button
          type="button"
          disabled={pending}
          onClick={() => void run(ACTIONS.conversationPin, { conv_id: conv.conv_id, value: !conv.pinned }, conv.pinned ? "已取消置顶" : "已置顶")}
          className="flex w-full items-center gap-2 px-3 py-1.5 text-[10.5px] text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
          data-testid="row-menu-pin"
        >
          {conv.pinned ? <PinOff size={12} /> : <Pin size={12} />}
          {conv.pinned ? "取消置顶" : "置顶会话"}
        </button>
        <button
          type="button"
          disabled={pending}
          onClick={() => void run(ACTIONS.conversationMute, { conv_id: conv.conv_id, value: !conv.muted }, conv.muted ? "已恢复提醒" : "已设为免打扰")}
          className="flex w-full items-center gap-2 px-3 py-1.5 text-[10.5px] text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
          data-testid="row-menu-mute"
        >
          <BellOff size={12} />
          {conv.muted ? "恢复提醒" : "消息免打扰"}
        </button>
        <button
          type="button"
          disabled={pending || conv.unread === 0}
          onClick={() => void run(ACTIONS.messageRead, { conv_id: conv.conv_id }, "已标为已读")}
          className="flex w-full items-center gap-2 px-3 py-1.5 text-[10.5px] text-[var(--ink-2)] hover:bg-[var(--surface-3)] disabled:opacity-40"
          data-testid="row-menu-read"
        >
          <Check size={12} /> 标为已读
        </button>
      </div>
    </>
  );
}

function ConversationRow({
  conv,
  active,
  index,
  onSelect,
  menuOpen,
  onMenu,
}: {
  conv: Conversation;
  active: boolean;
  index: number;
  onSelect: () => void;
  menuOpen: boolean;
  onMenu: (open: boolean) => void;
}) {
  return (
    <div className="absolute left-0 right-0" style={{ top: index * ROW_HEIGHT, height: ROW_HEIGHT }}>
      <div
        className={cn(
          "group relative flex h-full w-full items-center gap-2.5 px-3 text-left transition-colors",
          active ? "bg-[var(--surface-3)]" : "hover:bg-[var(--surface-3)]/60",
        )}
      >
        {active ? <span className="absolute left-0 top-1/2 h-8 w-[3px] -translate-y-1/2 rounded-r bg-[var(--accent)]" /> : null}
        <button type="button" onClick={onSelect} className="flex min-w-0 flex-1 items-center gap-2.5 text-left" data-testid={`conv-${conv.conv_id}`}>
          <span className="relative">
            <Avatar src={conv.avatar} name={conv.title} size={40} rounded="rounded-[7px]" />
            {conv.kind === "group" ? (
              <span className="absolute -bottom-1 -right-1 rounded bg-[var(--surface-2)] px-1 text-[7px] font-medium text-[var(--muted)] ring-1 ring-[var(--line)]">
                群
              </span>
            ) : conv.kind === "file" ? (
              <span className="absolute -bottom-1 -right-1 rounded bg-[var(--surface-2)] px-1 text-[7px] font-medium text-[var(--muted)] ring-1 ring-[var(--line)]">
                文件
              </span>
            ) : null}
          </span>
          <span className="min-w-0 flex-1">
            <span className="flex items-baseline justify-between gap-1">
              <span className="truncate text-[12px] font-medium text-[var(--ink)]">{conv.title}</span>
              <span className="shrink-0 text-[9px] text-[var(--muted)]">{formatRelative(conv.last_at_ms)}</span>
            </span>
            <span className="mt-1 flex items-center justify-between gap-1">
              <span className="flex min-w-0 items-center gap-1 truncate text-[10px] text-[var(--muted)]">
                {conv.pinned ? <Pin size={9} className="shrink-0" /> : null}
                {conv.muted ? <BellOff size={9} className="shrink-0" /> : null}
                <span className="truncate">{conv.last_preview || "暂无消息"}</span>
              </span>
              {conv.unread > 0 ? (
                <span className="shrink-0">
                  {conv.muted ? (
                    <span className="grid h-[16px] min-w-[16px] place-items-center rounded-full bg-[var(--surface-3)] px-1 text-[8px] font-semibold text-[var(--muted)]">
                      {conv.unread}
                    </span>
                  ) : (
                    <Badge count={conv.unread} />
                  )}
                </span>
              ) : null}
            </span>
          </span>
        </button>
        <button
          type="button"
          aria-label={`${conv.title} 的更多操作`}
          onClick={() => onMenu(!menuOpen)}
          className="grid h-6 w-6 shrink-0 place-items-center rounded text-[var(--muted)] opacity-0 transition hover:bg-[var(--surface-2)] hover:text-[var(--ink)] group-hover:opacity-100 focus-visible:opacity-100"
          data-testid={`conv-menu-${conv.conv_id}`}
        >
          <MoreHorizontal size={13} />
        </button>
        {menuOpen ? <RowMenu conv={conv} onClose={() => onMenu(false)} onDone={() => onMenu(false)} /> : null}
      </div>
    </div>
  );
}

export function ConversationList({
  activeConvId,
  onSelect,
}: {
  activeConvId?: string;
  onSelect: (conv: Conversation) => void;
}) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const debouncedQuery = useDebounced(query, 220);
  const [rows, setRows] = useState<Conversation[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<DomainError | null>(null);
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const [newChatOpen, setNewChatOpen] = useState(false);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(560);
  const generation = useRef(0);
  const scrollerRef = useRef<HTMLDivElement>(null);

  const filterParam = filter === "all" ? "all" : filter;

  const loadPage = useCallback(
    async (mode: "reset" | "more") => {
      const ticket = mode === "reset" ? ++generation.current : generation.current;
      if (mode === "reset") {
        setLoading(true);
        setError(null);
      } else {
        setLoadingMore(true);
      }
      const response = await dispatch<{ items: Conversation[]; next_cursor: string | null; has_more: boolean }>(
        ACTIONS.conversationsList,
        {
          filter: filterParam,
          query: debouncedQuery.trim(),
          limit: PAGE_SIZE,
          cursor: mode === "more" ? cursor : null,
        },
        { silent: true },
      );
      if (ticket !== generation.current) return;
      if (response.ok) {
        setRows((prev) => (mode === "reset" ? response.data.items : mergeRows(prev, response.data.items)));
        setCursor(response.data.next_cursor);
        setHasMore(response.data.has_more);
        setError(null);
      } else {
        setError(response.error);
      }
      setLoading(false);
      setLoadingMore(false);
    },
    [dispatch, filterParam, debouncedQuery, cursor],
  );

  useEffect(() => {
    setRows([]);
    setCursor(null);
    setHasMore(false);
    setScrollTop(0);
    void loadPage("reset");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filterParam, debouncedQuery]);

  // Server events and the store are the truth for unread/pin/preview updates.
  useEffect(() => {
    if (!state.conversations.length) return;
    setRows((prev) => (prev.length ? mergeRows(prev, state.conversations) : prev));
  }, [state.conversations]);

  useEffect(() => {
    const element = scrollerRef.current;
    if (!element) return;
    const measure = () => setViewportHeight(element.clientHeight || 560);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const totalHeight = rows.length * ROW_HEIGHT;
  const canVirtualize = viewportHeight > 0 && rows.length > 40;
  const start = canVirtualize ? Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - 4) : 0;
  const end = canVirtualize ? Math.min(rows.length, Math.ceil((scrollTop + viewportHeight) / ROW_HEIGHT) + 4) : rows.length;
  const visible = useMemo(() => rows.slice(start, end), [rows, start, end]);

  const onScroll = () => {
    const element = scrollerRef.current;
    if (!element) return;
    setScrollTop(element.scrollTop);
    if (element.scrollHeight - element.scrollTop - element.clientHeight < 120 && hasMore && !loadingMore && !loading) {
      void loadPage("more");
    }
  };

  const unreadTotal = rows.reduce((sum, row) => sum + row.unread, 0);

  const createChat = async (userId: string) => {
    const response = await dispatch<Conversation>(ACTIONS.conversationEnsure, { user_id: userId });
    if (response.ok) {
      setNewChatOpen(false);
      onSelect(response.data);
    }
  };

  return (
    <aside className="flex w-[280px] shrink-0 flex-col border-r border-[var(--line)] bg-[var(--surface)]">
      <div className="shrink-0 px-3 pb-2 pt-3">
        <div className="flex h-7 items-center justify-between">
          <h2 className="flex items-baseline gap-2 text-[15px] font-semibold text-[var(--ink)]">
            消息
            {unreadTotal > 0 ? <span className="text-[9px] font-normal text-[var(--muted)]">{unreadTotal} 条未读</span> : null}
          </h2>
          <div className="flex items-center gap-1">
            <button
              type="button"
              onClick={() => setNewChatOpen(true)}
              title="发起新会话"
              className="grid h-7 w-7 place-items-center rounded-md text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
              data-testid="conv-new"
            >
              <MessageSquarePlus size={15} />
            </button>
            <button
              type="button"
              onClick={() => void dispatch(ACTIONS.conversationReadAll, {})}
              title="全部标为已读"
              className="grid h-7 w-7 place-items-center rounded-md text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
              data-testid="conv-read-all"
            >
              <Check size={15} />
            </button>
          </div>
        </div>
        <div className="mt-2 flex h-[30px] items-center gap-1.5 rounded-md bg-[var(--surface-3)] px-2.5">
          <Search size={12} className="shrink-0 text-[var(--muted)]" />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="搜索会话与消息"
            aria-label="搜索会话"
            className="w-full bg-transparent text-[11px] text-[var(--ink)] outline-none placeholder:text-[var(--muted)]"
            data-testid="conv-search"
          />
          {query ? (
            <button type="button" aria-label="清空搜索" onClick={() => setQuery("")} className="grid h-4 w-4 place-items-center rounded-full bg-[var(--muted)] text-white">
              <X size={9} strokeWidth={3} />
            </button>
          ) : null}
        </div>
        <div className="mt-2 flex items-center gap-1">
          {(Object.keys(FILTER_LABEL) as Filter[]).map((key) => (
            <button
              key={key}
              type="button"
              onClick={() => setFilter(key)}
              className={cn(
                "rounded-full px-2.5 py-1 text-[9.5px] transition",
                filter === key ? "bg-[var(--ink)] font-medium text-[var(--app-bg)]" : "text-[var(--muted)] hover:bg-[var(--surface-3)]",
              )}
              data-testid={`conv-filter-${key}`}
            >
              {FILTER_LABEL[key]}
            </button>
          ))}
          {hasMore ? <span className="ml-auto text-[8.5px] text-[var(--muted)]">上滑加载更多</span> : null}
        </div>
      </div>

      <div ref={scrollerRef} onScroll={onScroll} className="scrollbar-thin relative min-h-0 flex-1 overflow-y-auto" data-testid="conv-scroller">
        {loading ? (
          <LoadingState label="正在加载会话…" />
        ) : error ? (
          <ErrorState error={error} onRetry={() => void loadPage("reset")} compact />
        ) : rows.length === 0 ? (
          <EmptyState
            title={debouncedQuery ? "没有匹配的会话" : filter === "unread" ? "没有未读消息" : "还没有会话"}
            description={debouncedQuery ? "换个关键词，或到全局搜索里按分类查找。" : "从通讯录发起会话，或等待新消息。"}
            action={
              <Button size="sm" onClick={() => setNewChatOpen(true)}>
                发起会话
              </Button>
            }
          />
        ) : (
          <div style={{ height: totalHeight, position: "relative" }} data-testid="conv-virtual-window">
            {visible.map((conv, index) => (
              <ConversationRow
                key={conv.conv_id}
                conv={conv}
                active={conv.conv_id === activeConvId}
                index={start + index}
                onSelect={() => onSelect(conv)}
                menuOpen={menuFor === conv.conv_id}
                onMenu={(open) => setMenuFor(open ? conv.conv_id : null)}
              />
            ))}
          </div>
        )}
        {loadingMore ? <div className="py-2 text-center text-[9px] text-[var(--muted)]">加载更早会话…</div> : null}
      </div>

      <div className="flex h-7 shrink-0 items-center justify-between border-t border-[var(--line)] px-3 text-[9px] text-[var(--muted)]">
        <span>{rows.length} 个会话</span>
        <span>按服务端排序</span>
      </div>

      <NewChatDialog
        open={newChatOpen}
        onClose={() => setNewChatOpen(false)}
        onPick={(userId) => void createChat(userId)}
        notify={(text) => toast(text, "error")}
      />
    </aside>
  );
}

function NewChatDialog({
  open,
  onClose,
  onPick,
  notify,
}: {
  open: boolean;
  onClose: () => void;
  onPick: (userId: string) => void;
  notify: (text: string) => void;
}) {
  const state = useAppState();
  const [query, setQuery] = useState("");
  const contacts = state.contacts.filter(
    (contact) => !contact.blocked && (contact.name.includes(query) || (contact.note ?? "").includes(query)),
  );
  return (
    <Dialog open={open} title="发起会话" description="选择一位好友；会话由核心创建并返回真实会话 ID。" onClose={onClose} width={380}>
      <input
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder="搜索联系人"
        aria-label="搜索联系人"
        className="mb-2 h-8 w-full rounded-md border border-[var(--line-strong)] bg-[var(--surface)] px-2.5 text-[11px] outline-none"
        data-testid="new-chat-search"
      />
      <div className="max-h-[260px] overflow-y-auto scrollbar-thin">
        {contacts.length === 0 ? (
          <EmptyState title="没有可选联系人" description="先到通讯录添加好友。" />
        ) : (
          contacts.map((contact) => (
            <button
              key={contact.user_id}
              type="button"
              onClick={() => (contact.blocked ? notify("已拉黑的联系人不能发起会话") : onPick(contact.user_id))}
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-[var(--surface-3)]"
              data-testid={`new-chat-${contact.user_id}`}
            >
              <Avatar src={contact.avatar} name={contact.name} size={28} />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-[11px] text-[var(--ink)]">{contact.name}</span>
                <span className="block truncate text-[9px] text-[var(--muted)]">{contact.note ?? contact.region ?? contact.user_id}</span>
              </span>
            </button>
          ))
        )}
      </div>
    </Dialog>
  );
}
