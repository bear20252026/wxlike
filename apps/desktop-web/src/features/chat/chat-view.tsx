import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowLeft,
  Bell,
  BellOff,
  CheckSquare,
  Forward,
  Info,
  Pin,
  PinOff,
  RefreshCw,
  Search,
} from "lucide-react";
import { ACTIONS, type DomainError } from "../../shell/core-contract";
import type { ChatMessage, Conversation } from "../../shell/models";
import { useApp, useAppState, useCoreAction, useToast } from "../../shell/store";
import { navigate } from "../../shell/router";
import { Button, ConfirmDialog, EmptyState, ErrorState, IconButton, LoadingState, cn } from "../../shell/ui";
import { Composer } from "./composer";
import { MessageRow } from "./message-row";
import { ForwardDialog } from "./forward-dialog";

const PAGE_SIZE = 30;

function mergeMessages(rows: ChatMessage[]): ChatMessage[] {
  const map = new Map<string, ChatMessage>();
  for (const row of rows) map.set(row.msg_id, row);
  return [...map.values()].sort((a, b) => Number(a.seq) - Number(b.seq));
}

function previewOf(message: ChatMessage): string {
  switch (message.body.kind) {
    case "text":
      return message.body.text ?? "";
    case "image":
      return "[图片]";
    case "voice":
      return `[语音] ${Math.round((message.body.duration_ms ?? 0) / 1000)}″`;
    case "file":
      return `[文件] ${message.body.name ?? ""}`;
    case "sticker":
      return message.body.sticker ?? "[表情]";
    default:
      return message.body.text ?? "";
  }
}

