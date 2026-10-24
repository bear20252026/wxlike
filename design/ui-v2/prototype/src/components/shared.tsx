import { useEffect, useState } from "react";
import { cn } from "../utils/cn";

/* ---------- WeChat logo with depth ---------- */
export function WeChatLogo({ className, mono = false }: { className?: string; mono?: boolean }) {
  const gid = `wg${Math.random().toString(36).slice(2, 7)}`;
  return (
    <svg viewBox="0 0 40 40" className={cn("drop-shadow-[0_2px_6px_rgba(7,193,96,0.35)]", className)} aria-label="微信">
      <defs>
        <linearGradient id={gid} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0%" stopColor="#0BD566" />
          <stop offset="100%" stopColor="#06A854" />
        </linearGradient>
      </defs>
      {!mono && <circle cx="20" cy="20" r="20" fill={`url(#${gid})`} />}
      {!mono && <ellipse cx="20" cy="12.5" rx="13" ry="6" fill="white" opacity="0.14" />}
      <g fill={mono ? "currentColor" : "#fff"}>
        <path d="M16.4 10c-5.3 0-9.6 3.5-9.6 7.9 0 2.5 1.3 4.7 3.4 6.1l-.8 2.7 3-1.5c1.2.4 2.6.6 4 .6h.7c-.2-.6-.3-1.2-.3-1.8 0-4.2 4-7.6 9-7.6.4 0 .8 0 1.2.1C26.2 12.8 21.7 10 16.4 10Z" />
        <circle cx="13" cy="16.5" r="1.25" fill={mono ? "#fff" : "#07C160"} />
        <circle cx="19.7" cy="16.5" r="1.25" fill={mono ? "#fff" : "#07C160"} />
        <path d="M26.7 18c-4.3 0-7.8 2.9-7.8 6.4 0 3.5 3.5 6.4 7.8 6.4 1 0 2-.2 2.9-.5l2.4 1.3-.7-2.3c1.7-1.2 2.7-3 2.7-4.9 0-3.5-3.5-6.4-7.3-6.4Z" />
        <circle cx="24.2" cy="23.5" r=".95" fill={mono ? "#fff" : "#07C160"} />
        <circle cx="29.3" cy="23.5" r=".95" fill={mono ? "#fff" : "#07C160"} />
      </g>
    </svg>
  );
}

/* ---------- Avatar with gradient fallback + presence ---------- */
const fallbackGradients = [
  "linear-gradient(135deg,#A9CBB8,#6FA287)",
  "linear-gradient(135deg,#E5C9A8,#C08A5A)",
  "linear-gradient(135deg,#B9C7E8,#7C93C9)",
  "linear-gradient(135deg,#E3B7C2,#C07A8C)",
  "linear-gradient(135deg,#B8DCCF,#6FB598)",
  "linear-gradient(135deg,#D9C9EE,#9A82C9)",
];

function hashName(name: string) {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0;
  return h % fallbackGradients.length;
}

export function Avatar({
  src,
  name = "",
  className,
  online,
  rounded = "rounded-[7px]",
  ring = true,
}: {
  src?: string;
  name?: string;
  className?: string;
  online?: boolean;
  rounded?: string;
  ring?: boolean;
}) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [src]);
  return (
    <span className={cn("relative inline-block shrink-0", className)}>
      <span
        className={cn(
          "block h-full w-full overflow-hidden text-white",
          rounded,
          ring && "ring-1 ring-black/[0.06] shadow-[0_1px_3px_rgba(0,0,0,0.08)]",
        )}
        style={!src || failed ? { background: fallbackGradients[hashName(name || "W")] } : undefined}
      >
        {src && !failed ? (
          <img src={src} alt={name} onError={() => setFailed(true)} className="h-full w-full object-cover" loading="lazy" draggable={false} />
        ) : (
          <span className="flex h-full w-full items-center justify-center text-xs font-semibold tracking-wide">
            {(name || "微").slice(0, 1)}
          </span>
        )}
      </span>
      {online && (
        <span className="absolute -bottom-0.5 -right-0.5">
          <span className="absolute inset-0 animate-ping rounded-full bg-[#07C160] opacity-40" />
          <span className="relative block h-2.5 w-2.5 rounded-full border-2 border-white bg-[#07C160] shadow-sm" />
        </span>
      )}
    </span>
  );
}

export function GroupAvatar({ images, className }: { images: string[]; className?: string }) {
  const shown = images.slice(0, 4);
  return (
    <span
      className={cn(
        "grid h-11 w-11 shrink-0 grid-cols-2 gap-[1.5px] overflow-hidden rounded-[7px] bg-[#DEDEDE] p-[1.5px] shadow-[0_1px_3px_rgba(0,0,0,0.08)] ring-1 ring-black/[0.05]",
        className,
      )}
    >
      {shown.map((img, i) => (
        <img key={i} src={img} alt="" className="h-full w-full rounded-[2px] object-cover" loading="lazy" draggable={false} />
      ))}
      {shown.length < 4 && Array.from({ length: 4 - shown.length }).map((_, i) => <span key={i} className="rounded-[2px] bg-[#E8E8E8]" />)}
    </span>
  );
}

