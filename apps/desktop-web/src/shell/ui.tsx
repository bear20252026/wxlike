import {
  createContext,
  useContext,
  useEffect,
  useId,
  useRef,
  type ButtonHTMLAttributes,
  type InputHTMLAttributes,
  type ReactNode,
  type TextareaHTMLAttributes,
} from "react";
import { createPortal } from "react-dom";
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import { AlertTriangle, Loader2, Lock, SearchX, X } from "lucide-react";
import { describeError, type DomainError } from "./core-contract";
import { useAppState, useDismissToast } from "./store";

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

const gradients = [
  "linear-gradient(135deg,#A9CBB8,#6FA287)",
  "linear-gradient(135deg,#E5C9A8,#C08A5A)",
  "linear-gradient(135deg,#B9C7E8,#7C93C9)",
  "linear-gradient(135deg,#E3B7C2,#C07A8C)",
  "linear-gradient(135deg,#B8DCCF,#6FB598)",
  "linear-gradient(135deg,#D9C9EE,#9A82C9)",
];

function hashName(name: string): number {
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  return hash % gradients.length;
}

export function Avatar({
  src,
  name = "",
  size = 40,
  rounded = "rounded-[7px]",
  online,
  className,
}: {
  src?: string;
  name?: string;
  size?: number;
  rounded?: string;
  online?: boolean;
  className?: string;
}) {
  const initials = (name || "微").slice(0, 1);
  return (
    <span className={cn("relative inline-block shrink-0", className)} style={{ width: size, height: size }}>
      <span
        className={cn("block h-full w-full overflow-hidden text-white ring-1 ring-black/[0.06]", rounded)}
        style={src ? undefined : { background: gradients[hashName(name || "W")] }}
      >
        {src ? (
          <img src={src} alt={name} className="h-full w-full object-cover" draggable={false} />
        ) : (
          <span className="flex h-full w-full items-center justify-center text-xs font-semibold">
            {initials}
          </span>
        )}
      </span>
      {online ? (
        <span className="absolute bottom-0 right-0 h-2.5 w-2.5 rounded-full border-2 border-[var(--surface)] bg-[var(--accent)]" />
      ) : null}
    </span>
  );
}

const buttonVariants = {
  primary:
    "bg-[var(--accent)] text-white hover:bg-[var(--accent-ink)] disabled:bg-[var(--surface-3)] disabled:text-[var(--muted)]",
  secondary:
    "border border-[var(--line-strong)] bg-[var(--surface-2)] text-[var(--ink)] hover:bg-[var(--surface-3)]",
  ghost: "text-[var(--ink-2)] hover:bg-[var(--surface-3)] hover:text-[var(--ink)]",
  danger:
    "bg-[var(--danger)] text-white hover:opacity-90 disabled:bg-[var(--surface-3)] disabled:text-[var(--muted)]",
} as const;

const buttonSizes = {
  sm: "h-7 px-2.5 text-[11px] gap-1",
  md: "h-9 px-3.5 text-[12px] gap-1.5",
  lg: "h-11 px-5 text-[13px] gap-2",
} as const;

export function Button({
  variant = "secondary",
  size = "md",
  pending = false,
  className,
  children,
  disabled,
  type = "button",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: keyof typeof buttonVariants;
  size?: keyof typeof buttonSizes;
  pending?: boolean;
}) {
  return (
    <button
      type={type}
      disabled={disabled || pending}
      aria-busy={pending || undefined}
      className={cn(
        "inline-flex items-center justify-center rounded-md font-medium transition-colors disabled:cursor-not-allowed",
        buttonVariants[variant],
        buttonSizes[size],
        className,
      )}
      {...rest}
    >
      {pending ? <Loader2 size={13} className="spin" aria-hidden /> : null}
      {children}
    </button>
  );
}

export function IconButton({
  label,
  className,
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      className={cn(
        "grid h-8 w-8 place-items-center rounded-md text-[var(--ink-2)] transition-colors hover:bg-[var(--surface-3)] hover:text-[var(--ink)] disabled:opacity-40",
        className,
      )}
      {...rest}
    >
      {children}
    </button>
  );
}

export function Badge({ count, className }: { count: number; className?: string }) {
  if (count <= 0) return null;
  return (
    <span
      className={cn(
        "grid h-[17px] min-w-[17px] place-items-center rounded-full bg-[var(--danger)] px-1 text-[9px] font-bold leading-none text-white",
        className,
      )}
      aria-label={`${count} 条未读`}
    >
      {count > 99 ? "99+" : count}
    </span>
  );
}

export function Spinner({ label = "加载中" }: { label?: string }) {
  return (
    <span className="inline-flex items-center gap-2 text-[11px] text-[var(--muted)]" role="status">
      <Loader2 size={14} className="spin" aria-hidden />
      {label}
    </span>
  );
}

export function LoadingState({ label = "正在加载…" }: { label?: string }) {
  return (
    <div className="flex h-full min-h-[160px] flex-col items-center justify-center gap-3" role="status">
      <Loader2 size={20} className="spin text-[var(--muted)]" aria-hidden />
      <span className="text-[11px] text-[var(--muted)]">{label}</span>
    </div>
  );
}

