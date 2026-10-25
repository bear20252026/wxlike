/** Small shared primitives for the G11 feature pages (Tailwind, UI v2 look). */
import { useEffect, useRef, useState } from "react";

export function Banner({ tone, text }: { tone: "error" | "info" | "ok"; text: string }) {
  if (!text) return null;
  const style =
    tone === "error"
      ? "bg-red-50 text-red-700 border-red-200"
      : tone === "ok"
        ? "bg-emerald-50 text-emerald-700 border-emerald-200"
        : "bg-sky-50 text-sky-700 border-sky-200";
  return (
    <div role={tone === "error" ? "alert" : "status"} className={`rounded-lg border px-3 py-2 text-sm ${style}`}>
      {text}
    </div>
  );
}

export function PrimaryButton({
  children,
  onClick,
  disabled,
  busy,
  type = "button",
  title,
}: {
  children: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  busy?: boolean;
  type?: "button" | "submit";
  title?: string;
}) {
  return (
    <button
      type={type}
      title={title}
      disabled={disabled || busy}
      onClick={onClick}
      className="inline-flex items-center gap-1.5 rounded-lg bg-emerald-600 px-3.5 py-1.5 text-sm font-medium text-white transition hover:bg-emerald-700 disabled:cursor-not-allowed disabled:opacity-50"
    >
      {busy ? <Spinner /> : null}
      {children}
    </button>
  );
}

export function GhostButton({
  children,
  onClick,
  disabled,
  title,
  tone = "neutral",
  testId,
}: {
  children: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
  title?: string;
  tone?: "neutral" | "danger";
  testId?: string;
}) {
  const style =
    tone === "danger"
      ? "border-red-200 text-red-600 hover:bg-red-50"
      : "border-zinc-200 text-zinc-600 hover:bg-zinc-100";
  return (
    <button
      type="button"
      title={title}
      disabled={disabled}
      onClick={onClick}
      data-testid={testId}
      className={`inline-flex items-center gap-1.5 rounded-lg border bg-white px-3 py-1.5 text-sm transition disabled:cursor-not-allowed disabled:opacity-50 ${style}`}
    >
      {children}
    </button>
  );
}

export function Spinner() {
  return (
    <span
      aria-hidden
      className="inline-block h-3.5 w-3.5 animate-spin rounded-full border-2 border-white/40 border-t-white"
    />
  );
}

/**
 * Three-dot menu. 验收要求所有三点菜单展开可用：菜单内每一项都是真实按钮，
 * 空菜单（无可用项）不会渲染触点。
 */
export function DotMenu({ items }: { items: Array<{ label: string; onSelect: () => void; danger?: boolean }> }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (ref.current && !ref.current.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);
  if (items.length === 0) return null;
  return (
    <div className="relative" ref={ref}>
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="更多操作"
        data-testid="dot-menu-trigger"
        onClick={() => setOpen((value) => !value)}
        className="rounded-md px-1.5 py-0.5 text-zinc-400 hover:bg-zinc-100 hover:text-zinc-600"
      >
        ⋯
      </button>
      {open ? (
        <div role="menu" className="absolute right-0 z-20 mt-1 min-w-32 rounded-lg border border-zinc-200 bg-white py-1 shadow-lg">
          {items.map((item) => (
            <button
              key={item.label}
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                item.onSelect();
              }}
              className={`block w-full px-3 py-1.5 text-left text-sm hover:bg-zinc-100 ${item.danger ? "text-red-600" : "text-zinc-700"}`}
            >
              {item.label}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

export function EmptyState({ title, hint }: { title: string; hint?: string }) {
  return (
    <div className="flex flex-col items-center justify-center gap-1 rounded-xl border border-dashed border-zinc-200 py-10 text-center">
      <p className="text-sm font-medium text-zinc-500">{title}</p>
      {hint ? <p className="text-xs text-zinc-400">{hint}</p> : null}
    </div>
  );
}

export function Modal({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4" onClick={onClose}>
      <div
        className="w-full max-w-md rounded-2xl bg-white p-5 shadow-2xl"
        onClick={(event) => event.stopPropagation()}
        role="dialog"
        aria-label={title}
      >
        <div className="mb-3 flex items-center justify-between">
          <h3 className="text-base font-semibold text-zinc-800">{title}</h3>
          <button type="button" aria-label="关闭" onClick={onClose} className="rounded-md px-2 text-zinc-400 hover:bg-zinc-100">
            ✕
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}

export function formatTime(ms: number): string {
  const date = new Date(ms);
  const now = Date.now();
  const diff = now - ms;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  return date.toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" });
}
