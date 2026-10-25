import { useState, type ReactNode } from "react";
import {
  AlertCircle,
  Check,
  CheckCheck,
  Clock,
  Copy,
  CornerUpLeft,
  FileText,
  Forward,
  Image as ImageIcon,
  Mic,
  Undo2,
} from "lucide-react";
import type { ChatMessage } from "../../shell/models";
import { Avatar, cn } from "../../shell/ui";
import { formatClock } from "./format";
import { formatBytes } from "./upload";

const stateLabel: Partial<Record<ChatMessage["state"], string>> = {
  LOCAL_QUEUED: "排队中",
  SENDING: "发送中",
  SERVER_ACCEPTED: "已发送",
  DEVICE_STORED: "已送达",
  FAILED_FINAL: "发送失败",
};

export function MessageRow({
  message,
  showSender,
  quoteText,
  selectionMode,
  selected,
  onToggleSelect,
  onQuote,
  onForward,
  onRecall,
  onCopy,
  onRetry,
  onPlayVoice,
  onOpenMedia,
}: {
  message: ChatMessage;
  showSender: boolean;
  quoteText?: string;
  selectionMode: boolean;
  selected: boolean;
  onToggleSelect: () => void;
  onQuote: (message: ChatMessage) => void;
  onForward: (message: ChatMessage) => void;
  onRecall: (message: ChatMessage) => void;
  onCopy: (message: ChatMessage) => void;
  onRetry: (message: ChatMessage) => void;
  onPlayVoice: (message: ChatMessage) => void;
  onOpenMedia: (message: ChatMessage) => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const out = message.direction === "out";
  const recalled = message.recalled || message.state === "RECALLED";

  if (message.body.kind === "system" || recalled) {
    return (
      <div className="flex justify-center py-1.5" data-testid={`msg-${message.msg_id}`}>
        <span className="rounded bg-black/[0.04] px-2 py-0.5 text-[9px] text-[var(--muted)] dark:bg-white/[0.06]">
          {message.body.text ?? "你撤回了一条消息"}
        </span>
      </div>
    );
  }

  const bubble = (
    <div
      className={cn(
        "relative max-w-[420px] rounded-lg px-3 py-2 text-[12px] leading-5 shadow-sm",
        out ? "bg-[var(--bubble-me)] text-[var(--bubble-me-ink)]" : "bg-[var(--surface-2)] text-[var(--ink)]",
      )}
    >
      {quoteText ? (
        <div className="mb-1 border-l-2 border-black/15 pl-2 text-[9.5px] leading-4 opacity-70 dark:border-white/20">
          引用：{quoteText.slice(0, 80)}
        </div>
      ) : null}
      {message.body.kind === "text" ? <p className="whitespace-pre-wrap break-words">{message.body.text}</p> : null}
      {message.body.kind === "sticker" ? <p className="text-[30px] leading-9">{message.body.sticker}</p> : null}
      {message.body.kind === "image" ? (
        <button
          type="button"
          onClick={() => onOpenMedia(message)}
          className="flex w-[220px] flex-col items-start gap-1.5 rounded-md bg-black/5 p-2 text-left dark:bg-white/10"
          data-testid={`msg-image-${message.msg_id}`}
        >
          <span className="flex h-[92px] w-full items-center justify-center rounded bg-black/5 dark:bg-white/5">
            <ImageIcon size={26} className="opacity-50" />
          </span>
          <span className="text-[10px] opacity-80">{message.body.name ?? "图片"} · {formatBytes(message.body.size ?? 0)}</span>
        </button>
      ) : null}
      {message.body.kind === "voice" ? (
        <button
          type="button"
          onClick={() => onPlayVoice(message)}
          className="flex items-center gap-1.5 text-left"
          data-testid={`msg-voice-${message.msg_id}`}
        >
          <Mic size={13} />
          <span className="text-[11px] tabular-nums">{Math.round((message.body.duration_ms ?? 0) / 1000)}″</span>
        </button>
      ) : null}
      {message.body.kind === "file" ? (
        <button
          type="button"
          onClick={() => onOpenMedia(message)}
          className="flex w-[220px] items-center gap-2 text-left"
          data-testid={`msg-file-${message.msg_id}`}
        >
          <span className="grid h-9 w-9 place-items-center rounded bg-black/5 dark:bg-white/10">
            <FileText size={16} />
          </span>
          <span className="min-w-0">
            <span className="block truncate text-[10.5px] font-medium">{message.body.name ?? "文件"}</span>
            <span className="block text-[9px] opacity-70">{formatBytes(message.body.size ?? 0)}</span>
          </span>
        </button>
      ) : null}
      {message.body.kind === "link" ? <p className="break-all text-[11px] underline decoration-black/20">{message.body.text}</p> : null}
    </div>
  );

  return (
    <div
      className={cn("message-item group relative flex gap-2 py-1.5", out ? "flex-row-reverse" : "flex-row")}
      data-testid={`msg-${message.msg_id}`}
      data-state={message.state}
    >
      {selectionMode ? (
        <input
          type="checkbox"
          checked={selected}
          onChange={onToggleSelect}
          aria-label={`选择消息 ${message.msg_id}`}
          className="mt-6 accent-[var(--accent)]"
          data-testid={`msg-select-${message.msg_id}`}
        />
      ) : null}
      <Avatar src={message.sender.avatar} name={message.sender.name} size={30} rounded="rounded-[6px]" />
      <div className={cn("flex min-w-0 flex-col", out ? "items-end" : "items-start")}>
        {showSender && !out ? <span className="mb-0.5 text-[9px] text-[var(--muted)]">{message.sender.name}</span> : null}
        <div className="flex items-end gap-1.5">
          {out && !selectionMode ? (
            <div className="mb-1 flex flex-col items-end gap-0.5 text-[8.5px] text-[var(--muted)]">
              {message.state === "FAILED_RETRYABLE" || message.state === "FAILED_FINAL" ? (
                <>
                  <span className="flex items-center gap-1 text-[var(--danger)]">
                    <AlertCircle size={10} /> {message.error?.message ?? stateLabel[message.state]}
                  </span>
                  {message.state === "FAILED_RETRYABLE" ? (
                    <button
                      type="button"
                      onClick={() => onRetry(message)}
                      className="text-[9px] text-[var(--accent-ink)] hover:underline"
                      data-testid={`msg-retry-${message.msg_id}`}
                    >
                      重试
                    </button>
                  ) : null}
                </>
              ) : (
                <span className="flex items-center gap-0.5 tabular-nums">
                  {formatClock(message.created_at_ms)}
                  {message.state === "SENDING" ? <Clock size={9} /> : null}
                  {message.state === "SERVER_ACCEPTED" ? <Check size={9} /> : null}
                  {message.state === "DEVICE_STORED" ? <CheckCheck size={9} className="text-[var(--accent-ink)]" /> : null}
                  {stateLabel[message.state]}
                </span>
              )}
            </div>
          ) : null}
          {selectionMode ? (
            bubble
          ) : (
            <div
              role="group"
              aria-label="消息操作"
              tabIndex={0}
              onClick={() => setMenuOpen((open) => !open)}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  setMenuOpen((open) => !open);
                }
              }}
              className="cursor-default text-left outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent)]"
              data-testid={`msg-open-${message.msg_id}`}
            >
              {bubble}
            </div>
          )}
        </div>
        <span className={cn("mt-0.5 text-[8px] text-[var(--muted)]", out ? "text-right" : "text-left")}>
          {out ? "" : formatClock(message.created_at_ms)}
        </span>
      </div>

      {!selectionMode && menuOpen ? (
        <>
          <div className="fixed inset-0 z-20" onClick={() => setMenuOpen(false)} />
          <div
            className={cn(
              "absolute top-2 z-30 flex items-center gap-0.5 rounded-md border border-[var(--line-strong)] bg-[var(--surface-2)] p-0.5 shadow-[var(--shadow-pop)]",
              out ? "right-[120px]" : "left-[120px]",
            )}
            data-testid={`msg-menu-${message.msg_id}`}
          >
            <ActionButton label="引用" testId={`msg-quote-${message.msg_id}`} onClick={() => { setMenuOpen(false); onQuote(message); }}>
              <CornerUpLeft size={12} />
            </ActionButton>
            <ActionButton label="转发" testId={`msg-forward-${message.msg_id}`} onClick={() => { setMenuOpen(false); onForward(message); }}>
              <Forward size={12} />
            </ActionButton>
            {message.body.kind === "text" ? (
              <ActionButton label="复制" testId={`msg-copy-${message.msg_id}`} onClick={() => { setMenuOpen(false); onCopy(message); }}>
                <Copy size={12} />
              </ActionButton>
            ) : null}
            {out ? (
              <ActionButton label="撤回" testId={`msg-recall-${message.msg_id}`} onClick={() => { setMenuOpen(false); onRecall(message); }}>
                <Undo2 size={12} />
              </ActionButton>
            ) : null}
          </div>
        </>
      ) : null}
    </div>
  );
}

function ActionButton({
  label,
  testId,
  onClick,
  children,
}: {
  label: string;
  testId: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      onClick={onClick}
      className="grid h-6 w-6 place-items-center rounded text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
      data-testid={testId}
    >
      {children}
    </button>
  );
}