export function WeChatFileIcon({ className }: { className?: string }) {
  return (
    <span className={cn("relative grid place-items-center overflow-hidden bg-gradient-to-br from-[#FAFAFA] to-[#ECECEC] ring-1 ring-black/[0.05]", className)}>
      <WeChatLogo className="h-3/5 w-3/5" />
    </span>
  );
}

/* ---------- Live phone status bar ---------- */
export function useNow() {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 15000);
    return () => clearInterval(t);
  }, []);
  return now;
}

export function PhoneStatusBar({ dark = true, bg = "transparent" }: { dark?: boolean; bg?: string }) {
  const now = useNow();
  const hh = now.getHours();
  const mm = String(now.getMinutes()).padStart(2, "0");
  const color = dark ? "text-[#111]" : "text-white";
  return (
    <div className={cn("relative z-30 flex h-[30px] shrink-0 items-center justify-between px-7 pt-1.5 text-[11px] font-semibold tabular-nums", color, bg)}>
      <span className="w-12 tracking-[-0.01em]">{hh}:{mm}</span>
      <div className="flex items-center gap-1.5">
        <svg viewBox="0 0 18 12" className="h-[10px] w-[15px] fill-current">
          <rect x="0" y="8" width="2.6" height="4" rx="0.8" />
          <rect x="4" y="5.5" width="2.6" height="6.5" rx="0.8" />
          <rect x="8" y="3" width="2.6" height="9" rx="0.8" />
          <rect x="12" y="0" width="2.6" height="12" rx="0.8" opacity="0.35" />
        </svg>
        <svg viewBox="0 0 16 12" className="h-[10px] w-[14px] fill-current">
          <path d="M8 10.2a1.35 1.35 0 1 1 0 2.7 1.35 1.35 0 0 1 0-2.7ZM4.6 8.4l1.3 1.3a3.2 3.2 0 0 1 4.2 0l1.3-1.3a5 5 0 0 0-6.8 0ZM1.4 5.4 2.7 6.7a7.2 7.2 0 0 1 10.6 0l1.3-1.3a9.1 9.1 0 0 0-13.2 0Z" transform="translate(0 -2.5)" />
        </svg>
        <span className="relative ml-0.5 flex h-[11px] w-[22px] items-center rounded-[3.5px] border border-current opacity-90">
          <span className="ml-[1px] block h-[7px] w-[15px] rounded-[2px] bg-current" />
          <span className="absolute -right-[2.5px] top-1/2 h-[4px] w-[1.5px] -translate-y-1/2 rounded-r bg-current opacity-60" />
        </span>
      </div>
    </div>
  );
}

/* ---------- iOS switch ---------- */
export function Switch({ on, onChange, small = false }: { on: boolean; onChange?: () => void; small?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={on ? "关闭" : "开启"}
      onClick={(e) => {
        e.stopPropagation();
        onChange?.();
      }}
      className={cn(
        "relative shrink-0 rounded-full transition-all duration-300 ease-[cubic-bezier(0.22,1,0.36,1)]",
        small ? "h-[20px] w-[33px]" : "h-[24px] w-[41px]",
        on ? "bg-[#07C160] shadow-[inset_0_1px_3px_rgba(0,0,0,0.12)]" : "bg-[#E4E4E4] shadow-[inset_0_1px_3px_rgba(0,0,0,0.08)]",
      )}
    >
      <span
        className={cn(
          "absolute top-[2px] rounded-full bg-white transition-all duration-300 ease-[cubic-bezier(0.22,1,0.36,1)]",
          "shadow-[0_2px_5px_rgba(0,0,0,0.18),0_0_1px_rgba(0,0,0,0.2)]",
          small ? "h-[16px] w-[16px]" : "h-[20px] w-[20px]",
          on ? (small ? "translate-x-[15px]" : "translate-x-[19px]") : "translate-x-[2px]",
        )}
      />
    </button>
  );
}

export function SectionLabel({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <div className={cn("flex items-center gap-2 text-[10px] font-semibold tracking-[0.16em] text-[#9B9B93] uppercase", className)}>
      {children}
    </div>
  );
}

/* ---------- Badge with pop ---------- */
export function WeChatBadge({ count, className }: { count: number; className?: string }) {
  return (
    <span
      className={cn(
        "pop-in grid h-[17px] min-w-[17px] place-items-center rounded-full bg-gradient-to-b from-[#FB5E5E] to-[#F54848] px-1 text-[10px] font-semibold leading-none text-white shadow-[0_2px_6px_rgba(250,81,81,0.4)] ring-1 ring-white/40",
        className,
      )}
    >
      {count > 99 ? "99+" : count}
    </span>
  );
}