function separatorLabel(iso: string): string {
  const date = new Date(iso);
  if (!Number.isFinite(date.getTime())) return "";
  const today = new Date();
  const sameDay = date.toDateString() === today.toDateString();
  const time = `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  if (sameDay) return `今天 ${time}`;
  return `${date.getMonth() + 1}月${date.getDate()}日 ${time}`;
}

export function ChatView({ conv }: { conv: Conversation }) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const { platform } = useApp();
  const [pageRows, setPageRows] = useState<ChatMessage[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [error, setError] = useState<DomainError | null>(null);
  const [quote, setQuote] = useState<ChatMessage | null>(null);
  const [forwardIds, setForwardIds] = useState<string[] | null>(null);
  const [recallTarget, setRecallTarget] = useState<ChatMessage | null>(null);
  const [selectionMode, setSelectionMode] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const scrollerRef = useRef<HTMLDivElement>(null);
  const stickToBottom = useRef(true);
  const loadTicket = useRef(0);
  const lastReadAt = useRef(0);

  const storeRows = state.messages[conv.conv_id] ?? [];
  const messages = useMemo(() => mergeMessages([...pageRows, ...storeRows]), [pageRows, storeRows]);
  const groupLike = conv.kind === "group";
  const peer = conv.peer_user_id ? state.contacts.find((contact) => contact.user_id === conv.peer_user_id) : undefined;

  const loadMessages = useCallback(async () => {
    const ticket = ++loadTicket.current;
    setLoading(true);
    setError(null);
    const response = await dispatch<{ items: ChatMessage[]; next_cursor: string | null; has_more: boolean }>(
      ACTIONS.messagesPage,
      { conv_id: conv.conv_id, limit: PAGE_SIZE },
      { silent: true },
    );
    if (ticket !== loadTicket.current) return;
    if (response.ok) {
      setPageRows(response.data.items);
      setCursor(response.data.next_cursor);
      setHasMore(response.data.has_more);
    } else {
      setError(response.error);
    }
    setLoading(false);
  }, [conv.conv_id, dispatch]);

  useEffect(() => {
    setPageRows([]);
    setSelected(new Set());
    setSelectionMode(false);
    setQuote(null);
    lastReadAt.current = 0;
    stickToBottom.current = true;
    void loadMessages();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conv.conv_id]);

  useEffect(() => {
    if (loading) return;
    const element = scrollerRef.current;
    if (element && stickToBottom.current) element.scrollTop = element.scrollHeight;
  }, [messages.length, loading, conv.conv_id]);

  useEffect(() => {
    if (!messages.length) return;
    const stamp = Date.now();
    if (stamp - lastReadAt.current < 1500) return;
    lastReadAt.current = stamp;
    void dispatch(ACTIONS.messageRead, { conv_id: conv.conv_id }, { silent: true, idempotencyKey: `read:${conv.conv_id}:${messages[messages.length - 1]?.seq}` });
  }, [messages, conv.conv_id, dispatch]);

  const loadOlder = async () => {
    if (loadingOlder || !hasMore || !cursor) return;
    setLoadingOlder(true);
    const ticket = loadTicket.current;
    const response = await dispatch<{ items: ChatMessage[]; next_cursor: string | null; has_more: boolean }>(
      ACTIONS.messagesPage,
      { conv_id: conv.conv_id, limit: PAGE_SIZE, cursor },
      { silent: true },
    );
    if (ticket === loadTicket.current && response.ok) {
      const element = scrollerRef.current;
      const previousHeight = element?.scrollHeight ?? 0;
      setPageRows((rows) => mergeMessages([...response.data.items, ...rows]));
      setCursor(response.data.next_cursor);
      setHasMore(response.data.has_more);
      requestAnimationFrame(() => {
        const node = scrollerRef.current;
        if (node) node.scrollTop += node.scrollHeight - previousHeight;
      });
    }
    setLoadingOlder(false);
  };

  const onScroll = () => {
    const element = scrollerRef.current;
    if (!element) return;
    stickToBottom.current = element.scrollHeight - element.scrollTop - element.clientHeight < 80;
    if (element.scrollTop < 40 && hasMore && !loadingOlder && !loading) void loadOlder();
  };

  const retry = async (message: ChatMessage) => {
    const response = await dispatch(ACTIONS.messageRetry, { conv_id: conv.conv_id, msg_id: message.msg_id }, {
      idempotencyKey: `retry:${message.msg_id}`,
    });
    if (response.ok) toast("已重新加入 outbox，等待核心投影为发送中");
  };

  const recall = async () => {
    if (!recallTarget) return;
    const target = recallTarget;
    setRecallTarget(null);
    const response = await dispatch(ACTIONS.messageRecall, { conv_id: conv.conv_id, msg_id: target.msg_id }, {
      idempotencyKey: `recall:${target.msg_id}`,
    });
    if (response.ok) toast("消息已撤回");
  };

  const copy = async (message: ChatMessage) => {
    const result = await platform.clipboard.writeText(message.body.text ?? "");
    if (result.ok) toast("已复制到剪贴板");
    else toast(result.message, "error");
  };

  const playMedia = (message: ChatMessage) => {
    toast(
      message.body.media_id
        ? `媒体 ${message.body.media_id} 由 G04 媒体服务提供；当前宿主未挂载 blob 读取接口，无法在此播放`
        : "该消息没有可读取的媒体内容",
      "error",
    );
  };

  const toggleSelected = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const toggleFlag = async (kind: "pin" | "mute") => {
    const action = kind === "pin" ? ACTIONS.conversationPin : ACTIONS.conversationMute;
    const value = kind === "pin" ? !conv.pinned : !conv.muted;
    const response = await dispatch(action, { conv_id: conv.conv_id, value }, {
      idempotencyKey: `${kind}:${conv.conv_id}:${value}`,
    });
    if (response.ok) toast(kind === "pin" ? (value ? "已置顶" : "已取消置顶") : value ? "已静音" : "已恢复提醒");
  };

  const openInfo = () => {
    if (conv.kind === "group") {
      const groupId = conv.conv_id.replace(/^conv-/, "group-");
      navigate({ id: "group", groupId });
    } else if (conv.peer_user_id) {
      navigate({ id: "contact", userId: conv.peer_user_id });
    }
  };

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col bg-[var(--surface)]" aria-label="会话">
      <header className="flex h-[52px] shrink-0 items-center gap-2 border-b border-[var(--line)] bg-[var(--surface-2)] px-3">
        <IconButton label="返回会话列表" onClick={() => navigate({ id: "chats" })} className="lg:hidden">
          <ArrowLeft size={16} />
        </IconButton>
        <div className="min-w-0">
          <h2 className="truncate text-[13px] font-semibold text-[var(--ink)]">
            {conv.title}
            {conv.muted ? <BellOff size={10} className="ml-1 inline text-[var(--muted)]" /> : null}
            {conv.pinned ? <Pin size={10} className="ml-1 inline text-[var(--muted)]" /> : null}
          </h2>
          <p className="truncate text-[9px] text-[var(--muted)]">
            {conv.kind === "group" ? "群聊 · 成员与权限由核心返回" : conv.kind === "file" ? "本地文件传输" : peer?.online ? "在线" : peer?.note ?? "离线"}
          </p>
        </div>
        <div className="ml-auto flex items-center gap-0.5">
          <IconButton label="在会话中搜索" onClick={() => navigate({ id: "search", query: conv.title })} data-testid="chat-search">
            <Search size={15} />
          </IconButton>
          <IconButton label={conv.pinned ? "取消置顶" : "置顶会话"} onClick={() => void toggleFlag("pin")} data-testid="chat-pin">
            {conv.pinned ? <PinOff size={15} /> : <Pin size={15} />}
          </IconButton>
          <IconButton label={conv.muted ? "恢复提醒" : "消息免打扰"} onClick={() => void toggleFlag("mute")} data-testid="chat-mute">
            {conv.muted ? <Bell size={15} /> : <BellOff size={15} />}
          </IconButton>
          <IconButton label="会话信息" onClick={openInfo} data-testid="chat-info">
            <Info size={15} />
          </IconButton>
        </div>
      </header>

      <div ref={scrollerRef} onScroll={onScroll} className="chat-canvas scrollbar-thin relative min-h-0 flex-1 overflow-y-auto px-4 py-3" data-testid="message-scroller">
        {loading ? (
          <LoadingState label="正在按服务端分页载入消息…" />
        ) : error ? (
          <ErrorState error={error} onRetry={() => void loadMessages()} />
        ) : messages.length === 0 ? (
          <EmptyState title="还没有消息" description="发送第一条消息，它会在本地 outbox 中排队，等待核心投影状态。" />
        ) : (
          <>
            <div className="mb-2 flex justify-center">
              {hasMore ? (
                <Button size="sm" variant="ghost" pending={loadingOlder} onClick={() => void loadOlder()} data-testid="load-older">
                  <RefreshCw size={11} /> 加载更早的消息
                </Button>
              ) : (
                <span className="text-[9px] text-[var(--muted)]">已到会话开头</span>
              )}
            </div>
            {messages.map((message, index) => {
              const previous = messages[index - 1];
              const gap = previous
                ? new Date(message.created_at_ms).getTime() - new Date(previous.created_at_ms).getTime()
                : Number.POSITIVE_INFINITY;
              const separator = !previous || gap > 10 * 60 * 1000;
              const quoteSource = message.body.quote_msg_id
                ? messages.find((row) => row.msg_id === message.body.quote_msg_id)
                : undefined;
              return (
                <div key={message.msg_id}>
                  {separator ? (
                    <div className="my-2 text-center text-[8.5px] text-[var(--muted)]">{separatorLabel(message.created_at_ms)}</div>
                  ) : null}
                  <MessageRow
                    message={message}
                    showSender={groupLike && message.direction === "in"}
                    quoteText={quoteSource ? previewOf(quoteSource) : undefined}
                    selectionMode={selectionMode}
                    selected={selected.has(message.msg_id)}
                    onToggleSelect={() => toggleSelected(message.msg_id)}
                    onQuote={(row) => setQuote(row)}
                    onForward={(row) => setForwardIds([row.msg_id])}
                    onRecall={(row) => setRecallTarget(row)}
                    onCopy={(row) => void copy(row)}
                    onRetry={(row) => void retry(row)}
                    onPlayVoice={playMedia}
                    onOpenMedia={playMedia}
                  />
                </div>
              );
            })}
          </>
        )}
      </div>

      {selectionMode ? (
        <div className="flex h-[52px] shrink-0 items-center gap-2 border-t border-[var(--line)] bg-[var(--surface-2)] px-3" data-testid="selection-toolbar">
          <span className="text-[11px] text-[var(--ink-2)]">已选 {selected.size} 条</span>
          <Button
            size="sm"
            disabled={!selected.size}
            onClick={() => setForwardIds([...selected])}
            data-testid="selection-forward"
          >
            <Forward size={12} /> 转发
          </Button>
          <Button size="sm" variant="ghost" onClick={() => { setSelectionMode(false); setSelected(new Set()); }}>
            取消
          </Button>
          <span className="ml-auto text-[9px] text-[var(--muted)]">批量操作逐条调用消息 action</span>
        </div>
      ) : (
        <Composer convId={conv.conv_id} replyTo={quote} onClearReply={() => setQuote(null)} />
      )}

      <div className="flex h-6 shrink-0 items-center justify-between border-t border-[var(--line)] bg-[var(--surface-2)] px-3 text-[8.5px] text-[var(--muted)]">
        <span>{messages.length} 条已载入 · 分页游标 {cursor ?? "—"}</span>
        <button
          type="button"
          onClick={() => {
            setSelectionMode((mode) => !mode);
            setSelected(new Set());
          }}
          className={cn("flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-[var(--surface-3)]", selectionMode && "text-[var(--accent-ink)]")}
          data-testid="toggle-selection"
        >
          <CheckSquare size={10} /> 多选
        </button>
      </div>

      <ForwardDialog open={forwardIds !== null} msgIds={forwardIds ?? []} onClose={() => setForwardIds(null)} />
      <ConfirmDialog
        open={recallTarget !== null}
        title="撤回这条消息？"
        description="撤回由服务端校验时效与权限，界面不会先自行隐藏。"
        confirmLabel="撤回"
        danger
        onConfirm={() => void recall()}
        onClose={() => setRecallTarget(null)}
      />
    </section>
  );
}
