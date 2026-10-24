import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDownRight,
  ArrowLeft,
  ArrowRight,
  ArrowUpRight,
  Check,
  ChevronDown,
  Copy,
  Layers,
  Menu,
  Monitor,
  Pause,
  Play,
  Smartphone,
  Sparkles,
  X,
} from "lucide-react";
import { WeChatLogo } from "./components/shared";
import { DesktopWeChat, type DesktopPage } from "./components/desktop/DesktopWeChat";
import { PhoneWeChat } from "./components/phone/PhoneWeChat";
import { DESKTOP_PAGES, MOBILE_PAGES, TOTAL_SCREENS, type MobileScreen } from "./components/phone/types";
import { cn } from "./utils/cn";

type DeviceView = "both" | "desktop" | "mobile";
type CatalogFilter = "all" | "desktop" | "mobile";

/* ---------- stacked toasts ---------- */
type Toast = { id: number; text: string };
function useToasts() {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const idRef = useRef(0);
  const notify = useCallback((text: string) => {
    const id = ++idRef.current;
    setToasts((t) => [...t.slice(-2), { id, text }]);
    window.setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 2600);
  }, []);
  return { toasts, notify };
}

function ScrollLink({ target, children, className }: { target: string; children: React.ReactNode; className?: string }) {
  return (
    <button onClick={() => document.getElementById(target)?.scrollIntoView({ behavior: "smooth", block: "start" })} className={className}>
      {children}
    </button>
  );
}

/* ---------- scroll progress + reveal ---------- */
function useScrollProgress() {
  const [p, setP] = useState(0);
  useEffect(() => {
    const onScroll = () => {
      const h = document.documentElement.scrollHeight - window.innerHeight;
      setP(h > 0 ? Math.min(1, window.scrollY / h) : 0);
    };
    window.addEventListener("scroll", onScroll, { passive: true });
    onScroll();
    return () => window.removeEventListener("scroll", onScroll);
  }, []);
  return p;
}

function useReveal() {
  useEffect(() => {
    const els = Array.from(document.querySelectorAll(".reveal"));
    const io = new IntersectionObserver(
      (entries) => entries.forEach((e) => e.isIntersecting && e.target.classList.add("revealed")),
      { threshold: 0.12 },
    );
    els.forEach((el) => io.observe(el));
    return () => io.disconnect();
  }, []);
}

