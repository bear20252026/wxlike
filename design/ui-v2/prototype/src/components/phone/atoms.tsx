import { ChevronLeft, ChevronRight, MessageCircle, Compass, UserRound, Users } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "../../utils/cn";
import { WeChatBadge } from "../shared";

export function PhoneBack({ onClick, dark = false, label }: { onClick: () => void; dark?: boolean; label?: string }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "pressable -ml-1 flex h-8 items-center gap-0 rounded-full pr-1",
        dark ? "text-white" : "text-[#191919]",
      )}
      aria-label="返回"
    >
      <ChevronLeft size={26} strokeWidth={1.6} className="-mr-1" />
      {label && <span className="text-[13px]">{label}</span>}
    </button>
  );
}

export function MobileTop({
  title,
  subtitle,
  onBack,
  backLabel,
  left,
  right,
  tappableTitle,
  onTitleClick,
  transparent = false,
}: {
  title?: string;
  subtitle?: string;
  onBack?: () => void;
  backLabel?: string;
  left?: ReactNode;
  right?: ReactNode;
  tappableTitle?: boolean;
  onTitleClick?: () => void;
  transparent?: boolean;
}) {
  return (
    <header
      className={cn(
        "relative z-20 shrink-0 pt-[30px]",
        transparent ? "bg-transparent" : "hairline-b bg-[#EDEDED]/92 backdrop-blur-xl",
      )}
    >
      <div className="relative flex h-[44px] items-center justify-between px-2.5">
        <div className="flex min-w-[64px] items-center">
          {onBack && <PhoneBack onClick={onBack} label={backLabel} />}
          {left}
        </div>
        <button onClick={onTitleClick} disabled={!tappableTitle} className="min-w-0 max-w-[190px] px-1 text-center">
          <div className="truncate text-[15px] font-semibold tracking-[0.01em] text-[#1A1A1A]">{title}</div>
          {subtitle && <div className="mt-0.5 truncate text-[8.5px] font-normal text-[#999]">{subtitle}</div>}
        </button>
        <div className="flex min-w-[64px] items-center justify-end gap-3 text-[#242424]">{right}</div>
      </div>
    </header>
  );
}

export function MobileCell({
  icon,
  label,
  sublabel,
  right,
  onClick,
  badge,
  border = true,
  className,
  inset = true,
}: {
  icon?: ReactNode;
  label: string;
  sublabel?: string;
  right?: ReactNode;
  onClick?: () => void;
  badge?: ReactNode;
  border?: boolean;
  className?: string;
  inset?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "pressable group relative flex min-h-[52px] w-full items-center gap-3 bg-white px-4 text-left active:bg-[#F2F2F2]",
        className,
      )}
    >
      {icon}
      <span className="relative min-w-0 flex-1 py-2.5">
        <span className="flex items-center gap-2">
          <span className="block truncate text-[13.5px] text-[#1a1a1a]">{label}</span>
          {badge}
        </span>
        {sublabel && <span className="mt-0.5 block truncate text-[10px] text-[#A6A6A6]">{sublabel}</span>}
        {border && (
          <span
            className={cn("absolute -bottom-[1px] right-[-16px] h-px bg-[#EFEFEF]", inset ? "left-0" : "-left-[52px]")}
          />
        )}
      </span>
      {right}
      <ChevronRight size={15} className="shrink-0 text-[#C7C7CC] transition-transform group-active:translate-x-0.5" strokeWidth={1.8} />
    </button>
  );
}

export function MobileTile({
  icon,
  color,
  size = "md",
  gradient,
}: {
  icon: ReactNode;
  color: string;
  size?: "sm" | "md" | "lg";
  gradient?: string;
}) {
  return (
    <span
      className={cn(
        "relative grid shrink-0 place-items-center overflow-hidden text-white shadow-[0_2px_8px_rgba(0,0,0,0.1)]",
        {
          "h-6 w-6 rounded-[7px]": size === "sm",
          "h-[30px] w-[30px] rounded-[8px]": size === "md",
          "h-9 w-9 rounded-[10px]": size === "lg",
        },
      )}
      style={{ background: gradient ?? color }}
    >
      <span className="pointer-events-none absolute inset-x-0 top-0 h-1/2 bg-gradient-to-b from-white/25 to-transparent" />
      <span className="relative">{icon}</span>
    </span>
  );
}

export function GroupTitle({ children, className, action }: { children: ReactNode; className?: string; action?: ReactNode }) {
  return (
    <div className={cn("flex items-center justify-between px-4 pb-1.5 pt-3.5", className)}>
      <span className="text-[9.5px] font-medium tracking-[0.08em] text-[#8e8e93]">{children}</span>
      {action}
    </div>
  );
}

export function ScreenShell({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("screen-enter flex h-full flex-col overflow-hidden", className)}>{children}</div>;
}

export function SearchBar({ text = "搜索", onClick, className }: { text?: string; onClick?: () => void; className?: string }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "pressable flex h-[32px] w-full items-center justify-center gap-1.5 rounded-[8px] bg-white text-[#9a9a9a] shadow-[0_1px_2px_rgba(0,0,0,0.03)] ring-1 ring-black/[0.03]",
        className,
      )}
    >
      <svg viewBox="0 0 16 16" className="h-[13px] w-[13px]" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round">
        <circle cx="7" cy="7" r="4.5" />
        <path d="M10.5 10.5 14 14" />
      </svg>
      <span className="text-[12px]">{text}</span>
    </button>
  );
}

export function TabBar({
  active,
  onChange,
}: {
  active: "chats" | "contacts" | "discover" | "me";
  onChange: (tab: "chats" | "contacts" | "discover" | "me") => void;
}) {
  const items = [
    { id: "chats" as const, label: "微信", icon: MessageCircle, badge: 8 },
    { id: "contacts" as const, label: "通讯录", icon: Users },
    { id: "discover" as const, label: "发现", icon: Compass, dot: true },
    { id: "me" as const, label: "我", icon: UserRound },
  ];
  return (
    <nav className="hairline-t relative z-20 flex h-[68px] shrink-0 items-start bg-[#F7F7F7]/94 px-1 pt-[6px] backdrop-blur-xl">
      {items.map((item) => {
        const activeItem = active === item.id;
        return (
          <button
            key={item.id}
            onClick={() => onChange(item.id)}
            className="pressable relative flex flex-1 flex-col items-center gap-[3px] py-0.5"
          >
            <span className="relative">
              <item.icon
                size={24}
                strokeWidth={activeItem ? 2.1 : 1.6}
                className={cn("transition-colors", activeItem ? "text-[#07C160]" : "text-[#4a4a4a]")}
                fill={activeItem ? "rgba(7,193,96,0.08)" : "none"}
              />
              {item.badge && (
                <span className="absolute -right-3.5 -top-1.5">
                  <WeChatBadge count={item.badge} />
                </span>
              )}
              {item.dot && !activeItem && (
                <span className="absolute -right-[2px] top-[1px] h-[7px] w-[7px] rounded-full bg-[#FA5151] ring-[1.5px] ring-[#F7F7F7]" />
              )}
            </span>
            <span className={cn("text-[9.5px] leading-3", activeItem ? "font-semibold text-[#07C160]" : "text-[#5a5a5a]")}>
              {item.label}
            </span>
          </button>
        );
      })}
    </nav>
  );
}