export function EmptyState({
  title,
  description,
  action,
  icon,
}: {
  title: string;
  description?: string;
  action?: ReactNode;
  icon?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 px-8 py-14 text-center">
      <span className="grid h-11 w-11 place-items-center rounded-xl bg-[var(--surface-3)] text-[var(--muted)]">
        {icon ?? <SearchX size={18} aria-hidden />}
      </span>
      <div className="mt-1 text-[12px] font-medium text-[var(--ink-2)]">{title}</div>
      {description ? <div className="max-w-[340px] text-[10.5px] leading-5 text-[var(--muted)]">{description}</div> : null}
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  );
}

export function ErrorState({
  error,
  onRetry,
  compact = false,
}: {
  error: DomainError;
  onRetry?: () => void;
  compact?: boolean;
}) {
  return (
    <div
      role="alert"
      className={cn(
        "flex flex-col items-center justify-center gap-2 px-6 text-center",
        compact ? "py-6" : "h-full min-h-[180px] py-12",
      )}
    >
      <span className="grid h-10 w-10 place-items-center rounded-xl bg-[var(--danger)]/10 text-[var(--danger)]">
        <AlertTriangle size={18} aria-hidden />
      </span>
      <div className="text-[12px] font-medium text-[var(--ink)]">{describeError(error)}</div>
      <div className="max-w-[380px] text-[10.5px] leading-5 text-[var(--muted)]">
        {error.message}
        {error.request_id ? <span className="ml-1 opacity-60">({error.request_id})</span> : null}
      </div>
      {onRetry ? (
        <Button size="sm" variant="secondary" onClick={onRetry} className="mt-1">
          重试
        </Button>
      ) : null}
    </div>
  );
}

export function InlineError({ error }: { error: DomainError | null }) {
  if (!error) return null;
  return (
    <p className="mt-1.5 text-[10.5px] leading-4 text-[var(--danger)]" role="alert">
      {error.field_errors ? Object.values(error.field_errors)[0] : error.message || describeError(error)}
    </p>
  );
}

export function PermissionDenied({ reason, action }: { reason: string; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 px-8 py-12 text-center" role="alert">
      <span className="grid h-10 w-10 place-items-center rounded-xl bg-[var(--warn)]/10 text-[var(--warn)]">
        <Lock size={17} aria-hidden />
      </span>
      <div className="text-[12px] font-medium text-[var(--ink)]">没有权限</div>
      <div className="max-w-[360px] text-[10.5px] leading-5 text-[var(--muted)]">{reason}</div>
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  );
}

export function TextField({
  label,
  error,
  hint,
  textarea,
  className,
  id,
  ...rest
}: (InputHTMLAttributes<HTMLInputElement> & TextareaHTMLAttributes<HTMLTextAreaElement>) & {
  label: string;
  error?: string;
  hint?: string;
  textarea?: boolean;
}) {
  const autoId = useId();
  const fieldId = id ?? autoId;
  const errorId = `${fieldId}-error`;
  return (
    <label className="block" htmlFor={fieldId}>
      <span className="mb-1 block text-[10.5px] font-medium text-[var(--ink-2)]">{label}</span>
      {textarea ? (
        <textarea
          id={fieldId}
          aria-invalid={Boolean(error) || undefined}
          aria-describedby={error ? errorId : undefined}
          className={cn(
            "min-h-[76px] w-full resize-none rounded-md border bg-[var(--surface-2)] px-3 py-2 text-[12px] text-[var(--ink)] outline-none placeholder:text-[var(--muted)]",
            error ? "border-[var(--danger)]" : "border-[var(--line-strong)]",
            className,
          )}
          {...(rest as TextareaHTMLAttributes<HTMLTextAreaElement>)}
        />
      ) : (
        <input
          id={fieldId}
          aria-invalid={Boolean(error) || undefined}
          aria-describedby={error ? errorId : undefined}
          className={cn(
            "h-9 w-full rounded-md border bg-[var(--surface-2)] px-3 text-[12px] text-[var(--ink)] outline-none placeholder:text-[var(--muted)]",
            error ? "border-[var(--danger)]" : "border-[var(--line-strong)]",
            className,
          )}
          {...rest}
        />
      )}
      {error ? (
        <span id={errorId} className="mt-1 block text-[10px] text-[var(--danger)]">
          {error}
        </span>
      ) : hint ? (
        <span className="mt-1 block text-[10px] text-[var(--muted)]">{hint}</span>
      ) : null}
    </label>
  );
}