/* ---------- custom page picker ---------- */
function PagePicker<T extends string>({
  label,
  value,
  options,
  onChange,
  accent = false,
}: {
  label: string;
  value: T;
  options: { id: T; title: string; english: string }[];
  onChange: (v: T) => void;
  accent?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onClick = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", onClick);
    return () => window.removeEventListener("mousedown", onClick);
  }, []);
  const current = options.find((o) => o.id === value);
  const idx = options.findIndex((o) => o.id === value);
  return (
    <div ref={ref} className="relative">
      <button onClick={() => setOpen(!open)} className="pressable flex items-center gap-2 rounded-full py-1 pl-1 pr-2">
        <span className={cn("h-1.5 w-1.5 rounded-full", accent ? "bg-[#07C160]" : "bg-[#A7ADA4]")} />
        <span className="hidden text-[8px] font-semibold tracking-[0.14em] text-[#969C93] uppercase lg:block">{label}</span>
        <span className="text-[10.5px] font-semibold text-[#2D342D]">{current?.title}</span>
        <span className="rounded bg-[#ECEEE7] px-1 py-0.5 text-[7.5px] tabular-nums text-[#7c847c]">{String(idx + 1).padStart(2, "0")}/{String(options.length).padStart(2, "0")}</span>
        <ChevronDown size={11} className={cn("text-[#8E958C] transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <div className="pop-in absolute left-1/2 top-[calc(100%+10px)] z-50 w-[248px] -translate-x-1/2 overflow-hidden rounded-xl border border-black/[0.08] bg-white/98 shadow-[0_20px_60px_rgba(20,32,24,0.22)] backdrop-blur">
          <div className="border-b border-[#F0F0F0] bg-[#FAFAF7] px-3 py-2 text-[8px] font-semibold tracking-[0.14em] text-[#9aa39a]">{label} · 选择页面</div>
          <div className="scrollbar-thin max-h-[264px] overflow-y-auto p-1.5">
            {options.map((o, i) => (
              <button
                key={o.id}
                onClick={() => { onChange(o.id); setOpen(false); }}
                className={cn(
                  "flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left transition",
                  o.id === value ? "bg-[#EFFBF3]" : "hover:bg-[#F5F5F2]",
                )}
              >
                <span className={cn("text-[8px] tabular-nums", o.id === value ? "font-bold text-[#07A858]" : "text-[#bbb]")}>{String(i + 1).padStart(2, "0")}</span>
                <span className={cn("flex-1 text-[11px]", o.id === value ? "font-semibold text-[#1e2a21]" : "text-[#444]")}>{o.title}</span>
                <span className="font-serif text-[8px] italic text-[#aaa]">{o.english}</span>
                {o.id === value && <Check size={12} className="text-[#07A858]" strokeWidth={3} />}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function DeviceToggle({ value, onChange }: { value: DeviceView; onChange: (v: DeviceView) => void }) {
  const items: { id: DeviceView; label: string; icon: typeof Layers }[] = [
    { id: "both", label: "双端", icon: Layers },
    { id: "desktop", label: "桌面", icon: Monitor },
    { id: "mobile", label: "手机", icon: Smartphone },
  ];
  return (
    <div className="relative flex items-center rounded-full border border-black/[0.07] bg-[#EFEFE8] p-[3px]">
      {items.map((item) => (
        <button
          key={item.id}
          onClick={() => onChange(item.id)}
          className={cn(
            "relative z-10 flex items-center gap-1.5 rounded-full px-3.5 py-[7px] text-[10px] font-medium transition-all duration-200",
            value === item.id ? "bg-[#1F2922] text-white shadow-[0_4px_12px_rgba(31,41,34,0.3)]" : "text-[#82887D] hover:text-[#3F463E]",
          )}
        >
          <item.icon size={12} strokeWidth={2} />
          {item.label}
        </button>
      ))}
    </div>
  );
}

/* ---------- miniature arts ---------- */
function MiniScreenArt({ type, tone = "light" }: { type: string; tone?: "light" | "dark" }) {
  const dark = tone === "dark";
  const isPhoto = ["moments", "channels", "reading", "call", "profile"].includes(type);
  const isGrid = ["discover", "mini", "cards", "stickers", "status"].includes(type);
  const isPay = ["wallet", "pay", "redpacket", "transfer"].includes(type);
  const isChat = ["chats", "chat"].includes(type);
  const isMap = type === "location";
  const isLogin = type === "login";
  return (
    <div className={cn("relative h-[132px] w-[78px] overflow-hidden rounded-[11px] border shadow-[0_4px_14px_rgba(0,0,0,0.08)]", dark ? "border-white/10 bg-[#161B17]" : "border-black/[0.06] bg-[#F4F4EF]")}>
      <div className={cn("absolute left-1/2 top-[5px] h-[8px] w-[30px] -translate-x-1/2 rounded-full", dark ? "bg-black" : "bg-[#1a1a1a]")} />
      <div className="flex items-center justify-between px-2 pt-[16px]">
        <span className={cn("h-[3px] w-4 rounded-full", dark ? "bg-white/30" : "bg-black/15")} />
        <span className={cn("h-[3px] w-2 rounded-full", dark ? "bg-white/30" : "bg-black/15")} />
      </div>
      {isLogin ? (
        <div className="mt-3 flex flex-col items-center">
          <span className="grid h-7 w-7 place-items-center rounded-full bg-[#07C160] text-[10px] text-white">微</span>
          <span className={cn("mt-2 h-[3px] w-10 rounded-full", dark ? "bg-white/30" : "bg-black/15")} />
          <span className="mt-1.5 h-[14px] w-[52px] rounded-[4px] bg-[#07C160]" />
          <span className="mt-1.5 h-[3px] w-8 rounded-full bg-black/10" />
        </div>
      ) : isMap ? (
        <div className="mx-1.5 mt-1.5 h-[52px] overflow-hidden rounded-[4px] bg-[#DCE4D8]">
          <span className="mx-auto mt-3 block h-[14px] w-[2px] rounded bg-white" />
          <span className="mx-auto mt-0 block h-2.5 w-2.5 rounded-full bg-[#07C160] ring-2 ring-white" />
        </div>
      ) : isPhoto ? (
        <div className="relative mx-1.5 mt-1.5 h-[54px] overflow-hidden rounded-[4px] bg-gradient-to-br from-[#7A9484] via-[#5E7A68] to-[#3E5646]">
          <span className="absolute left-1.5 top-1.5 h-[3px] w-8 rounded-full bg-white/70" />
          <span className="absolute bottom-1.5 left-1.5 h-[3px] w-10 rounded-full bg-white/35" />
          <span className="absolute right-1.5 bottom-1.5 grid h-3.5 w-3.5 place-items-center rounded-full bg-white/30 backdrop-blur"><span className="ml-[1px] h-0 w-0 border-y-[2.5px] border-l-[4px] border-y-transparent border-l-white" /></span>
          <span className="absolute left-1.5 bottom-4 h-4 w-4 rounded-full bg-white/50 ring-1 ring-white/60" />
        </div>
      ) : isPay ? (
        <div className={cn("mx-1.5 mt-1.5 rounded-[5px] p-2", type === "redpacket" ? "bg-gradient-to-br from-[#E2574C] to-[#C93A30]" : "bg-gradient-to-br from-[#0BD566] to-[#06A854]")}>
          <span className="block h-[3px] w-6 rounded-full bg-white/50" />
          <span className="mt-1.5 block text-[8px] font-bold leading-none text-white tabular-nums">¥ 12,860</span>
          <span className="mx-auto mt-2 grid h-7 w-7 grid-cols-4 gap-[1px] rounded-[3px] bg-white p-[3px]">
            {Array.from({ length: 16 }).map((_, i) => <span key={i} className={i % 3 === 0 ? "bg-[#222]" : "bg-white"} />)}
          </span>
        </div>
      ) : isGrid ? (
        <div className="mx-1.5 mt-2 grid grid-cols-4 gap-[5px]">
          {Array.from({ length: 8 }).map((_, i) => (
            <span key={i} className="flex flex-col items-center gap-[3px]">
              <span className={cn("h-[13px] w-[13px] rounded-[4px]", i % 4 === 0 ? "bg-[#B7D7C1]" : i % 4 === 1 ? "bg-[#F2D9A8]" : i % 4 === 2 ? "bg-[#C3D6EE]" : "bg-white shadow-sm")} />
              <span className={cn("h-[2px] w-3 rounded-full", dark ? "bg-white/20" : "bg-black/10")} />
            </span>
          ))}
        </div>
      ) : isChat ? (
        <div className="mt-2 space-y-[5px] px-2">
          <div className="flex justify-center"><span className="rounded-[2px] bg-black/10 px-1 py-[1px] text-[3px] text-white">14:26</span></div>
          <div className="flex items-center gap-1"><span className="h-[9px] w-[9px] rounded-[2px] bg-[#D8C6B4]" /><span className="h-[10px] w-[30px] rounded-[2px] bg-white shadow-sm" /></div>
          <div className="flex items-center justify-end gap-1"><span className="h-[10px] w-[36px] rounded-[2px] bg-[#95EC69]" /><span className="h-[9px] w-[9px] rounded-[2px] bg-[#B7D7C1]" /></div>
          <div className="flex items-center gap-1"><span className="h-[9px] w-[9px] rounded-[2px] bg-[#D8C6B4]" /><span className="h-[14px] w-[26px] rounded-[2px] bg-gradient-to-br from-[#B9CFBE] to-[#8FAF95]" /></div>
          <div className="flex items-center justify-end gap-1"><span className="h-[10px] w-[24px] rounded-[2px] bg-[#95EC69]" /><span className="h-[9px] w-[9px] rounded-[2px] bg-[#B7D7C1]" /></div>
        </div>
      ) : (
        <div className="mt-2 space-y-[4px] px-1.5">
          {Array.from({ length: 4 }).map((_, i) => (
            <div key={i} className={cn("flex items-center gap-1.5 rounded-[4px] p-[5px]", dark ? "bg-white/[0.06]" : "bg-white shadow-sm")}>
              <span className={cn("h-[11px] w-[11px] rounded-[3px]", ["bg-[#B9D8C2]", "bg-[#EDD3A8]", "bg-[#C3D3EC]", "bg-[#E8BFC6]"][i])} />
              <span className="min-w-0 flex-1"><span className={cn("block h-[3px] w-[65%] rounded-full", dark ? "bg-white/30" : "bg-black/15")} /><span className={cn("mt-[3px] block h-[2.5px] w-[85%] rounded-full", dark ? "bg-white/10" : "bg-black/[0.07]")} /></span>
            </div>
          ))}
        </div>
      )}
      <div className={cn("absolute inset-x-0 bottom-0 flex h-[17px] items-center justify-around border-t", dark ? "border-white/10 bg-black/50" : "border-black/[0.05] bg-white/85 backdrop-blur")}>
        {["微信", "通讯录", "发现", "我"].map((t, i) => <span key={t} className={cn("text-[4.5px]", i === 0 ? "font-bold text-[#07C160]" : dark ? "text-white/40" : "text-black/25")}>{t}</span>)}
      </div>
    </div>
  );
}

function DesktopScreenArt({ type }: { type: string }) {
  const isChat = ["chats", "files", "contacts"].includes(type);
  const isGrid = type === "mini";
  const isPay = ["wallet", "login"].includes(type);
  return (
    <div className="h-[112px] w-full overflow-hidden rounded-[9px] border border-black/[0.06] bg-[#F5F5F0] shadow-[0_4px_14px_rgba(0,0,0,0.06)]">
      <div className="flex h-[15px] items-center gap-[5px] border-b border-black/[0.05] bg-gradient-to-b from-[#F0F0F0] to-[#E9E9E9] px-2">
        <span className="h-[7px] w-[7px] rounded-full bg-[#FF5F57] shadow-inner" /><span className="h-[7px] w-[7px] rounded-full bg-[#FEBC2E] shadow-inner" /><span className="h-[7px] w-[7px] rounded-full bg-[#28C840] shadow-inner" />
        <span className="mx-auto h-[5px] w-16 rounded-full bg-black/[0.07]" />
      </div>
      <div className="flex h-[calc(100%-15px)]">
        <div className="flex w-[15px] flex-col items-center gap-[5px] bg-[#2A2A2A] py-2">
          <span className="h-[9px] w-[9px] rounded-[3px] bg-gradient-to-br from-[#C9D8CB] to-[#9AB5A0]" />
          <span className="h-[9px] w-[9px] rounded-[3px] bg-[#07C160] shadow-[0_0_6px_rgba(7,193,96,0.6)]" />
          <span className="h-[9px] w-[9px] rounded-[3px] bg-white/15" />
          <span className="h-[9px] w-[9px] rounded-[3px] bg-white/15" />
          <span className="mt-auto h-[9px] w-[9px] rounded-[3px] bg-white/15" />
        </div>
        <div className="w-[52px] shrink-0 border-r border-black/[0.05] bg-white/70 p-[7px]">
          <span className="block h-[9px] w-full rounded-[4px] bg-[#E8E8E8]" />
          {type === "login" ? (
            <span className="mx-auto mt-3 grid h-9 w-9 grid-cols-4 gap-[1px] rounded-[3px] border border-black/[0.06] bg-white p-1">
              {Array.from({ length: 16 }).map((_, i) => <span key={i} className={i % 3 === 0 ? "bg-[#222]" : "bg-white"} />)}
            </span>
          ) : (
            Array.from({ length: 4 }).map((_, i) => (
              <div key={i} className={cn("mt-[7px] flex gap-[5px] rounded-[3px] p-[2px]", i === 0 && "bg-[#E2E2E2]")}>
                <span className={cn("h-[13px] w-[13px] shrink-0 rounded-[3px]", ["bg-[#C9D4C4]", "bg-[#E4CBA4]", "bg-[#BCCFE8]", "bg-[#DFC2C7]"][i])} />
                <span className="flex-1 pt-[1px]"><span className="block h-[3px] w-[90%] rounded-full bg-black/10" /><span className="mt-[3px] block h-[2.5px] w-[60%] rounded-full bg-black/[0.05]" /></span>
              </div>
            ))
          )}
        </div>
        <div className="flex min-w-0 flex-1 flex-col bg-[#F0F0EB]">
          <div className="flex h-[17px] items-center gap-2 border-b border-black/[0.05] bg-[#F6F6F2] px-2.5">
            <span className="h-[13px] w-[13px] rounded-[3px] bg-[#C9D4C4]" />
            <span className="block h-[5px] w-12 rounded-full bg-black/10" />
          </div>
          <div className="flex-1 p-2.5">
            {isChat ? (
              <div className="space-y-[7px]">
                <span className="block h-[11px] w-[36%] rounded-[3px] bg-white shadow-sm" />
                <span className="ml-auto block h-[11px] w-[48%] rounded-[3px] bg-[#95EC69] shadow-sm" />
                <span className="block h-[20px] w-[30%] rounded-[3px] bg-gradient-to-br from-[#B9CFBE] to-[#8FAF95]" />
                <span className="ml-auto block h-[11px] w-[38%] rounded-[3px] bg-[#95EC69] shadow-sm" />
              </div>
            ) : isGrid ? (
              <div className="grid grid-cols-4 gap-[7px]">{Array.from({ length: 8 }).map((_, i) => <span key={i} className="flex aspect-square flex-col items-center justify-center gap-1 rounded-[4px] bg-white shadow-sm"><span className={cn("h-[11px] w-[11px] rounded-[3px]", ["bg-[#B9D8C2]", "bg-[#EDD3A8]", "bg-[#C3D3EC]", "bg-[#E8BFC6]"][i % 4])} /></span>)}</div>
            ) : isPay ? (
              <div>
                <div className="rounded-[5px] bg-gradient-to-br from-[#0BD566] to-[#06A854] p-2 shadow-sm"><span className="block h-[4px] w-10 rounded bg-white/50" /><span className="mt-1.5 block text-[9px] font-bold text-white tabular-nums">¥ 12,860</span></div>
                <div className="mt-2 space-y-[5px]">{Array.from({ length: 2 }).map((_, i) => <span key={i} className="block h-[11px] w-full rounded-[3px] bg-white shadow-sm" />)}</div>
              </div>
            ) : (
              <div>
                <div className="h-[34px] rounded-[4px] bg-gradient-to-br from-[#8FAF95] via-[#6E8F75] to-[#4C6A54]" />
                <div className="mt-[7px] grid grid-cols-3 gap-[5px]">{Array.from({ length: 3 }).map((_, i) => <span key={i} className="h-[22px] rounded-[3px] bg-white shadow-sm" />)}</div>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export default function App() {
  const [desktopPage, setDesktopPage] = useState<DesktopPage>("chats");
  const [mobileScreen, setMobileScreen] = useState<MobileScreen>("chats");
  const [device, setDevice] = useState<DeviceView>("both");
  const [catalogFilter, setCatalogFilter] = useState<CatalogFilter>("all");
  const [menuOpen, setMenuOpen] = useState(false);
  const [heroLoaded, setHeroLoaded] = useState(false);
  const [touring, setTouring] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const { toasts, notify } = useToasts();
  const stageRef = useRef<HTMLDivElement>(null);
  const progress = useScrollProgress();
  useReveal();

  useEffect(() => {
    const t = window.setTimeout(() => setHeroLoaded(true), 60);
    return () => window.clearTimeout(t);
  }, []);

  /* keyboard navigation */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      if (e.key === "ArrowRight") {
        setMobileScreen((s) => MOBILE_PAGES[(MOBILE_PAGES.findIndex((p) => p.id === s) + 1) % MOBILE_PAGES.length].id);
      } else if (e.key === "ArrowLeft") {
        setMobileScreen((s) => MOBILE_PAGES[(MOBILE_PAGES.findIndex((p) => p.id === s) - 1 + MOBILE_PAGES.length) % MOBILE_PAGES.length].id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  /* auto tour */
  useEffect(() => {
    if (!touring) return;
    const t = setInterval(() => {
      setMobileScreen((s) => MOBILE_PAGES[(MOBILE_PAGES.findIndex((p) => p.id === s) + 1) % MOBILE_PAGES.length].id);
    }, 2800);
    return () => clearInterval(t);
  }, [touring]);

  const revealDesktop = useCallback((page: DesktopPage) => {
    setDesktopPage(page);
    const tooNarrow = typeof window !== "undefined" && window.innerWidth < 768;
    setDevice((d) => (d === "mobile" || (d === "both" && tooNarrow) ? "desktop" : d));
  }, []);
  const revealMobile = useCallback((page: MobileScreen) => {
    setMobileScreen(page);
    setDevice((d) => (d === "desktop" ? "mobile" : d));
  }, []);
  const openDesktop = (page: DesktopPage) => {
    revealDesktop(page);
    setTouring(false);
    document.getElementById("showcase")?.scrollIntoView({ behavior: "smooth", block: "start" });
    notify(`桌面端 · ${DESKTOP_PAGES.find((p) => p.id === page)?.title}`);
  };
  const openMobile = (page: MobileScreen) => {
    revealMobile(page);
    setTouring(false);
    document.getElementById("showcase")?.scrollIntoView({ behavior: "smooth", block: "start" });
    notify(`移动端 · ${MOBILE_PAGES.find((p) => p.id === page)?.title}`);
  };

  const copyHex = (hex: string) => {
    navigator.clipboard?.writeText(hex).catch(() => {});
    setCopied(hex);
    notify(`已复制 ${hex}`);
    setTimeout(() => setCopied(null), 1600);
  };

  const activeDesktop = DESKTOP_PAGES.find((p) => p.id === desktopPage)!;
  const activeMobile = MOBILE_PAGES.find((p) => p.id === mobileScreen)!;
  const currentMobileIndex = MOBILE_PAGES.findIndex((p) => p.id === mobileScreen);
  const currentDesktopIndex = DESKTOP_PAGES.findIndex((p) => p.id === desktopPage);
  const visibleDesktop = catalogFilter !== "mobile";
  const visibleMobile = catalogFilter !== "desktop";

  const marqueeItems = useMemo(() => [...MOBILE_PAGES.map((p) => p.title), ...DESKTOP_PAGES.map((p) => p.title)], []);

  return (
    <div className="min-h-screen overflow-x-hidden bg-[#F4F2EB] text-[#222923] antialiased">
      {/* scroll progress */}
      <div className="fixed inset-x-0 top-0 z-[60] h-[2.5px] bg-transparent">
        <div className="h-full bg-gradient-to-r from-[#07C160] via-[#0BD566] to-[#07C160] transition-[width]" style={{ width: `${progress * 100}%` }} />
      </div>

      {/* header */}
      <header className="sticky top-0 z-50 border-b border-black/[0.06] bg-[#F4F2EB]/82 backdrop-blur-xl">
        <div className="mx-auto flex h-[66px] max-w-[1540px] items-center justify-between px-5 sm:px-8">
          <div className="flex items-center gap-3">
            <span className="relative">
              <WeChatLogo className="h-[30px] w-[30px]" />
              <span className="absolute -bottom-0.5 -right-0.5 h-2 w-2 rounded-full border-[1.5px] border-[#F4F2EB] bg-[#07C160]" />
            </span>
            <div className="h-6 w-px bg-[#D7D7CD]" />
            <div className="leading-none">
              <div className="text-[17px] font-semibold tracking-[-0.01em] text-[#1E2821]">
                微信 <span className="ml-0.5 font-serif text-[15px] font-normal italic text-[#7A847A]">unfolded</span>
              </div>
              <div className="mt-1.5 text-[6.5px] font-semibold tracking-[0.3em] text-[#9AA095]">WECHAT INTERFACE ARCHIVE</div>
            </div>
          </div>
          <nav className="hidden items-center gap-1 text-[11px] text-[#666E65] md:flex">
            {[
              ["交互预览", "showcase"],
              ["页面图鉴", "screen-index"],
              ["设计语言", "design-language"],
              ["作品说明", "about"],
            ].map(([label, target]) => (
              <ScrollLink key={target} target={target} className="rounded-full px-3.5 py-2 transition hover:bg-black/[0.04] hover:text-[#17251C]">
                {label}
              </ScrollLink>
            ))}
          </nav>
          <div className="flex items-center gap-2.5">
            <span className="hidden items-center gap-1.5 rounded-full border border-black/[0.07] bg-white/60 px-3 py-1.5 text-[8.5px] font-semibold tracking-[0.12em] text-[#7E857B] lg:flex">
              <span className="relative flex h-1.5 w-1.5">
                <span className="absolute h-full w-full animate-ping rounded-full bg-[#07C160] opacity-50" />
                <span className="relative h-1.5 w-1.5 rounded-full bg-[#07C160]" />
              </span>
              {TOTAL_SCREENS} SCREENS · LIVE
            </span>
            <ScrollLink target="screen-index" className="pressable hidden rounded-full bg-[#1F2A22] px-4 py-2 text-[10.5px] font-medium text-white shadow-[0_6px_18px_rgba(31,42,34,0.25)] transition hover:bg-[#33413a] sm:block">
              浏览全部页面 <ArrowUpRight size={11} className="ml-1 inline" />
            </ScrollLink>
            <button onClick={() => setMenuOpen((v) => !v)} className="grid h-9 w-9 place-items-center rounded-full border border-black/[0.08] text-[#4A5148] md:hidden" aria-label="菜单">
              {menuOpen ? <X size={16} /> : <Menu size={16} />}
            </button>
          </div>
        </div>
        {menuOpen && (
          <div className="border-t border-black/[0.06] bg-[#F4F2EB] px-6 py-4 md:hidden">
            <div className="grid grid-cols-2 gap-2.5 text-[12px] text-[#555D53]">
              {[["交互预览", "showcase"], ["页面图鉴", "screen-index"], ["设计语言", "design-language"], ["作品说明", "about"]].map(([label, target]) => (
                <ScrollLink key={target} target={target} className="rounded-xl border border-black/[0.06] bg-white/60 px-3.5 py-3 text-left font-medium">
                  {label}
                </ScrollLink>
              ))}
            </div>
          </div>
        )}
      </header>

      {/* hero */}
      <section className="paper-grain relative overflow-hidden px-5 pb-8 pt-10 sm:px-8 sm:pb-10 sm:pt-14">
        <div className="pointer-events-none absolute inset-0 opacity-70" style={{ backgroundImage: "radial-gradient(#D3D3C8 0.7px, transparent 0.7px)", backgroundSize: "20px 20px", maskImage: "linear-gradient(to bottom, black, transparent 80%)" }} />
        <div className="pointer-events-none absolute -right-32 -top-16 h-[460px] w-[460px] rounded-full bg-[radial-gradient(circle,#CFE4D1_0%,transparent_65%)] opacity-80" />
        <div className="pointer-events-none absolute -left-24 bottom-0 h-[320px] w-[320px] rounded-full bg-[radial-gradient(circle,#E4E7C8_0%,transparent_65%)] opacity-60" />
        <div className="relative mx-auto grid max-w-[1540px] items-center gap-10 lg:grid-cols-[1.2fr_0.8fr] lg:gap-14">
          <div className={cn("transition-all duration-700", heroLoaded ? "translate-y-0 opacity-100" : "translate-y-4 opacity-0")}>
            <div className="flex flex-wrap items-center gap-2.5 text-[8px] font-semibold tracking-[0.2em] text-[#7C8579]">
              <span className="flex items-center gap-1.5 rounded-full border border-[#C6D4C3] bg-white/60 py-1 pl-2 pr-2.5 backdrop-blur">
                <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[#07C160]" />
                非官方界面复刻 · 桌面端 × 移动端
              </span>
              <span className="rounded-full bg-[#1F2A22] px-2.5 py-1 text-[7.5px] tracking-[0.14em] text-[#B9DCC2]">DESIGN COMPETITION 2026</span>
            </div>
            <h1 className="mt-5 text-[42px] font-semibold leading-[1.12] tracking-[-0.05em] text-[#1A251D] sm:text-[56px] lg:text-[64px]">
              让每一次连接，
              <br />
              <span className="relative inline-block">
                都<span className="shimmer-text">自然发生</span>。
                <svg className="draw-underline absolute -bottom-1.5 left-0 h-[9px] w-full text-[#07C160]/60" viewBox="0 0 260 10" preserveAspectRatio="none">
                  <path d="M1 8C58 2 152 1 259 6" stroke="currentColor" strokeWidth="3.5" fill="none" strokeLinecap="round" />
                </svg>
              </span>
            </h1>
            <p className="mt-4 flex items-center gap-3 font-serif text-[16px] italic text-[#697567] sm:text-[19px]">
              <span className="h-px w-8 bg-[#B9C2B4]" /> Every connection, thoughtfully recreated.
            </p>
            <p className="text-balance mt-4 max-w-[540px] text-[11px] leading-[1.9] text-[#6B746A] sm:text-[12px]">
              一次关于微信双端体验的界面研究。从一条消息、一次分享，到一笔支付——我们把熟悉的日常拆解成
              <strong className="mx-1 rounded bg-[#E4EFE2] px-1.5 py-0.5 font-semibold text-[#2E4A34]">{TOTAL_SCREENS} 个可交互页面</strong>
             ，重新注视那些被忽略的细节与克制。
            </p>
            <div className="mt-6 flex flex-wrap items-center gap-3">
              <button onClick={() => stageRef.current?.scrollIntoView({ behavior: "smooth", block: "start" })} className="group flex items-center gap-3 rounded-full bg-[#1C2920] py-2 pl-5 pr-2 text-[11.5px] font-medium text-white shadow-[0_10px_28px_rgba(28,41,32,0.3)] transition hover:bg-[#2C3E32] hover:shadow-[0_14px_34px_rgba(28,41,32,0.35)]">
                进入交互预览
                <span className="grid h-7 w-7 place-items-center rounded-full bg-gradient-to-br from-[#0BD566] to-[#06A854] shadow-[0_2px_8px_rgba(7,193,96,0.5)] transition group-hover:rotate-[-35deg]"><ArrowDownRight size={14} /></span>
              </button>
              <ScrollLink target="screen-index" className="pressable flex items-center gap-2 rounded-full border border-black/[0.09] bg-white/70 px-4 py-2.5 text-[10.5px] font-medium text-[#586056] backdrop-blur transition hover:border-[#9DB8A2] hover:bg-white">
                查看页面图鉴 <ArrowRight size={12} />
              </ScrollLink>
              <div className="ml-1 hidden items-center gap-2 text-[8.5px] text-[#8C9389] sm:flex">
                <kbd className="rounded border border-black/[0.08] bg-white/70 px-1.5 py-0.5 text-[8px]">←</kbd>
                <kbd className="rounded border border-black/[0.08] bg-white/70 px-1.5 py-0.5 text-[8px]">→</kbd>
                切换手机页面
              </div>
            </div>
            <div className="mt-6 flex flex-wrap items-center gap-x-6 gap-y-3">
              {[
                [String(TOTAL_SCREENS), "界面收录", "SCREENS"],
                [String(MOBILE_PAGES.length), "移动端页面", "MOBILE"],
                [String(DESKTOP_PAGES.length), "桌面端页面", "DESKTOP"],
              ].map(([value, label, en], i) => (
                <div key={label} className={cn("flex items-center gap-2.5", i > 0 && "border-l border-black/[0.08] pl-6")}>
                  <span className="text-[26px] font-semibold leading-none tracking-[-0.04em] text-[#223026] tabular-nums">{value}</span>
                  <span><span className="block text-[9px] font-medium text-[#4c584d]">{label}</span><span className="mt-0.5 block text-[6.5px] tracking-[0.16em] text-[#A1A69C]">{en}</span></span>
                </div>
              ))}
            </div>
          </div>

          {/* hero visual */}
          <div className={cn("relative hidden h-[330px] transition-all delay-150 duration-1000 lg:block", heroLoaded ? "translate-y-0 opacity-100" : "translate-y-6 opacity-0")}>
            <div className="absolute right-0 top-0 w-[86%] overflow-hidden rounded-[24px] bg-gradient-to-br from-[#1E3A2A] via-[#1B3527] to-[#14261C] p-6 text-white shadow-[0_30px_70px_rgba(27,50,38,0.35)]">
              <div className="pointer-events-none absolute inset-0 opacity-[0.12]" style={{ backgroundImage: "radial-gradient(#fff 0.7px, transparent 0.7px)", backgroundSize: "15px 15px" }} />
              <div className="pointer-events-none absolute -right-16 -top-16 h-48 w-48 rounded-full bg-[#07C160]/25 blur-3xl" />
              <div className="relative z-10 flex items-start justify-between">
                <div className="flex items-center gap-2.5">
                  <WeChatLogo className="h-7 w-7" />
                  <span className="text-[7px] font-semibold leading-relaxed tracking-[0.24em] text-white/55">WECHAT<br />ARCHIVE</span>
                </div>
                <span className="rounded-full border border-white/15 bg-white/[0.06] px-2.5 py-1 font-serif text-[9px] italic text-[#9AC3A7]">est. 2011 / re:2026</span>
              </div>
              <div className="relative z-10 mt-9 flex items-end justify-between">
                <div>
                  <div className="text-[52px] font-semibold leading-none tracking-[-0.06em]">微信<span className="ml-1 text-[28px] font-light text-[#8FD0A0]">。</span></div>
                  <div className="mt-3.5 flex items-center gap-2 text-[8px] tracking-[0.26em] text-white/40">
                    <span className="h-px w-6 bg-white/25" /> EVERYDAY · CONNECTION · DESIGN
                  </div>
                </div>
                <div className="grid h-16 w-16 place-items-center rounded-2xl border border-white/15 bg-white/[0.07] backdrop-blur">
                  <div className="grid h-9 w-9 grid-cols-2 gap-1">
                    <span className="rounded-[3px] bg-white/85" /><span className="rounded-[3px] bg-white/25" /><span className="rounded-[3px] bg-white/25" /><span className="rounded-[3px] bg-white/85" />
                  </div>
                </div>
              </div>
              <div className="relative z-10 mt-7 flex items-center justify-between border-t border-white/10 pt-3.5">
                <span className="text-[8.5px] text-white/45">A study of familiar interfaces</span>
                <span className="flex items-center gap-1.5 text-[8.5px] font-medium text-[#9AC3A7]"><span className="h-1 w-1 animate-pulse rounded-full bg-[#0BD566]" /> 02 devices · live</span>
              </div>
            </div>

            <div className="float-slow absolute -bottom-2 left-0 w-[200px] rounded-2xl border border-white/70 bg-[#FBFBF6]/95 p-3 shadow-[0_18px_44px_rgba(35,49,38,0.18)] backdrop-blur" style={{ ["--fl-rot" as string]: "-4deg", transform: "rotate(-4deg)" }}>
              <div className="flex items-center gap-1.5">
                <img src="https://images.pexels.com/photos/14664508/pexels-photo-14664508.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=60&h=60" alt="" className="h-5 w-5 rounded-[5px] object-cover" />
                <span className="text-[8px] font-semibold text-[#455246]">林小满</span>
                <span className="ml-auto rounded bg-[#E9F8EF] px-1 py-0.5 text-[6px] font-medium text-[#07A858]">在线</span>
              </div>
              <div className="mt-2 rounded-[5px] rounded-tl-[2px] bg-white px-2 py-1.5 text-[8.5px] leading-4 text-[#5A6359] shadow-sm ring-1 ring-black/[0.03]">这周末的展，一起去吧 🌿</div>
              <div className="ml-auto mt-1.5 w-fit rounded-[5px] rounded-tr-[2px] bg-[#95EC69] px-2 py-1.5 text-[8.5px] text-[#31402F] shadow-sm">好呀，美术馆见！</div>
            </div>

            <div className="float-slow absolute bottom-6 right-4 z-10 h-[124px] w-[94px] overflow-hidden rounded-[12px] border-[3px] border-[#FAFAF4] bg-[#DDE3D9] shadow-[0_16px_36px_rgba(30,45,33,0.25)]" style={{ ["--fl-rot" as string]: "5deg", transform: "rotate(5deg)", animationDelay: "1.2s" }}>
              <img src="https://images.pexels.com/photos/35983785/pexels-photo-35983785.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=240&h=300" alt="春日花束" className="h-full w-full object-cover" />
              <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/50 to-transparent px-2 pb-1.5 pt-5 text-[6.5px] font-medium text-white">April, 2026 · 朋友圈</div>
            </div>
            <div className="absolute right-[120px] top-[52%] z-20 flex items-center gap-1.5 rounded-full border border-black/[0.06] bg-[#F8F8F2]/95 px-3 py-1.5 text-[7.5px] font-semibold tracking-[0.08em] text-[#5F6B5F] shadow-lg backdrop-blur">
              <span className="h-1.5 w-1.5 rounded-full bg-[#07C160]" /> 克制 · 清晰 · 有温度
            </div>
          </div>
        </div>
      </section>

      {/* marquee */}
      <div className="overflow-hidden border-y border-black/[0.06] bg-[#1E2A22] py-2.5">
        <div className="animate-marquee flex w-max items-center gap-6 whitespace-nowrap">
          {[...marqueeItems, ...marqueeItems].map((t, i) => (
            <span key={i} className="flex items-center gap-6 text-[10px] tracking-[0.2em] text-white/55">
              {t} <span className="h-1 w-1 rounded-full bg-[#07C160]" />
            </span>
          ))}
        </div>
      </div>

      {/* live stage */}
      <section id="showcase" ref={stageRef} className="scroll-mt-[80px] px-3 pb-6 pt-6 sm:px-6 lg:px-8">
        <div className="reveal mx-auto max-w-[1540px] overflow-hidden rounded-[24px] border border-black/[0.07] bg-[#EAE9E1] shadow-[0_20px_60px_rgba(43,53,44,0.1)]">
          <div className="flex flex-wrap items-center justify-between gap-3 border-b border-black/[0.06] bg-[#F1F0EA]/90 px-4 py-3 backdrop-blur sm:px-5">
            <div className="flex items-center gap-3">
              <span className="relative grid h-8 w-8 place-items-center rounded-full bg-gradient-to-br from-[#2A3A2E] to-[#1A251D] text-[#A5D8B3] shadow-md">
                <Sparkles size={14} />
                <span className="absolute -right-0.5 -top-0.5 h-2.5 w-2.5 rounded-full border-2 border-[#F1F0EA] bg-[#07C160]" />
              </span>
              <div>
                <div className="flex items-center gap-2 text-[11px] font-semibold text-[#2E3A32]">
                  实时双端预览
                  <span className="flex items-center gap-1 rounded-full bg-[#07C160]/10 px-2 py-0.5 text-[7px] font-bold tracking-[0.1em] text-[#078A47]">
                    <span className="h-1 w-1 animate-pulse rounded-full bg-[#07C160]" /> LIVE
                  </span>
                </div>
                <div className="mt-0.5 text-[7.5px] tracking-[0.16em] text-[#999F94]">CLICK ANYTHING · 全站可交互</div>
              </div>
            </div>
            <div className="order-3 flex w-full flex-wrap items-center justify-between gap-2 sm:order-none sm:w-auto sm:justify-end">
              <div className="flex items-center divide-x divide-black/[0.07] rounded-full border border-black/[0.07] bg-white/80 py-1 pl-2.5 pr-1 shadow-sm backdrop-blur">
                <PagePicker label="Desktop" value={desktopPage} options={DESKTOP_PAGES} onChange={(v) => revealDesktop(v as DesktopPage)} />
                <div className="ml-2 flex items-center pl-2"><PagePicker label="Mobile" accent value={mobileScreen} options={MOBILE_PAGES} onChange={(v) => revealMobile(v as MobileScreen)} /></div>
              </div>
              <DeviceToggle value={device} onChange={setDevice} />
            </div>
          </div>

          <div className="stage-canvas paper-grain relative overflow-x-auto overflow-y-hidden p-4 sm:p-7 lg:px-8 lg:py-9">
            <div className="pointer-events-none absolute inset-0 opacity-[0.3]" style={{ backgroundImage: "radial-gradient(#9CA79A 0.7px, transparent 0.7px)", backgroundSize: "22px 22px" }} />
            <div className="pointer-events-none absolute -left-20 top-10 h-[380px] w-[380px] rounded-full bg-[radial-gradient(circle,#BFD9C3_0%,transparent_68%)] opacity-60" />
            <div className="pointer-events-none absolute -bottom-20 -right-12 h-[380px] w-[380px] rounded-full bg-[radial-gradient(circle,#D8E2BE_0%,transparent_66%)] opacity-50" />
            <div className="pointer-events-none absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 select-none whitespace-nowrap font-serif text-[150px] italic leading-none text-[#213024]/[0.03]">wechat everyday</div>

            <div className={cn("relative z-10 mx-auto flex w-full items-start justify-center", device === "both" && "flex-col gap-10 xl:flex-row xl:gap-8")}>
              <div className={cn("min-w-0", device === "mobile" ? "hidden" : device === "both" ? "hidden md:block md:w-full xl:flex-1" : "block w-full")}>
                <div className="mb-3.5 flex items-center justify-between gap-4">
                  <div className="flex items-center gap-2">
                    <span className="flex h-[22px] items-center gap-1.5 rounded-full border border-black/[0.07] bg-white/80 px-2.5 text-[7.5px] font-bold tracking-[0.14em] text-[#5d665c] shadow-sm backdrop-blur"><Monitor size={10} /> DESKTOP</span>
                    <span className="text-[8.5px] tabular-nums text-[#8F968B]">WeChat 4.0.6 · 可交互</span>
                  </div>
                  <div className="flex items-center gap-2 rounded-full border border-black/[0.06] bg-white/70 px-3 py-1 text-[8.5px] text-[#8D9489] backdrop-blur">
                    <span className="font-semibold text-[#3d4a3e]">{activeDesktop.title}</span>
                    <span className="text-[#B5BAB0]">/</span>
                    <span className="font-serif italic">{activeDesktop.english}</span>
                    <span className="tabular-nums text-[#B5BAB0]">{String(currentDesktopIndex + 1).padStart(2, "0")}/{String(DESKTOP_PAGES.length).padStart(2, "0")}</span>
                  </div>
                </div>
                <DesktopWeChat page={desktopPage} onPageChange={setDesktopPage} onNotify={notify} />
                <div className="mt-3 flex items-center justify-between text-[7.5px] text-[#959C92]">
                  <span className="flex items-center gap-1.5"><kbd className="rounded border border-black/[0.08] bg-white/70 px-1">⌘K</kbd> 全局搜索 · 点击左侧导航切换页面</span>
                  <span className="hidden sm:block">© WeChat interface study · 非官方</span>
                </div>
              </div>

              <div className={cn("flex flex-col items-center", device === "desktop" && "hidden", device === "both" && "mx-auto xl:mx-0")}>
                <div className="mb-3.5 flex w-full max-w-[340px] items-center justify-between">
                  <div className="flex items-center gap-1.5 rounded-full border border-black/[0.07] bg-white/80 px-2.5 py-[5px] text-[7.5px] font-bold tracking-[0.14em] text-[#5d665c] shadow-sm backdrop-blur"><Smartphone size={10} /> MOBILE</div>
                  <div className="flex items-center gap-1.5 rounded-full border border-black/[0.06] bg-white/70 px-2.5 py-1 text-[8px] text-[#8D9489] backdrop-blur">
                    <span className="font-semibold text-[#3d4a3e]">{activeMobile.title}</span>
                    <span className="text-[#B5BAB0]">/</span>
                    <span className="font-serif italic">{activeMobile.english}</span>
                  </div>
                </div>
                <PhoneWeChat screen={mobileScreen} onScreenChange={setMobileScreen} onNotify={notify} />
              </div>
            </div>

            {/* control bar */}
            <div className="relative z-10 mx-auto mt-7 flex max-w-[1060px] flex-wrap items-center justify-between gap-3 rounded-2xl border border-black/[0.07] bg-white/75 px-4 py-3 shadow-[0_10px_30px_rgba(40,52,40,0.08)] backdrop-blur-xl">
              <div className="flex items-center gap-2.5">
                <button
                  onClick={() => setTouring(!touring)}
                  className={cn(
                    "pressable flex items-center gap-1.5 rounded-full px-3 py-1.5 text-[9px] font-semibold transition",
                    touring ? "bg-[#FA5151] text-white shadow-[0_4px_12px_rgba(250,81,81,0.35)]" : "bg-[#1F2A22] text-white shadow-[0_4px_12px_rgba(31,42,34,0.25)]",
                  )}
                >
                  {touring ? <Pause size={10} /> : <Play size={10} />} {touring ? "停止巡览" : "自动巡览"}
                </button>
                <span className="hidden text-[7.5px] tracking-[0.14em] text-[#9aa39a] sm:block">AUTO TOUR · 2.8S</span>
              </div>
              <div className="flex items-center gap-2">
                <button
                  onClick={() => { const n = MOBILE_PAGES[(currentMobileIndex - 1 + MOBILE_PAGES.length) % MOBILE_PAGES.length]; setMobileScreen(n.id); }}
                  className="pressable grid h-8 w-8 place-items-center rounded-full border border-black/[0.08] bg-white text-[#5d665c] shadow-sm transition hover:bg-[#F2F5F0]"
                  aria-label="上一个移动端页面"
                >
                  <ArrowLeft size={13} />
                </button>
                <div className="min-w-[190px] px-1 text-center">
                  <div className="text-[9.5px] font-semibold tabular-nums text-[#3d4a3e]">{String(currentMobileIndex + 1).padStart(2, "0")} / {String(MOBILE_PAGES.length).padStart(2, "0")} · {activeMobile.title}</div>
                  <div className="mx-auto mt-1.5 h-[3px] w-full max-w-[190px] overflow-hidden rounded-full bg-black/[0.07]">
                    <div className="h-full rounded-full bg-gradient-to-r from-[#07C160] to-[#0BD566] transition-all duration-300" style={{ width: `${((currentMobileIndex + 1) / MOBILE_PAGES.length) * 100}%` }} />
                  </div>
                </div>
                <button
                  onClick={() => { const n = MOBILE_PAGES[(currentMobileIndex + 1) % MOBILE_PAGES.length]; setMobileScreen(n.id); }}
                  className="pressable grid h-8 w-8 place-items-center rounded-full border border-black/[0.08] bg-white text-[#5d665c] shadow-sm transition hover:bg-[#F2F5F0]"
                  aria-label="下一个移动端页面"
                >
                  <ArrowRight size={13} />
                </button>
              </div>
              <div className="flex items-center gap-2.5">
                <div className="text-right">
                  <div className="text-[9px] font-semibold tabular-nums text-[#3d4a3e]">桌面 {String(currentDesktopIndex + 1).padStart(2, "0")}/{String(DESKTOP_PAGES.length).padStart(2, "0")}</div>
                  <div className="mt-1 h-[3px] w-[76px] overflow-hidden rounded-full bg-black/[0.07]">
                    <div className="h-full rounded-full bg-[#1F2A22] transition-all duration-300" style={{ width: `${((currentDesktopIndex + 1) / DESKTOP_PAGES.length) * 100}%` }} />
                  </div>
                </div>
                <button onClick={() => document.getElementById("screen-index")?.scrollIntoView({ behavior: "smooth" })} className="pressable hidden items-center gap-1 rounded-full bg-[#E8EDE4] px-3 py-1.5 text-[9px] font-medium text-[#4a5a4b] sm:flex">全部页面 <ArrowDownRight size={10} /></button>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* screen index */}
      <section id="screen-index" className="scroll-mt-[78px] px-5 pb-16 pt-12 sm:px-8 sm:pt-16">
        <div className="mx-auto max-w-[1540px]">
          <div className="reveal flex flex-col justify-between gap-6 border-b border-black/[0.07] pb-7 md:flex-row md:items-end">
            <div>
              <div className="flex items-center gap-2 text-[8px] font-bold tracking-[0.22em] text-[#8C9388]">
                <span className="h-1.5 w-1.5 rounded-full bg-[#07C160]" /> THE SCREEN INDEX / 页面图鉴
              </div>
              <h2 className="mt-4 text-[32px] font-semibold leading-[1.22] tracking-[-0.04em] text-[#1d2921] sm:text-[42px]">
                不只是几个界面，<br /><span className="font-serif font-normal italic text-[#4E6B52]">而是一整套体验。</span>
              </h2>
            </div>
            <div className="flex flex-col items-start gap-4 md:items-end">
              <p className="max-w-[380px] text-[10.5px] leading-[1.9] text-[#7A8379] md:text-right">
                从会话、通讯录、朋友圈，到支付、卡包与视频通话。点击任意卡片，即可在上方预览中打开对应界面。
              </p>
              <div className="flex items-center gap-1 rounded-full border border-black/[0.07] bg-white/70 p-1 shadow-sm backdrop-blur">
                {[
                  { id: "all" as const, label: `全部 ${TOTAL_SCREENS}` },
                  { id: "desktop" as const, label: `桌面端 ${DESKTOP_PAGES.length}` },
                  { id: "mobile" as const, label: `移动端 ${MOBILE_PAGES.length}` },
                ].map((f) => (
                  <button key={f.id} onClick={() => setCatalogFilter(f.id)} className={cn("rounded-full px-3.5 py-1.5 text-[9.5px] font-medium transition", catalogFilter === f.id ? "bg-[#223027] text-white shadow-[0_4px_12px_rgba(34,48,39,0.3)]" : "text-[#858C81] hover:text-[#3F473D]")}>{f.label}</button>
                ))}
              </div>
            </div>
          </div>

          <div className="mt-7 space-y-10">
            {visibleDesktop && (
              <div className="reveal">
                <div className="mb-4 flex items-center justify-between">
                  <div className="flex items-center gap-2.5">
                    <span className="grid h-7 w-7 place-items-center rounded-full bg-[#223027] text-[#B9DCC2] shadow-md"><Monitor size={13} /></span>
                    <h3 className="text-[13px] font-semibold text-[#39453B]">桌面端 <span className="ml-1 font-serif text-[11px] font-normal italic text-[#9AA296]">Desktop experience</span></h3>
                    <span className="rounded-full bg-[#223027]/8 px-2 py-0.5 text-[7.5px] font-bold tracking-[0.08em] text-[#5d6d5e]">{DESKTOP_PAGES.length} SCREENS</span>
                  </div>
                </div>
                <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
                  {DESKTOP_PAGES.map((page, i) => {
                    const active = desktopPage === page.id;
                    return (
                      <button
                        key={page.id}
                        onClick={() => openDesktop(page.id as DesktopPage)}
                        className={cn(
                          "card-lift group relative overflow-hidden rounded-2xl border bg-[#FAFAF5] p-3 text-left",
                          active ? "border-[#07C160] shadow-[0_0_0_3px_rgba(7,193,96,0.12),0_16px_40px_rgba(7,193,96,0.12)]" : "border-black/[0.06]",
                        )}
                      >
                        <div className="flex items-center justify-between px-0.5 pb-2.5">
                          <span className="text-[7.5px] font-bold tracking-[0.16em] text-[#A0A69B] tabular-nums">{String(i + 1).padStart(2, "0")} / DESKTOP</span>
                          {active ? (
                            <span className="grid h-4 w-4 place-items-center rounded-full bg-[#07C160] text-white"><Check size={9} strokeWidth={3.5} /></span>
                          ) : (
                            <ArrowUpRight size={12} className="text-[#B0B6AC] transition group-hover:translate-x-0.5 group-hover:-translate-y-0.5 group-hover:text-[#07C160]" />
                          )}
                        </div>
                        <DesktopScreenArt type={page.id} />
                        <div className="px-0.5 pt-3">
                          <div className="flex items-baseline justify-between gap-1"><span className="text-[12px] font-semibold text-[#313D33]">{page.title}</span><span className="font-serif text-[8.5px] italic text-[#9DA497]">{page.english}</span></div>
                          <div className="mt-1 line-clamp-1 text-[8.5px] text-[#8B9287]">{page.caption}</div>
                        </div>
                      </button>
                    );
                  })}
                </div>
              </div>
            )}

            {visibleMobile && (
              <div className="reveal">
                <div className="mb-4 flex items-center justify-between">
                  <div className="flex items-center gap-2.5">
                    <span className="grid h-7 w-7 place-items-center rounded-full bg-[#07C160] text-white shadow-[0_4px_12px_rgba(7,193,96,0.35)]"><Smartphone size={13} /></span>
                    <h3 className="text-[13px] font-semibold text-[#39453B]">移动端 <span className="ml-1 font-serif text-[11px] font-normal italic text-[#9AA296]">Mobile experience</span></h3>
                    <span className="rounded-full bg-[#07C160]/10 px-2 py-0.5 text-[7.5px] font-bold tracking-[0.08em] text-[#078A47]">{MOBILE_PAGES.length} SCREENS</span>
                  </div>
                </div>
                <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 xl:grid-cols-7">
                  {MOBILE_PAGES.map((page, i) => {
                    const active = mobileScreen === page.id;
                    const dark = ["channels", "call", "redpacket", "scan"].includes(page.id);
                    return (
                      <button
                        key={page.id}
                        onClick={() => openMobile(page.id)}
                        className={cn(
                          "card-lift group relative overflow-hidden rounded-2xl border bg-[#FAFAF5] p-3 text-left",
                          active ? "border-[#07C160] shadow-[0_0_0_3px_rgba(7,193,96,0.12),0_16px_40px_rgba(7,193,96,0.12)]" : "border-black/[0.06]",
                        )}
                      >
                        <div className="flex items-center justify-between px-0.5 pb-2.5">
                          <span className="text-[7.5px] font-bold tracking-[0.16em] text-[#A0A69B] tabular-nums">{String(i + 1).padStart(2, "0")} / MOBILE</span>
                          {active ? (
                            <span className="grid h-4 w-4 place-items-center rounded-full bg-[#07C160] text-white"><Check size={9} strokeWidth={3.5} /></span>
                          ) : (
                            <ArrowUpRight size={12} className="text-[#B0B6AC] transition group-hover:translate-x-0.5 group-hover:-translate-y-0.5 group-hover:text-[#07C160]" />
                          )}
                        </div>
                        <div className={cn("flex justify-center rounded-xl pb-0 pt-3", dark ? "bg-gradient-to-b from-[#232B24] to-[#171C18]" : "bg-gradient-to-b from-[#EDECE4] to-[#E3E2D8]")}>
                          <MiniScreenArt type={page.id} tone={dark ? "dark" : "light"} />
                        </div>
                        <div className="px-0.5 pt-3">
                          <div className="flex items-baseline justify-between gap-1"><span className="text-[12px] font-semibold text-[#313D33]">{page.title}</span><span className="truncate font-serif text-[7.5px] italic text-[#9DA497]">{page.english}</span></div>
                          <div className="mt-1 line-clamp-1 text-[8.5px] text-[#8B9287]">{page.caption}</div>
                        </div>
                        <span className={cn("absolute right-2.5 top-[38px] rounded-full px-1.5 py-0.5 text-[6.5px] font-bold tracking-[0.06em]", dark ? "bg-white/15 text-white/80 backdrop-blur" : "bg-black/[0.05] text-[#7c847c]")}>{page.group}</span>
                      </button>
                    );
                  })}
                </div>
              </div>
            )}
          </div>
        </div>
      </section>

      {/* design language */}
      <section id="design-language" className="scroll-mt-[78px] border-y border-black/[0.06] bg-[#ECEBE3] px-5 py-14 sm:px-8 sm:py-20">
        <div className="mx-auto max-w-[1540px]">
          <div className="grid gap-10 lg:grid-cols-[0.72fr_1.28fr] lg:gap-16">
            <div className="reveal">
              <div className="flex items-center gap-2 text-[8px] font-bold tracking-[0.22em] text-[#8C9388]"><span className="h-1.5 w-1.5 rounded-full bg-[#07C160]" /> DESIGN LANGUAGE / 设计语言</div>
              <h2 className="mt-4 text-[32px] font-semibold leading-[1.28] tracking-[-0.04em] text-[#1d2921] sm:text-[40px]">
                克制，是为了<br /><span className="font-serif font-normal italic text-[#4E6B52]">让内容被看见。</span>
              </h2>
              <p className="mt-4 max-w-[360px] text-[11px] leading-[1.95] text-[#798278]">
                微信的视觉从来不喧哗。大量留白、克制的灰阶与唯一的绿色强调，让界面退后一步，把每一次沟通推到前面。点击色卡即可复制色值。
              </p>
              <div className="mt-7 space-y-0">
                {[
                  ["01", "熟悉感优先", "不为了不同而不同，让每个入口都在直觉的位置。"],
                  ["02", "单一绿色强调", "绿色只属于「可行动」的元素，状态与动作一目了然。"],
                  ["03", "有温度的留白", "不追求填满，给文字、图片与关系留出呼吸的余地。"],
                  ["04", "双端一致体验", "桌面与移动共享同一套语言，只是换一种姿态呈现。"],
                ].map(([n, title, desc]) => (
                  <div key={n} className="group flex gap-4 border-t border-black/[0.07] py-3.5 transition hover:pl-1">
                    <span className="font-serif text-[12px] italic text-[#07A858]">{n}</span>
                    <div><div className="text-[11px] font-semibold text-[#3D4B40]">{title}</div><div className="mt-1 text-[9.5px] leading-5 text-[#879085]">{desc}</div></div>
                  </div>
                ))}
              </div>
            </div>

            <div className="reveal reveal-d1 grid gap-3 sm:grid-cols-2">
              <div className="rounded-2xl border border-black/[0.06] bg-[#F9F9F4] p-5 shadow-[0_4px_20px_rgba(45,56,46,0.05)] sm:col-span-2">
                <div className="flex items-center justify-between">
                  <span className="text-[9px] font-bold tracking-[0.14em] text-[#737E70]">COLOR SYSTEM · 点击复制</span>
                  <span className="text-[8px] tabular-nums text-[#A1A79C]">8 tokens</span>
                </div>
                <div className="mt-4 grid grid-cols-4 gap-2.5 sm:grid-cols-8">
                  {[
                    ["#07C160", "WeChat", "品牌动作"],
                    ["#95EC69", "Bubble", "我的消息"],
                    ["#1A1A1A", "Ink", "主要文字"],
                    ["#9A9A9A", "Muted", "次要文字"],
                    ["#EDEDED", "Mist", "页面底色"],
                    ["#FA5151", "Alert", "提醒未读"],
                    ["#576B95", "Link", "链接文字"],
                    ["#F5B640", "Gold", "星标强调"],
                  ].map(([hex, name, desc]) => (
                    <button key={hex} onClick={() => copyHex(hex)} className="group text-left">
                      <div className="relative h-[54px] overflow-hidden rounded-[10px] border border-black/[0.06] shadow-inner transition group-hover:scale-[1.04] group-hover:shadow-md" style={{ background: hex }}>
                        <span className="absolute inset-0 grid place-items-center bg-black/0 opacity-0 transition group-hover:bg-black/15 group-hover:opacity-100">
                          {copied === hex ? <Check size={14} className="text-white" strokeWidth={3} /> : <Copy size={13} className="text-white" />}
                        </span>
                      </div>
                      <div className="mt-2 text-[8.5px] font-semibold text-[#49544A]">{name}</div>
                      <div className="mt-0.5 text-[7.5px] tabular-nums tracking-wider text-[#9AA196]">{hex}</div>
                      <div className="mt-0.5 text-[7px] text-[#B0B6AB]">{desc}</div>
                    </button>
                  ))}
                </div>
              </div>

              <div className="rounded-2xl border border-black/[0.06] bg-[#F9F9F4] p-5">
                <div className="flex items-center justify-between"><span className="text-[9px] font-bold tracking-[0.14em] text-[#737E70]">TYPOGRAPHY</span><span className="font-serif text-[10px] italic text-[#A0A69B]">PingFang · Inter</span></div>
                <div className="mt-4 flex items-end gap-3">
                  <span className="text-[44px] font-semibold leading-none tracking-[-0.05em] text-[#253127]">Aa</span>
                  <span className="pb-1 text-[26px] font-medium text-[#6A7569]">微信</span>
                  <span className="ml-auto pb-1.5 font-serif text-[13px] italic text-[#9aa39a]">unfolded</span>
                </div>
                <div className="mt-3 space-y-2 border-t border-black/[0.06] pt-3">
                  {[
                    ["导航标题", "17 · Semibold", "text-[14px] font-semibold"],
                    ["正文内容", "14 · Regular", "text-[11.5px]"],
                    ["辅助说明", "12 · Regular", "text-[9.5px] text-[#8a918a]"],
                    ["微小标注", "10 · Medium", "text-[8px] tracking-wide text-[#9aa39a]"],
                  ].map(([label, spec, cls]) => (
                    <div key={label} className="flex items-center justify-between"><span className={cn("text-[#3d4a3e]", cls)}>{label} · 让每一次连接都自然发生</span><span className="ml-2 shrink-0 text-[7px] tabular-nums tracking-wider text-[#A5ABA0]">{spec}</span></div>
                  ))}
                </div>
              </div>

              <div className="rounded-2xl border border-black/[0.06] bg-[#F9F9F4] p-5">
                <div className="flex items-center justify-between"><span className="text-[9px] font-bold tracking-[0.14em] text-[#737E70]">COMPONENTS · LIVE</span><span className="flex items-center gap-1 text-[8px] text-[#07A858]"><span className="h-1 w-1 animate-pulse rounded-full bg-[#07C160]" /> 可交互</span></div>
                <div className="mt-4 space-y-3.5">
                  <div className="flex items-center gap-2">
                    <div className="relative max-w-[150px] rounded-[6px] rounded-tl-[2px] bg-white px-2.5 py-2 text-[9.5px] text-[#444] shadow-[0_2px_6px_rgba(0,0,0,0.06)]">下午三点，美术馆见 🌿</div>
                    <div className="rounded-[6px] rounded-tr-[2px] bg-[#95EC69] px-2.5 py-2 text-[9.5px] text-[#2E3A2B] shadow-[0_2px_6px_rgba(110,160,60,0.2)]">好呀</div>
                    <span className="grid h-[18px] min-w-[18px] place-items-center rounded-full bg-[#FA5151] px-1 text-[9px] font-bold text-white shadow">3</span>
                  </div>
                  <div className="flex items-center justify-between"><span className="text-[9px] text-[#717C6E]">主按钮 / Primary</span><span className="cursor-pointer rounded-full bg-gradient-to-b from-[#0BD566] to-[#06B058] px-4 py-1.5 text-[8.5px] font-medium text-white shadow-[0_4px_12px_rgba(7,193,96,0.35)] transition hover:shadow-[0_6px_16px_rgba(7,193,96,0.45)]">发送</span></div>
                  <div className="flex items-center justify-between border-t border-black/[0.06] pt-3"><span className="text-[9px] text-[#717C6E]">消息免打扰</span><DemoSwitch /></div>
                  <div className="flex items-center justify-between border-t border-black/[0.06] pt-3">
                    <span className="text-[9px] text-[#717C6E]">圆角 / Radius</span>
                    <div className="flex items-end gap-1.5">
                      {[["4px", "rounded-[4px]"], ["8px", "rounded-[8px]"], ["12px", "rounded-[12px]"], ["full", "rounded-full"]].map(([l, c]) => (
                        <span key={l} className="flex flex-col items-center gap-1"><span className={cn("h-5 w-5 bg-[#223027]", c)} /><span className="text-[6.5px] tabular-nums text-[#9aa39a]">{l}</span></span>
                      ))}
                    </div>
                  </div>
                </div>
              </div>

              <div className="relative overflow-hidden rounded-2xl bg-gradient-to-br from-[#22392C] via-[#1B2E23] to-[#141F18] p-6 text-white sm:col-span-2">
                <div className="pointer-events-none absolute -right-10 -top-16 h-52 w-52 rounded-full bg-[#07C160]/20 blur-3xl" />
                <div className="relative flex flex-wrap items-center justify-between gap-5">
                  <div>
                    <div className="text-[8px] font-bold tracking-[0.22em] text-[#8FB39A]">MICRO DETAIL · 像素级还原</div>
                    <div className="mt-2.5 font-serif text-[21px] italic leading-snug text-[#F3F1E7]">“好的界面，应该在需要时出现，<br />在不需要时退后。”</div>
                  </div>
                  <div className="flex items-center gap-6 border-l border-white/10 pl-6">
                    {[
                      ["0.5px", "iOS 分割线"],
                      ["5px", "消息圆角"],
                      ["62px", "桌面导航"],
                      ["37", "界面总数"],
                    ].map(([value, label]) => (
                      <div key={label}><div className="text-[20px] font-semibold tracking-tight text-[#CFE3D3] tabular-nums">{value}</div><div className="mt-1 text-[7px] tracking-[0.12em] text-white/40">{label}</div></div>
                    ))}
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      </section>

      {/* about */}
      <section id="about" className="scroll-mt-[78px] px-5 py-14 sm:px-8 sm:py-20">
        <div className="mx-auto max-w-[1540px]">
          <div className="reveal grid gap-8 rounded-[22px] border border-black/[0.06] bg-[#F9F9F4] p-6 shadow-[0_16px_50px_rgba(40,52,40,0.07)] sm:p-9 lg:grid-cols-[1.05fr_0.95fr] lg:p-12">
            <div>
              <div className="flex items-center gap-2 text-[8px] font-bold tracking-[0.22em] text-[#8C9388]"><span className="h-1.5 w-1.5 rounded-full bg-[#07C160]" /> ABOUT THE STUDY / 作品说明</div>
              <h2 className="mt-4 text-[28px] font-semibold leading-[1.38] tracking-[-0.035em] text-[#223027] sm:text-[34px]">
                一次重新注视日常的<br /><span className="font-serif font-normal italic text-[#4E6B52]">界面练习。</span>
              </h2>
              <p className="mt-4 max-w-[520px] text-[11px] leading-[2] text-[#727D71]">
                本作品以微信桌面端与移动端为研究对象，围绕「熟悉感」与「克制」两个关键词，完成 {TOTAL_SCREENS} 个界面的高保真复刻与交互补全：从聊天、朋友圈、视频号，到支付、卡包、扫码登录与视频通话。所有内容均为虚构演示数据，不涉及真实账号、聊天记录或支付信息。
              </p>
              <div className="mt-5 flex items-center gap-4 border-t border-black/[0.06] pt-4">
                {[
                  ["2011", "微信诞生"],
                  ["2016", "小程序"],
                  ["2020", "视频号"],
                  ["2026", "本次复刻"],
                ].map(([y, e], i) => (
                  <div key={y} className="flex items-center gap-4">
                    <div><div className={cn("text-[13px] font-bold tabular-nums", i === 3 ? "text-[#07A858]" : "text-[#3d4a3e]")}>{y}</div><div className="mt-0.5 text-[7.5px] text-[#9aa39a]">{e}</div></div>
                    {i < 3 && <span className="h-px w-5 bg-black/10 sm:w-8" />}
                  </div>
                ))}
              </div>
              <div className="mt-5 flex flex-wrap gap-2">
                {["React", "Vite", "Tailwind CSS", "Lucide Icons", `${TOTAL_SCREENS} screens`, "Fully Interactive", "Responsive"].map((tag) => (
                  <span key={tag} className="rounded-full border border-black/[0.07] bg-white/70 px-2.5 py-1 text-[8px] font-medium text-[#788375]">{tag}</span>
                ))}
              </div>
            </div>
            <div className="grid grid-cols-2 gap-2.5">
              {[
                [String(TOTAL_SCREENS), "界面复刻", "从主界面到细节流程", "SCREENS"],
                ["02", "平台联动", "桌面端 × 移动端", "PLATFORMS"],
                ["120+", "可交互元素", "导航、输入、支付与通话", "INTERACTIONS"],
                ["100%", "原创实现", "代码构建全部界面", "HANDMADE"],
              ].map(([value, label, desc, en]) => (
                <div key={label} className="card-lift rounded-2xl border border-black/[0.06] bg-white p-4">
                  <div className="flex items-start justify-between"><span className="text-[27px] font-semibold leading-none tracking-[-0.04em] text-[#29372C] tabular-nums">{value}</span><span className="text-[6px] font-bold tracking-[0.14em] text-[#A6ACA1]">{en}</span></div>
                  <div className="mt-3 text-[10px] font-semibold text-[#4C584D]">{label}</div><div className="mt-1 text-[8.5px] leading-4 text-[#959D92]">{desc}</div>
                </div>
              ))}
            </div>
          </div>

          <div className="mt-10 flex flex-col justify-between gap-5 border-t border-black/[0.07] pt-6 sm:flex-row sm:items-end">
            <div className="flex items-center gap-3">
              <WeChatLogo className="h-9 w-9" />
              <div>
                <div className="text-[15px] font-semibold tracking-[-0.01em] text-[#2B362D]">微信 <span className="font-serif font-normal italic text-[#788576]">unfolded</span></div>
                <div className="mt-1 text-[7.5px] font-medium tracking-[0.18em] text-[#A0A69B]">A NON-OFFICIAL INTERFACE STUDY · 2026</div>
              </div>
            </div>
            <div className="flex flex-wrap items-center gap-x-5 gap-y-2 text-[8.5px] text-[#969D92]">
              <span className="flex items-center gap-1.5"><span className="h-1.5 w-1.5 rounded-full bg-[#07C160]" /> 非官方概念设计 · 仅用于学习交流</span>
              <ScrollLink target="showcase" className="pressable rounded-full border border-black/[0.08] bg-white/60 px-3 py-1.5 font-medium text-[#5F6F60] hover:bg-white">回到预览 ↑</ScrollLink>
            </div>
          </div>
        </div>
      </section>

      {/* toasts */}
      <div className="pointer-events-none fixed inset-x-0 bottom-7 z-[100] flex flex-col items-center gap-2">
        {toasts.map((t) => (
          <div key={t.id} className="toast-in flex max-w-[calc(100vw-32px)] items-center gap-2.5 rounded-full border border-white/10 bg-[#1B2620]/96 py-2.5 pl-3 pr-4 text-[10.5px] text-[#E9F2EA] shadow-[0_16px_50px_rgba(15,25,18,0.35)] backdrop-blur-xl">
            <span className="grid h-5 w-5 shrink-0 place-items-center rounded-full bg-gradient-to-br from-[#0BD566] to-[#06A854] shadow-[0_2px_8px_rgba(7,193,96,0.5)]"><Check size={11} strokeWidth={3} /></span>
            <span className="line-clamp-1">{t.text}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function DemoSwitch() {
  const [on, setOn] = useState(true);
  return (
    <button
      onClick={() => setOn(!on)}
      className={cn("relative h-[20px] w-[34px] rounded-full transition-all duration-300", on ? "bg-[#07C160]" : "bg-[#E2E2E2]")}
    >
      <span className={cn("absolute top-[2px] h-[16px] w-[16px] rounded-full bg-white shadow transition-all duration-300", on ? "translate-x-[16px]" : "translate-x-[2px]")} />
    </button>
  );
}