export function UnreadDot({ className }: { className?: string }) {
  return <span className={cn("pop-in block h-[9px] w-[9px] rounded-full bg-[#FA5151] shadow-[0_1px_4px_rgba(250,81,81,0.5)] ring-2 ring-white", className)} />;
}

/* ---------- Typing indicator ---------- */
export function TypingDots({ className }: { className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-[3px] rounded-full bg-white px-2.5 py-2 shadow-sm", className)}>
      {[0, 1, 2].map((i) => (
        <span key={i} className="typing-dot h-[5px] w-[5px] rounded-full bg-[#8a8a8a]" />
      ))}
    </span>
  );
}

/* ---------- Voice waveform ---------- */
export function Waveform({ bars = 14, playing = false, tone = "dark" }: { bars?: number; playing?: boolean; tone?: "dark" | "light" }) {
  const heights = [5, 11, 7, 14, 9, 16, 6, 12, 8, 15, 6, 10, 13, 7, 11, 5];
  return (
    <span className="flex items-center gap-[2px]">
      {Array.from({ length: bars }).map((_, i) => (
        <span
          key={i}
          className={cn("w-[2px] rounded-full", tone === "dark" ? "bg-[#4a4a4a]/55" : "bg-white/75", playing && "wave-bar")}
          style={{ height: heights[i % heights.length], animationDelay: `${i * 70}ms` }}
        />
      ))}
    </span>
  );
}

/* ---------- Date / system pill ---------- */
export function SystemPill({ children, dark = false }: { children: React.ReactNode; dark?: boolean }) {
  return (
    <div className="flex justify-center py-1">
      <span
        className={cn(
          "rounded-[4px] px-2 py-[3px] text-[10px] leading-4",
          dark ? "bg-white/10 text-white/60" : "bg-black/[0.07] text-white",
        )}
        style={dark ? undefined : { background: "rgba(0,0,0,0.28)", color: "#fff" }}
      >
        {children}
      </span>
    </div>
  );
}

/* ---------- Search pill ---------- */
export function SearchPill({ text = "搜索", onClick, dark = false }: { text?: string; onClick?: () => void; dark?: boolean }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "flex h-[30px] w-full items-center justify-center gap-1.5 rounded-[7px] text-[#9a9a9a] transition active:scale-[0.99]",
        dark ? "bg-white/10 text-white/50" : "bg-[#E9E9E9]/90",
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

/* ---------- Bottom sheet handle ---------- */
export function SheetHandle({ dark = false }: { dark?: boolean }) {
  return (
    <div className="flex justify-center pb-1 pt-2">
      <span className={cn("h-[4px] w-[36px] rounded-full", dark ? "bg-white/25" : "bg-black/15")} />
    </div>
  );
}

/* ---------- Empty state ---------- */
export function EmptyState({ icon, title, desc }: { icon: React.ReactNode; title: string; desc?: string }) {
  return (
    <div className="flex flex-col items-center px-8 py-14 text-center">
      <span className="grid h-14 w-14 place-items-center rounded-2xl bg-[#EFF0EA] text-[#9aa39a]">{icon}</span>
      <div className="mt-3 text-[12px] font-medium text-[#555e55]">{title}</div>
      {desc && <div className="mt-1 text-[10px] leading-5 text-[#9aa39b]">{desc}</div>}
    </div>
  );
}

export function EmojiSticker({ children, className }: { children: React.ReactNode; className?: string }) {
  return <div className={cn("grid place-items-center text-4xl drop-shadow-sm", className)}>{children}</div>;
}

export const stickerSets = [
  ["🌿", "🌷", "✦", "☕", "🥐", "🌙", "🫧", "🌤️"],
  ["🐱", "🐰", "🐻", "🦊", "🐼", "🐥", "🦋", "🐳"],
  ["😊", "🥹", "😎", "🤍", "😴", "🤭", "😌", "🙌"],
  ["🎨", "📷", "🎧", "📖", "✈️", "⛰️", "🌊", "🍵"],
];

/* ---------- Muted bell icon (accurate) ---------- */
export function MutedIcon({ size = 12, className }: { size?: number; className?: string }) {
  return (
    <svg width={size} height={size} viewBox="0 0 12 12" fill="none" className={className}>
      <path d="M6 1a3.5 3.5 0 0 0-3.5 3.5v2L1 8h10L9.5 6.5v-2A3.5 3.5 0 0 0 6 1Z" stroke="currentColor" strokeWidth="1" strokeLinejoin="round" />
      <path d="M5 9.5a1 1 0 0 0 2 0" stroke="currentColor" strokeWidth="1" strokeLinecap="round" />
      <path d="M1.5 1.5l9 9" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" />
    </svg>
  );
}