export function Switch({
  checked,
  onChange,
  label,
  disabled,
  pending,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
  pending?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled || pending}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative h-[22px] w-[38px] shrink-0 rounded-full border transition-colors",
        checked ? "border-[var(--accent)] bg-[var(--accent)]" : "border-[var(--line-strong)] bg-[var(--surface-3)]",
        (disabled || pending) && "opacity-50",
      )}
    >
      <span
        className={cn(
          "absolute top-[2px] h-[16px] w-[16px] rounded-full bg-white shadow transition-all",
          checked ? "left-[19px]" : "left-[2px]",
        )}
      />
    </button>
  );
}

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: string;
  actions?: ReactNode;
}) {
  return (
    <header className="flex shrink-0 items-start justify-between gap-4 border-b border-[var(--line)] px-5 py-3">
      <div>
        <h1 className="text-[15px] font-semibold text-[var(--ink)]">{title}</h1>
        {description ? <p className="mt-0.5 text-[10.5px] text-[var(--muted)]">{description}</p> : null}
      </div>
      {actions ? <div className="flex items-center gap-2">{actions}</div> : null}
    </header>
  );
}

const DialogCloseContext = createContext<(() => void) | null>(null);
export const useDialogClose = () => useContext(DialogCloseContext);

export function Dialog({
  open,
  title,
  description,
  onClose,
  children,
  footer,
  width = 420,
  testId,
}: {
  open: boolean;
  title: string;
  description?: string;
  onClose: () => void;
  children?: ReactNode;
  footer?: ReactNode;
  width?: number;
  testId?: string;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const titleId = useId();

  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement as HTMLElement | null;
    const panel = panelRef.current;
    const focusables = () =>
      Array.from(
        panel?.querySelectorAll<HTMLElement>(
          'button:not([disabled]), [href], input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ) ?? [],
      );
    focusables()[0]?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
      }
      if (event.key === "Tab") {
        const items = focusables();
        if (!items.length) return;
        const first = items[0];
        const last = items[items.length - 1];
        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault();
          first.focus();
        }
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      previous?.focus?.();
    };
  }, [open, onClose]);

  if (!open) return null;
  return createPortal(
    <DialogCloseContext.Provider value={onClose}>
      <div
        className="fixed inset-0 z-50 flex items-center justify-center bg-black/35 p-6 backdrop-blur-[2px]"
        onMouseDown={(event) => {
          if (event.target === event.currentTarget) onClose();
        }}
      >
        <div
          ref={panelRef}
          role="dialog"
          aria-modal="true"
          aria-labelledby={titleId}
          data-testid={testId}
          className="pop-in w-full overflow-hidden rounded-xl border border-[var(--line-strong)] bg-[var(--surface-2)] shadow-[var(--shadow-pop)]"
          style={{ maxWidth: width }}
        >
          <div className="flex items-start justify-between gap-4 border-b border-[var(--line)] px-4 py-3">
            <div>
              <h2 id={titleId} className="text-[13px] font-semibold text-[var(--ink)]">
                {title}
              </h2>
              {description ? <p className="mt-1 text-[10.5px] leading-4 text-[var(--muted)]">{description}</p> : null}
            </div>
            <IconButton label="关闭" onClick={onClose} data-testid="dialog-close">
              <X size={14} />
            </IconButton>
          </div>
          {children ? <div className="px-4 py-3">{children}</div> : null}
          {footer ? (
            <div className="flex justify-end gap-2 border-t border-[var(--line)] bg-[var(--surface)] px-4 py-2.5">
              {footer}
            </div>
          ) : null}
        </div>
      </div>
    </DialogCloseContext.Provider>,
    document.body,
  );
}

export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel = "确认",
  danger = false,
  pending = false,
  onConfirm,
  onClose,
  children,
}: {
  open: boolean;
  title: string;
  description?: string;
  confirmLabel?: string;
  danger?: boolean;
  pending?: boolean;
  onConfirm: () => void;
  onClose: () => void;
  children?: ReactNode;
}) {
  return (
    <Dialog
      open={open}
      title={title}
      description={description}
      onClose={onClose}
      footer={
        <>
          <Button size="sm" onClick={onClose} disabled={pending}>
            取消
          </Button>
          <Button
            size="sm"
            variant={danger ? "danger" : "primary"}
            onClick={onConfirm}
            pending={pending}
            data-testid="confirm-action"
          >
            {confirmLabel}
          </Button>
        </>
      }
    >
      {children}
    </Dialog>
  );
}

export function Toasts() {
  const state = useAppState();
  const dismiss = useDismissToast();
  return (
    <div className="pointer-events-none fixed bottom-5 left-1/2 z-[60] flex -translate-x-1/2 flex-col items-center gap-2">
      {state.toasts.map((toast) => (
        <div
          key={toast.id}
          role={toast.tone === "error" ? "alert" : "status"}
          className={cn(
            "toast-in pointer-events-auto flex max-w-[420px] items-start gap-2 rounded-lg px-3.5 py-2 text-[11px] shadow-[var(--shadow-pop)]",
            toast.tone === "error"
              ? "bg-[var(--danger)] text-white"
              : "border border-[var(--line-strong)] bg-[var(--surface-2)] text-[var(--ink)]",
          )}
        >
          <span className="leading-5">{toast.text}</span>
          <button
            type="button"
            aria-label="关闭提示"
            className="mt-0.5 opacity-70 hover:opacity-100"
            onClick={() => dismiss(toast.id)}
          >
            <X size={11} />
          </button>
        </div>
      ))}
    </div>
  );
}
