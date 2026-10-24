import { useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  ArrowLeftRight,
  Camera,
  ChevronRight,
  CirclePlus,
  FileText,
  Gift,
  Heart,
  Image as ImageIcon,
  MapPin,
  MessageCircle,
  Mic,
  MoreHorizontal,
  Music,
  Plus,
  QrCode,
  Search,
  Send,
  Share2,
  Smile,
  Sparkles,
  Star,
  Video,
  Volume2,
  VolumeX,
  X,
  Zap,
  ZoomIn,
} from "lucide-react";
import { assets, chats, contacts, conversations, me, miniApps, moments, px, type Chat, type Message } from "../../data";
import { Avatar, Switch, SystemPill, Waveform, stickerSets } from "../shared";
import { cn } from "../../utils/cn";
import { MobileTop, PhoneBack } from "./atoms";
import type { MobileScreen } from "./types";

type BaseProps = {
  onNavigate: (screen: MobileScreen) => void;
  onNotify: (message: string) => void;
};

// Keep demo conversations alive while moving into the call / red packet overlays.
let mobileMessageCache: Record<string, Message[]> = { ...conversations };
function updateMobileMessageCache(next: Record<string, Message[]>) {
  mobileMessageCache = next;
  return next;
}

function resolveChat(chatId: string): Chat {
  const found = chats.find((c) => c.id === chatId);
  if (found) return found;
  const person = contacts.find((c) => c.id === chatId) || contacts[0];
  return { id: person.id, name: person.name, avatar: person.avatar, type: "direct", last: "", time: "", online: person.online };
}

function MobileMessage({ message }: { message: Message }) {
  const mine = message.side === "me";
  if (message.kind === "sticker") return <div className="pop-in px-0.5 py-1 text-[40px] leading-none drop-shadow-sm">{message.text}</div>;
  if (message.kind === "image") {
    return (
      <span className="relative block overflow-hidden rounded-[6px] shadow-[0_2px_8px_rgba(0,0,0,0.1)]">
        <img src={message.image} alt="聊天图片" className="max-h-[160px] max-w-[180px] object-cover" loading="lazy" />
        <span className="pointer-events-none absolute inset-0 rounded-[6px] ring-1 ring-inset ring-black/[0.05]" />
      </span>
    );
  }
  if (message.kind === "voice") {
    return (
      <div className={cn("flex h-9 items-center gap-2 rounded-[5px] px-3 shadow-[0_1px_2px_rgba(0,0,0,0.06)]", mine ? "bg-[#95EC69]" : "bg-white")}>
        <Mic size={13} className={mine ? "text-[#3d5a2e]" : "text-[#666]"} />
        <Waveform bars={8} />
        <span className="text-[10px] tabular-nums text-[#777]">{message.duration}″</span>
      </div>
    );
  }
  if (message.kind === "file") {
    return (
      <div className="flex w-[198px] items-center gap-2.5 rounded-[6px] bg-white p-2.5 shadow-[0_2px_8px_rgba(0,0,0,0.07)]">
        <div className="relative grid h-10 w-8 shrink-0 place-items-center overflow-hidden rounded-[4px] bg-gradient-to-br from-[#FF8A8A] to-[#F06060] text-[7px] font-bold text-white">
          <span className="absolute inset-x-0 top-0 h-1/2 bg-gradient-to-b from-white/25 to-transparent" />
          <span className="relative">PDF</span>
        </div>
        <div className="min-w-0"><div className="truncate text-[10.5px] font-medium text-[#333]">{message.title}</div><div className="mt-1 text-[8.5px] tabular-nums text-[#AAA]">{message.size}</div></div>
      </div>
    );
  }
  if (message.kind === "redpacket") {
    return (
      <div className="w-[192px] overflow-hidden rounded-[7px] bg-gradient-to-br from-[#F6A94B] to-[#EE7F2D] text-white shadow-[0_4px_14px_rgba(238,127,45,0.35)]">
        <div className="relative flex items-center gap-2 p-2.5">
          <span className="absolute inset-0 opacity-20" style={{ backgroundImage: "radial-gradient(circle, #fff 0.7px, transparent 0.7px)", backgroundSize: "10px 10px" }} />
          <span className="relative grid h-9 w-9 shrink-0 place-items-center rounded-lg bg-[#D9532B] text-[19px] shadow-inner">🧧</span>
          <div className="relative min-w-0"><div className="line-clamp-1 text-[11px] font-medium">{message.text || "恭喜发财，大吉大利"}</div><div className="mt-0.5 text-[8px] text-white/85">微信红包 · 待领取</div></div>
        </div>
      </div>
    );
  }
  if (message.kind === "transfer") {
    return (
      <div className="w-[192px] overflow-hidden rounded-[7px] bg-gradient-to-br from-[#F7B733] to-[#F0962E] text-white shadow-[0_4px_14px_rgba(240,150,46,0.35)]">
        <div className="flex items-center gap-2 p-2.5"><span className="grid h-7 w-7 shrink-0 place-items-center rounded-full bg-white/25 text-sm shadow-inner">¥</span><div className="min-w-0"><div className="truncate text-[10.5px] font-medium">转账给 {message.title}</div><div className="mt-0.5 text-[8px] text-white/80">待收款 · 请及时确认</div></div></div>
        <div className="border-t border-white/25 bg-black/[0.06] px-3 py-1.5 text-[7.5px] text-white/80">微信支付 ›</div>
      </div>
    );
  }
  if (message.kind === "link") {
    return (
      <div className="w-[208px] rounded-[6px] bg-white p-2.5 shadow-[0_2px_8px_rgba(0,0,0,0.07)]">
        <div className="line-clamp-2 text-[10.5px] font-medium leading-4 text-[#222]">{message.title}</div>
        <div className="mt-1.5 flex gap-2">
          <div className="min-w-0 flex-1"><div className="line-clamp-2 text-[8px] leading-3.5 text-[#999]">{message.description}</div><div className="mt-1.5 flex items-center gap-1 text-[7.5px] text-[#bbb]"><span className="grid h-3 w-3 place-items-center rounded-[2px] bg-[#07C160] text-[6px] text-white">微</span>微信派</div></div>
          {message.image && <img src={message.image} alt="" className="h-9 w-9 shrink-0 rounded-[4px] object-cover" />}
        </div>
      </div>
    );
  }
  return (
    <div className={cn("relative max-w-[248px] rounded-[5px] px-2.5 py-[8px] text-[13px] leading-[1.55] text-[#171717]", mine ? "bg-[#95EC69] shadow-[0_1px_2px_rgba(110,160,60,0.3)]" : "bg-white shadow-[0_1px_3px_rgba(0,0,0,0.08)]")}>
      {message.text}
    </div>
  );
}

export function ChatScreen({ chatId, onNavigate, onNotify }: BaseProps & { chatId: string }) {
  const chat = resolveChat(chatId);
  const [allMessages, setAllMessages] = useState<Record<string, Message[]>>(mobileMessageCache);
  const [draft, setDraft] = useState("");
  const [showPlus, setShowPlus] = useState(false);
  const [showEmoji, setShowEmoji] = useState(false);
  const [showInfo, setShowInfo] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const list = allMessages[chat.id] || [{ id: "hello", side: "system" as const, kind: "system" as const, text: "开始新的对话" }];

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [chat.id]);

  const send = () => {
    if (!draft.trim()) return;
    setAllMessages((prev) => updateMobileMessageCache({
      ...prev,
      [chat.id]: [...(prev[chat.id] || []), { id: `${Date.now()}`, side: "me", kind: "text", text: draft.trim(), time: "刚刚" }],
    }));
    setDraft("");
    setShowPlus(false);
    setShowEmoji(false);
    setTimeout(() => {
      const el = scrollRef.current;
      if (el) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
    }, 30);
  };

  const plusItems = [
    { label: "相册", icon: ImageIcon, onClick: () => onNotify("相册已打开 · 最近 128 张") },
    { label: "拍摄", icon: Camera, onClick: () => onNavigate("scan") },
    { label: "视频通话", icon: Video, onClick: () => onNavigate("call") },
    { label: "位置", icon: MapPin, onClick: () => onNavigate("location") },
    { label: "红包", icon: Gift, onClick: () => onNavigate("redpacket") },
    { label: "转账", icon: ArrowLeftRight, onClick: () => onNavigate("transfer") },
    { label: "语音输入", icon: Mic, onClick: () => onNotify("正在聆听...") },
    { label: "收藏", icon: Star, onClick: () => onNavigate("favorites") },
  ];

  return (
    <div className="screen-enter relative flex h-full flex-col overflow-hidden bg-[#EDEDED]">
      <header className="hairline-b z-20 flex h-[74px] shrink-0 items-center justify-between bg-[#EDEDED]/94 px-2 pt-[27px] backdrop-blur-xl">
        <div className="flex min-w-0 items-center">
          <PhoneBack onClick={() => onNavigate("chats")} />
          <button onClick={() => setShowInfo(true)} className="flex max-w-[208px] items-center gap-1.5 text-left">
            <div className="min-w-0">
              <div className="flex items-center gap-1.5 truncate text-[13.5px] font-semibold text-[#1a1a1a]">
                <span className="truncate">{chat.name}</span>
                {chat.type === "group" && <span className="shrink-0 rounded bg-black/[0.06] px-1 py-px text-[8px] font-normal tabular-nums text-[#888]">{chat.avatars?.length || 4}</span>}
                {chat.type === "direct" && chat.online && <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-[#07C160]" />}
              </div>
              <div className="mt-0.5 truncate text-[8px] text-[#9a9a9a]">
                {chat.type === "group" ? "春日设计小组 · 4 位成员" : chat.online ? "在线" : chat.id === "file" ? "与手机互传文件" : "对方正在输入..."}
              </div>
            </div>
            <ChevronRight size={12} className="shrink-0 rotate-90 text-[#aaa]" />
          </button>
        </div>
        <button onClick={() => setShowInfo(true)} className="pressable grid h-8 w-8 shrink-0 place-items-center rounded-full text-[#2C2C2C] active:bg-black/5"><MoreHorizontal size={20} /></button>
      </header>

      <div ref={scrollRef} className="chat-canvas scrollbar-thin min-h-0 flex-1 overflow-y-auto px-3 py-3">
        <div className="mx-auto flex max-w-[340px] flex-col gap-[13px]">
          {list.map((m) => {
            if (m.side === "system") return <SystemPill key={m.id}>{m.text}</SystemPill>;
            const mine = m.side === "me";
            const sender = m.sender?.replace(/\s/g, "");
            const senderAvatar = sender ? contacts.find((c) => c.name.replace(/\s/g, "") === sender || c.name.replace(/\s/g, "").startsWith(sender))?.avatar : undefined;
            return (
              <div key={m.id} className={cn("flex items-start gap-2", mine && "flex-row-reverse")}>
                <Avatar src={mine ? me.avatar : senderAvatar || chat.avatar} name={mine ? me.name : m.sender || chat.name} className="h-[34px] w-[34px] rounded-[6px]" />
                <div className={cn("flex min-w-0 flex-col gap-1", mine ? "items-end" : "items-start")}>
                  {chat.type === "group" && !mine && m.sender && <span className="px-0.5 text-[8px] text-[#999]">{m.sender}</span>}
                  <div className={cn("relative", mine ? "items-end" : "items-start")}>
                    <MobileMessage message={m} />
                    {m.kind !== "image" && m.kind !== "sticker" && m.kind !== "file" && m.kind !== "link" && m.kind !== "redpacket" && m.kind !== "transfer" && (
                      <span className={cn("absolute top-[10px] h-[7px] w-[7px] rotate-45", mine ? "-right-[3px] bg-[#95EC69]" : "-left-[3px] bg-white")} />
                    )}
                  </div>
                </div>
              </div>
            );
          })}
          <div className="h-2" />
        </div>
      </div>

      <div className="hairline-t relative z-20 shrink-0 bg-[#F7F7F7]/98 pb-[22px] backdrop-blur-xl">
        {showPlus && (
          <div className="pop-in grid grid-cols-4 gap-y-4 border-b border-[#EAEAEA] bg-[#F7F7F7] px-5 py-4">
            {plusItems.map((item) => (
              <button key={item.label} onClick={item.onClick} className="pressable flex flex-col items-center gap-1.5">
                <span className="grid h-[52px] w-[52px] place-items-center rounded-[15px] bg-white text-[#3f3f3f] shadow-[0_2px_8px_rgba(0,0,0,0.06)] ring-1 ring-black/[0.03]"><item.icon size={22} strokeWidth={1.6} /></span>
                <span className="text-[9px] text-[#555]">{item.label}</span>
              </button>
            ))}
          </div>
        )}
        {showEmoji && (
          <div className="pop-in border-b border-[#EAEAEA] bg-[#F7F7F7] px-3 py-2.5">
            <div className="mb-1.5 flex items-center justify-between px-1">
              <div className="flex gap-1.5">
                {["最近", "收藏", "热门"].map((t, i) => <span key={t} className={cn("rounded-full px-2 py-0.5 text-[8.5px]", i === 0 ? "bg-[#07C160] font-medium text-white" : "bg-[#ECECEC] text-[#888]")}>{t}</span>)}
              </div>
              <button onClick={() => onNotify("表情商店 · 今日上新 12 套")} className="text-[9px] font-medium text-[#576B95]">表情商店 ›</button>
            </div>
            <div className="grid grid-cols-8 gap-0.5">
              {stickerSets.flat().map((emoji, i) => <button key={`${emoji}-${i}`} onClick={() => setDraft((d) => d + emoji)} className="grid h-8 place-items-center rounded-lg text-[20px] transition hover:scale-125 hover:bg-white">{emoji}</button>)}
            </div>
          </div>
        )}
        <div className="flex min-h-[52px] items-end gap-1.5 px-2.5 pb-2 pt-2">
          <button onClick={() => { setShowPlus(false); setShowEmoji((v) => !v); }} className={cn("pressable mb-[3px] grid h-8 w-8 shrink-0 place-items-center rounded-full", showEmoji ? "bg-[#E4F5EA] text-[#07A858]" : "text-[#3f3f3f]")}><Smile size={24} strokeWidth={1.6} /></button>
          <div className="min-h-[36px] flex-1 rounded-[9px] bg-white px-3 shadow-[0_1px_2px_rgba(0,0,0,0.04)] ring-1 ring-black/[0.04] transition focus-within:ring-[#07C160]/50">
            <input
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") send(); }}
              placeholder="说点什么..."
              className="h-[36px] w-full bg-transparent text-[13.5px] text-[#1a1a1a] outline-none placeholder:text-[#C2C2C2]"
            />
          </div>
          {draft.trim() ? (
            <button onClick={send} className="pressable mb-[3px] flex h-[32px] shrink-0 items-center gap-1 rounded-[8px] bg-gradient-to-b from-[#0BD566] to-[#06B058] px-3 text-[11px] font-semibold text-white shadow-[0_4px_12px_rgba(7,193,96,0.4)]"><Send size={12} />发送</button>
          ) : (
            <>
              <button onClick={() => onNotify("按住说话 · 松开发送")} className="mb-[3px] grid h-8 w-7 shrink-0 place-items-center text-[#3f3f3f]"><Mic size={22} strokeWidth={1.6} /></button>
              <button onClick={() => { setShowPlus((v) => !v); setShowEmoji(false); }} className={cn("pressable mb-[3px] grid h-8 w-7 shrink-0 place-items-center transition-transform duration-200", showPlus ? "rotate-45 text-[#07A858]" : "text-[#3f3f3f]")}><CirclePlus size={24} strokeWidth={1.5} /></button>
            </>
          )}
        </div>
      </div>

      {showInfo && (
        <div className="absolute inset-0 z-40 flex flex-col bg-black/35 backdrop-blur-[1px]" onClick={() => setShowInfo(false)}>
          <div className="mt-auto max-h-[84%] overflow-y-auto scrollbar-thin rounded-t-[20px] bg-[#F2F2F2] pb-7 shadow-2xl" style={{ animation: "sheetUp 0.4s cubic-bezier(0.22,1,0.36,1) both" }} onClick={(e) => e.stopPropagation()}>
            <div className="mx-auto mt-2 h-1 w-9 rounded-full bg-[#CFCFCF]" />
            <div className="flex items-center justify-between px-4 py-3">
              <button onClick={() => setShowInfo(false)} className="rounded-full bg-[#E8E8E8] px-3 py-1.5 text-[10px] text-[#666]">关闭</button>
              <span className="text-[13px] font-semibold">聊天信息</span>
              <button onClick={() => { setShowInfo(false); onNotify("设置已保存"); }} className="rounded-full bg-[#07C160] px-3 py-1.5 text-[10px] font-medium text-white">完成</button>
            </div>
            <div className="flex gap-2.5 overflow-x-auto px-4 pb-4 no-scrollbar">
              {[me, ...contacts.slice(0, 4)].map((p) => <div key={p.id} className="flex w-12 shrink-0 flex-col items-center gap-1.5"><Avatar src={p.avatar} name={p.name} className="h-11 w-11 rounded-[8px]" /><span className="w-full truncate text-center text-[8px] text-[#777]">{p.name}</span></div>)}
              <button onClick={() => onNotify("添加群聊成员")} className="flex w-12 shrink-0 flex-col items-center gap-1.5"><span className="grid h-11 w-11 place-items-center rounded-[8px] border-[1.5px] border-dashed border-[#C5C5C5] bg-white text-[#AAA]"><Plus size={19} /></span><span className="text-[8px] text-[#777]">添加</span></button>
            </div>
            <div className="mx-3 overflow-hidden rounded-xl bg-white shadow-sm">
              {[
                { label: "查找聊天记录", desc: "图片、视频、文件、链接", onClick: () => onNotify("搜索聊天记录") },
                { label: "消息免打扰", toggle: true },
                { label: "置顶聊天", toggle: false },
              ].map((row: any) => (
                <div key={row.label} className="flex min-h-[50px] items-center justify-between border-b border-[#F4F4F4] px-4 text-[12px] text-[#222] last:border-0">
                  <button onClick={row.onClick} className="text-left">
                    <span className="block">{row.label}</span>
                    {row.desc && <span className="mt-0.5 block text-[8.5px] text-[#aaa]">{row.desc}</span>}
                  </button>
                  {row.toggle !== undefined ? <Switch small on={row.toggle} onChange={() => onNotify(`${row.label}已更新`)} /> : <ChevronRight size={14} className="text-[#C5C5C5]" />}
                </div>
              ))}
            </div>
            <div className="mx-3 mt-2.5 rounded-xl bg-white p-3.5 shadow-sm">
              <div className="mb-2.5 flex items-center justify-between"><span className="text-[11px] font-medium text-[#333]">聊天文件</span><button onClick={() => onNotify("全部 12 个文件")} className="text-[9px] text-[#576B95]">全部 12 ›</button></div>
              <div className="grid grid-cols-3 gap-1.5">
                {[assets.gallery, assets.tulips, assets.coffee].map((img, i) => (
                  <button key={i} onClick={() => onNotify("图片预览")} className="overflow-hidden rounded-lg"><img src={img} alt="" className="h-[76px] w-full object-cover transition hover:scale-105" /></button>
                ))}
              </div>
            </div>
            <button onClick={() => onNotify("聊天记录已清空（演示）")} className="mx-3 mt-2.5 w-[calc(100%-24px)] rounded-xl bg-white py-3 text-[11.5px] font-medium text-[#D95353] shadow-sm">清空聊天记录</button>
          </div>
        </div>
      )}
    </div>
  );
}

export function MomentsScreen({ onNavigate, onNotify }: BaseProps) {
  const [liked, setLiked] = useState<string[]>(["mo1"]);
  const [commenting, setCommenting] = useState<string | null>(null);
  return (
    <div className="screen-enter scrollbar-thin relative h-full overflow-y-auto bg-[#FAFAF8] pb-8">
      <div className="relative h-[248px] shrink-0 overflow-hidden bg-[#28352E]">
        <img src={assets.cover} alt="朋友圈封面" className="h-full w-full object-cover" loading="lazy" />
        <div className="absolute inset-0 bg-gradient-to-b from-black/60 via-black/10 to-black/25" />
        <div className="absolute inset-x-0 top-[30px] z-20 flex h-[42px] items-center justify-between px-3 text-white">
          <button onClick={() => onNavigate("discover")} className="pressable grid h-8 w-8 place-items-center rounded-full bg-black/25 backdrop-blur"><ArrowLeft size={20} strokeWidth={1.8} /></button>
          <div className="flex items-center gap-2.5">
            <button onClick={() => onNotify("发表朋友圈 · 拍摄或从相册选择")} className="pressable grid h-8 w-8 place-items-center rounded-full bg-black/25 backdrop-blur"><Camera size={18} strokeWidth={1.8} /></button>
            <button onClick={() => onNotify("朋友圈设置：权限 / 提醒 / 封面")} className="pressable grid h-8 w-8 place-items-center rounded-full bg-black/25 backdrop-blur"><MoreHorizontal size={19} /></button>
          </div>
        </div>
        <div className="absolute inset-x-0 bottom-0 flex items-end justify-between px-4 pb-3">
          <div className="mb-8 max-w-[190px] text-white">
            <div className="text-[15px] font-semibold drop-shadow-md">{me.name}</div>
            <div className="mt-1 line-clamp-1 text-[9px] text-white/85">{me.note}</div>
          </div>
          <button onClick={() => onNotify("更换封面与头像")} className="relative shrink-0">
            <img src={me.avatar} alt={me.name} className="h-[64px] w-[64px] rounded-xl border-2 border-white/90 object-cover shadow-2xl" />
            <span className="absolute -bottom-1 -right-1 grid h-5 w-5 place-items-center rounded-full bg-[#07C160] text-white shadow ring-2 ring-white"><Camera size={10} /></span>
          </button>
        </div>
      </div>

      <div className="px-3 pt-3">
        <div className="flex items-center justify-between rounded-xl bg-white px-3.5 py-2.5 shadow-sm">
          <span className="flex items-center gap-1.5 text-[10px] font-medium text-[#555]"><Sparkles size={12} className="text-[#07A858]" /> 我的朋友圈</span>
          <button onClick={() => onNotify("查看我的朋友圈相册")} className="flex items-center gap-0.5 text-[9px] text-[#999]">全部照片 <ChevronRight size={10} /></button>
        </div>
        {moments.map((m) => {
          const isLiked = liked.includes(m.id);
          return (
            <article key={m.id} className="mt-2.5 rounded-xl bg-white p-3.5 shadow-[0_1px_4px_rgba(0,0,0,0.04)]">
              <div className="flex gap-2.5">
                <Avatar src={m.avatar} name={m.name} className="h-10 w-10 rounded-[8px]" />
                <div className="min-w-0 flex-1">
                  <div className="text-[12.5px] font-semibold text-[#4B6E91]">{m.name}</div>
                  <p className="mt-1 text-[12.5px] leading-[1.65] text-[#282828]">{m.text}</p>
                  {m.images && (
                    <div className={cn("mt-2 gap-1.5", m.images.length === 1 ? "max-w-[215px]" : "grid grid-cols-3")}>
                      {m.images.map((img, i) => (
                        <button key={i} onClick={() => onNotify("图片预览 · 双指缩放")} className="group block overflow-hidden rounded-[6px]">
                          <img src={img} alt="朋友圈图片" className={cn("object-cover transition duration-500 group-hover:scale-105", m.images!.length === 1 ? "max-h-[195px] w-full" : "aspect-square w-full")} loading="lazy" />
                        </button>
                      ))}
                    </div>
                  )}
                  <div className="mt-2.5 flex items-center justify-between">
                    <span className="text-[9px] tabular-nums text-[#ADADAD]">{m.time}{m.location && <> · <span className="text-[#7089A6]">{m.location}</span></>}</span>
                    <div className="flex items-center gap-1">
                      <button
                        onClick={() => { setLiked((arr) => isLiked ? arr.filter((id) => id !== m.id) : [...arr, m.id]); if (!isLiked) onNotify(`赞了 ${m.name} 的朋友圈`); }}
                        className={cn("pressable flex h-7 items-center gap-1 rounded-full px-2.5 text-[9px] tabular-nums", isLiked ? "bg-[#FFF0F0] font-medium text-[#FA5151]" : "bg-[#F5F5F5] text-[#777]")}
                      >
                        <Heart size={12} fill={isLiked ? "currentColor" : "none"} className={isLiked ? "heart-burst" : ""} />
                        {m.likes.length + (isLiked ? 1 : 0)}
                      </button>
                      <button onClick={() => setCommenting((v) => v === m.id ? null : m.id)} className="pressable flex h-7 items-center gap-1 rounded-full bg-[#F5F5F5] px-2.5 text-[9px] tabular-nums text-[#777]"><MessageCircle size={12} />{m.comments.length}</button>
                    </div>
                  </div>
                  {(liked.length > 0 || m.comments.length > 0) && (
                    <div className="relative mt-2 rounded-lg bg-[#F7F7F7] px-3 py-2.5">
                      <span className="absolute -top-[4px] left-4 h-2 w-2 rotate-45 bg-[#F7F7F7]" />
                      <div className="flex items-start gap-1.5 text-[10px] leading-[1.6] text-[#576B95]"><Heart size={9} className="mt-[5px] shrink-0" /><span>{["苏禾", "Anna", "知夏", ...(isLiked ? [] : ["小满"])].join("、")}</span></div>
                      {m.comments.map((c, i) => <div key={i} className="mt-1 text-[10px] leading-[1.6] text-[#444]"><span className="font-medium text-[#576B95]">{c.name}：</span>{c.text}</div>)}
                    </div>
                  )}
                  {commenting === m.id && (
                    <div className="pop-in mt-2 flex items-center gap-2 rounded-full bg-[#F2F2F2] py-1.5 pl-3.5 pr-1.5 ring-1 ring-black/[0.03] focus-within:bg-white focus-within:ring-[#07C160]/40">
                      <input autoFocus placeholder={`评论 ${m.name}...`} className="min-w-0 flex-1 bg-transparent text-[11px] outline-none placeholder:text-[#B5B5B5]" />
                      <button onClick={() => { setCommenting(null); onNotify("评论已发送"); }} className="rounded-full bg-[#07C160] px-3 py-1.5 text-[9px] font-medium text-white">发送</button>
                    </div>
                  )}
                </div>
              </div>
            </article>
          );
        })}
        <div className="py-4 text-center text-[8.5px] text-[#B8B8B8]">朋友仅展示最近半年的朋友圈</div>
      </div>
    </div>
  );
}

export function ChannelsScreen({ onNavigate, onNotify }: BaseProps) {
  const [liked, setLiked] = useState(false);
  const [muted, setMuted] = useState(true);
  const [tab, setTab] = useState("推荐");
  const [comments, setComments] = useState(false);
  return (
    <div className="relative h-full overflow-hidden bg-black">
      <img src={assets.dawn} alt="视频号内容" className="absolute inset-0 h-full w-full object-cover" loading="lazy" />
      <div className="absolute inset-0 bg-gradient-to-b from-black/55 via-transparent to-black/75" />
      <div className="absolute inset-x-0 top-[30px] z-20 flex h-[44px] items-center justify-between px-3 text-white">
        <button onClick={() => onNavigate("discover")}><ArrowLeft size={23} strokeWidth={1.6} /></button>
        <div className="flex items-center gap-4 text-[13px]">{["关注", "推荐", "朋友"].map((t) => <button key={t} onClick={() => setTab(t)} className={cn("relative py-1", tab === t ? "font-medium" : "text-white/60")}>{t}{tab === t && <span className="absolute -bottom-1.5 left-1/2 h-[2px] w-[14px] -translate-x-1/2 rounded bg-white" />}</button>)}</div>
        <button onClick={() => onNavigate("search")}><Search size={21} strokeWidth={1.7} /></button>
      </div>

      <div className="absolute right-3 top-[145px] z-20 flex flex-col items-center gap-5 text-white">
        <div className="relative">
          <img src={px(27688436, 100, 100)} alt="作者" className="h-10 w-10 rounded-full border-2 border-white object-cover" />
          <span className="absolute -bottom-2 left-1/2 grid h-4 w-4 -translate-x-1/2 place-items-center rounded-full bg-[#FA5151]"><Plus size={10} /></span>
        </div>
        <button onClick={() => setLiked((v) => !v)} className="flex flex-col items-center gap-1"><Heart size={27} fill={liked ? "#FA5151" : "none"} className={liked ? "text-[#FA5151]" : ""} /><span className="text-[9px]">{liked ? "2.5w" : "2.4w"}</span></button>
        <button onClick={() => setComments(true)} className="flex flex-col items-center gap-1"><MessageCircle size={27} /><span className="text-[9px]">318</span></button>
        <button onClick={() => onNotify("分享给朋友")} className="flex flex-col items-center gap-1"><Share2 size={26} /><span className="text-[9px]">分享</span></button>
        <button onClick={() => setMuted((v) => !v)} className="flex flex-col items-center gap-1">{muted ? <VolumeX size={25} /> : <Volume2 size={25} />}<span className="text-[9px]">{muted ? "静音" : "声音"}</span></button>
      </div>

      <div className="absolute inset-x-0 bottom-0 z-20 px-4 pb-[28px] text-white">
        <div className="mb-2 flex items-center gap-2"><span className="text-[12px] font-medium">@周予然</span><span className="rounded-full border border-white/40 px-1.5 py-0.5 text-[7.5px]">朋友关注</span></div>
        <p className="max-w-[260px] text-[11px] leading-[1.65] text-white/90">在路上的第 143 天。风把湖面吹皱的时候，刚好有人按下快门。<br /><span className="text-white/75">#旅行的意义 #洱海日记</span></p>
        <div className="mt-2 flex items-center gap-1.5 text-[8.5px] text-white/65"><Music size={11} /> 风经过的地方 · 房东的猫</div>
        <div className="mt-3 flex items-center gap-2 rounded-lg bg-black/20 p-2 backdrop-blur-sm">
          <img src={assets.rocks} alt="" className="h-9 w-9 rounded object-cover" />
          <div className="min-w-0 flex-1"><div className="text-[9.5px]">发现身边的小众旅行地</div><div className="mt-0.5 text-[7.5px] text-white/55">小红书 · 种草笔记</div></div>
          <ChevronRight size={13} className="text-white/70" />
        </div>
        <div className="mt-3 flex items-center gap-2"><div className="h-[2px] flex-1 rounded-full bg-white/25"><div className="h-full w-[38%] rounded-full bg-white/85" /></div><span className="text-[8px] text-white/70">01:42 / 04:38</span></div>
      </div>

      {comments && (
        <div className="absolute inset-0 z-40 flex flex-col bg-black/25" onClick={() => setComments(false)}>
          <div className="mt-auto max-h-[64%] overflow-y-auto rounded-t-2xl bg-white pb-6" onClick={(e) => e.stopPropagation()}>
            <div className="sticky top-0 flex items-center justify-between border-b border-[#F2F2F2] bg-white px-4 py-3"><span className="text-[11px] text-[#888]">{liked ? "2.5w" : "2.4w"} 次赞</span><span className="text-[12px] font-medium">318 条评论</span><button onClick={() => setComments(false)} className="text-[11px] text-[#576B95]">关闭</button></div>
            <div className="divide-y divide-[#F5F5F5] px-4">
              {[
                [px(14664508, 80, 80), "林小满", "这片云好像一张洇开的纸，太美了。", "2 小时前"],
                [px(9854302, 80, 80), "江屿", "下一站去哪里？等你更新。", "1 小时前"],
                [px(20894157, 80, 80), "陈知夏", "想去这里拍照！", "48 分钟前"],
              ].map(([avatar, name, text, time]) => (
                <div key={name} className="flex gap-2.5 py-3"><img src={avatar} alt="" className="h-7 w-7 rounded-full object-cover" /><div><div className="text-[9.5px] text-[#6A879F]">{name}</div><div className="mt-1 text-[11px] leading-4 text-[#333]">{text}</div><div className="mt-1 text-[8px] text-[#AAA]">{time} · 回复</div></div></div>
              ))}
            </div>
            <div className="mx-4 mt-2 flex items-center gap-2 rounded-full bg-[#F5F5F5] px-3 py-2"><input placeholder="说点什么..." className="flex-1 bg-transparent text-[11px] outline-none" /><Heart size={15} className="text-[#999]" /><Send size={14} className="text-[#07C160]" /></div>
          </div>
        </div>
      )}
    </div>
  );
}

export function MiniProgramsScreen({ onNavigate, onNotify }: BaseProps) {
  return (
    <div className="flex h-full flex-col bg-[#F5F5F5]">
      <MobileTop title="小程序" onBack={() => onNavigate("discover")} right={<><Search size={18} /><MoreHorizontal size={19} /></>} />
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pb-6 pt-2.5">
        <div className="flex h-8 items-center gap-1.5 rounded-md bg-white px-2.5 text-[#AAA] ring-1 ring-black/[0.03]"><Search size={13} /><span className="text-[10.5px]">搜索小程序</span></div>
        <div className="mt-2.5 grid grid-cols-2 gap-2">
          <button onClick={() => onNotify("正在打开微信读书")} className="relative h-[100px] overflow-hidden rounded-lg bg-[#22342B] p-3 text-left text-white">
            <img src={assets.dawn} alt="" className="absolute inset-0 h-full w-full object-cover opacity-40" />
            <div className="relative z-10"><div className="text-[7.5px] text-white/60">今日精选</div><div className="mt-1.5 text-[11.5px] font-medium leading-4">在文字里<br />去更远的地方</div><span className="mt-2 inline-block rounded-full bg-white px-2 py-0.5 text-[7.5px] text-[#222]">去阅读</span></div>
          </button>
          <button onClick={() => onNotify("正在打开腾讯地图")} className="relative h-[100px] overflow-hidden rounded-lg bg-[#EFE7D9] p-3 text-left">
            <div className="text-[7.5px] text-[#9C8467]">周末计划</div><div className="mt-1.5 text-[11.5px] font-medium leading-4 text-[#4A3B2A]">城市漫游<br />从这里开始</div><span className="mt-2 inline-block rounded-full bg-[#4A3B2A] px-2 py-0.5 text-[7.5px] text-white">去探索</span><span className="absolute -bottom-2 -right-1 text-4xl">🗺️</span>
          </button>
        </div>
        <div className="mt-3 rounded-xl bg-white p-3">
          <div className="mb-2.5 flex items-center justify-between"><span className="text-[11px] font-medium text-[#333]">最近使用</span><button className="text-[8.5px] text-[#AAA]">管理</button></div>
          <div className="grid grid-cols-4 gap-y-3">
            {miniApps.map((app) => (
              <button key={app.id} onClick={() => onNotify(`正在打开「${app.name}」`)} className="flex flex-col items-center gap-1.5">
                <span className="grid h-11 w-11 place-items-center rounded-xl text-xl" style={{ background: app.bg }}>{app.emoji}</span>
                <span className="max-w-full truncate text-[9px] text-[#555]">{app.name}</span>
              </button>
            ))}
          </div>
        </div>
        <div className="mt-3 rounded-xl bg-white p-3">
          <div className="flex items-center justify-between"><span className="text-[11px] font-medium text-[#333]">附近的小程序</span><button className="text-[8.5px] text-[#AAA]">查看全部</button></div>
          {[
            ["%Arabica 咖啡", "距你 220m · 咖啡", "#F2E7DB", "☕"],
            ["天目里美术馆", "距你 1.2km · 展览", "#E7EFE9", "🎨"],
            ["KEEP 运动空间", "距你 640m · 健身", "#EAF1FA", "🏃"],
          ].map(([name, sub, bg, emoji]) => (
            <button key={name} onClick={() => onNotify(`正在打开「${name}」`)} className="mt-2 flex w-full items-center gap-2.5 rounded-lg bg-[#FAFAFA] p-2 text-left">
              <span className="grid h-8 w-8 place-items-center rounded-lg text-base" style={{ background: bg }}>{emoji}</span>
              <span className="flex-1"><span className="block text-[10px] text-[#333]">{name}</span><span className="mt-0.5 block text-[8px] text-[#AAA]">{sub}</span></span><ChevronRight size={12} className="text-[#C6C6C6]" />
            </button>
          ))}
        </div>
        <div className="mt-4 text-center text-[8px] text-[#B6B6B6]">更多小程序，等你发现</div>
      </div>
    </div>
  );
}

export function SearchScreen({ onBack, onNotify }: { onBack: () => void; onNotify: (s: string) => void }) {
  const [query, setQuery] = useState("");
  const suggestions = ["春日回声 展览", "微信设计", "天目里美术馆", "朋友圈封面", "周末去哪儿"];
  return (
    <div className="flex h-full flex-col bg-[#F5F5F5] pt-[30px]">
      <div className="flex h-[48px] shrink-0 items-center gap-2 bg-[#EDEDED] px-3">
        <div className="flex h-8 flex-1 items-center gap-2 rounded-md bg-white px-2.5"><Search size={14} className="text-[#888]" /><input autoFocus value={query} onChange={(e) => setQuery(e.target.value)} placeholder="搜索" className="flex-1 bg-transparent text-[12px] outline-none" />{query && <button onClick={() => setQuery("")}><X size={13} className="text-[#AAA]" /></button>}</div>
        <button onClick={onBack} className="text-[12px] text-[#222]">取消</button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {query ? (
          <div className="bg-white px-4 py-4">
            <div className="text-[9px] text-[#A8A8A8]">搜索「{query}」</div>
            {[["聊天记录", `${query} 的相关消息 · 3 条`], ["联系人", "苏禾、林小满 · 2 位"], ["公众号", "设计手记 · 找到 1 个"], ["小程序", "微信读书 · 找到 1 个"], ["朋友圈", "朋友分享 · 找到 2 条"]].map(([title, sub]) => <button key={title} onClick={() => onNotify(`${title}搜索结果`)} className="mt-1 flex w-full items-center gap-3 border-b border-[#F5F5F5] py-2.5 text-left last:border-0"><span className="grid h-8 w-8 place-items-center rounded-lg bg-[#F1F1F1] text-[#777]"><Search size={14} /></span><span className="flex-1"><span className="block text-[11px] text-[#333]">{title}</span><span className="mt-0.5 block text-[8.5px] text-[#AAA]">{sub}</span></span><ChevronRight size={13} className="text-[#C8C8C8]" /></button>)}
          </div>
        ) : (
          <>
            <div className="bg-white px-4 pb-3 pt-3">
              <div className="flex items-center justify-between"><span className="text-[10px] font-medium text-[#333]">搜索发现</span><button onClick={() => onNotify("已刷新搜索发现")} className="text-[8.5px] text-[#AAA]">换一换</button></div>
              <div className="mt-2 flex flex-wrap gap-2">{["春日看展指南", "微信隐藏功能", "杭州周末", "手机摄影", "桌面端效率"].map((tag) => <button key={tag} onClick={() => setQuery(tag)} className="rounded-full border border-[#EEE] bg-[#FAFAFA] px-2.5 py-1 text-[9px] text-[#666]">{tag}</button>)}</div>
            </div>
            <div className="mt-2 bg-white px-4 py-3">
              <div className="flex items-center justify-between"><span className="text-[10px] font-medium text-[#333]">最近搜索</span><button onClick={() => onNotify("已清空搜索历史")} className="text-[8.5px] text-[#AAA]">清空</button></div>
              <div className="mt-2 flex flex-wrap gap-2">{suggestions.map((s) => <button key={s} onClick={() => setQuery(s)} className="rounded-full bg-[#F5F5F5] px-2.5 py-1 text-[9px] text-[#666]">{s}</button>)}</div>
            </div>
            <div className="mt-2 bg-white px-4 py-3">
              <div className="flex items-center gap-1.5"><Sparkles size={12} className="text-[#FA9D3B]" /><span className="text-[10px] font-medium text-[#333]">微信热搜</span><span className="ml-auto text-[8px] text-[#B0B0B0]">实时</span></div>
              {["「春日回声」开展，城市里的自然", "桌面端微信改版细节", "如何拍出有呼吸感的照片", "周末短途旅行清单", "微信读书今日推荐"].map((t, i) => <button key={t} onClick={() => setQuery(t)} className="mt-2 flex w-full items-center gap-2 text-left"><span className={cn("text-[9px]", i < 3 ? "text-[#FA5151]" : "text-[#B0B0B0]")}>{String(i + 1).padStart(2, "0")}</span><span className="flex-1 truncate text-[10.5px] text-[#444]">{t}</span><Zap size={10} className="text-[#FA9D3B]" /></button>)}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export function ScanScreen({ onNavigate, onNotify }: BaseProps) {
  const [torch, setTorch] = useState(false);
  const [tab, setTab] = useState("扫码");
  return (
    <div className="relative flex h-full flex-col overflow-hidden bg-[#111]">
      <img src={assets.whiteWall} alt="取景画面" className="absolute inset-0 h-full w-full object-cover opacity-65 blur-[1.5px]" />
      <div className="absolute inset-0 bg-black/25" />
      <div className="relative z-10 flex h-[78px] shrink-0 items-center justify-start px-3 pt-[24px]">
        <button onClick={() => onNavigate("discover")} className="grid h-8 w-8 place-items-center rounded-full bg-black/20 text-white"><ArrowLeft size={22} /></button>
        <div className="ml-3 text-[13px] font-medium text-white">扫一扫</div>
      </div>
      <div className="relative z-10 flex flex-1 flex-col items-center justify-center pb-10">
        <div className="mb-6 text-[10px] text-white/80">将二维码 / 条码放入框内，即可自动扫描</div>
        <div className="relative h-[230px] w-[230px]">
          <div className="absolute left-0 top-0 h-8 w-8 rounded-tl-md border-l-2 border-t-2 border-[#07C160]" />
          <div className="absolute right-0 top-0 h-8 w-8 rounded-tr-md border-r-2 border-t-2 border-[#07C160]" />
          <div className="absolute bottom-0 left-0 h-8 w-8 rounded-bl-md border-b-2 border-l-2 border-[#07C160]" />
          <div className="absolute bottom-0 right-0 h-8 w-8 rounded-br-md border-b-2 border-r-2 border-[#07C160]" />
          <div className="scan-line absolute left-3 right-3 top-1/2 h-[1px] bg-[#07C160] shadow-[0_0_12px_#07C160]" />
          <div className="absolute inset-0 grid place-items-center"><QrCode size={44} className="text-white/15" /></div>
        </div>
        <div className="mt-8 flex items-center gap-9 text-white">
          <button onClick={() => onNotify("已从相册选取二维码")} className="flex flex-col items-center gap-1.5"><span className="grid h-10 w-10 place-items-center rounded-full bg-white/15"><ImageIcon size={19} /></span><span className="text-[9px]">相册</span></button>
          <button onClick={() => setTorch((v) => !v)} className={cn("flex flex-col items-center gap-1.5", torch && "text-[#07C160]")}><span className={cn("grid h-10 w-10 place-items-center rounded-full", torch ? "bg-[#07C160] text-white" : "bg-white/15")}><Zap size={19} /></span><span className="text-[9px]">{torch ? "已开启" : "手电筒"}</span></button>
          <button onClick={() => onNotify("已切换至识物模式")} className="flex flex-col items-center gap-1.5"><span className="grid h-10 w-10 place-items-center rounded-full bg-white/15"><ZoomIn size={19} /></span><span className="text-[9px]">识物</span></button>
        </div>
      </div>
      <div className="relative z-10 flex h-[92px] shrink-0 items-center justify-center gap-7 bg-black/25 pb-4 text-white backdrop-blur-sm">
        {["扫码", "识物", "翻译"].map((t) => <button key={t} onClick={() => { setTab(t); onNotify(`已切换至${t}模式`); }} className={cn("text-[11px]", tab === t ? "font-medium text-[#07C160]" : "text-white/60")}>{t}</button>)}
      </div>
    </div>
  );
}

export function ReadingScreen({ onNavigate, onNotify }: BaseProps) {
  const [liked, setLiked] = useState(false);
  return (
    <div className="relative flex h-full flex-col bg-white">
      <div className="min-h-0 flex-1 overflow-y-auto pb-16">
        <div className="relative h-[245px] shrink-0">
          <img src={assets.gallery} alt="文章封面" className="h-full w-full object-cover" loading="lazy" />
          <div className="absolute inset-0 bg-gradient-to-b from-black/45 via-transparent to-black/20" />
          <div className="absolute inset-x-0 top-[30px] z-10 flex h-[40px] items-center justify-between px-3 text-white"><button onClick={() => onNavigate("discover")} className="grid h-8 w-8 place-items-center rounded-full bg-black/20"><ArrowLeft size={21} /></button><button onClick={() => onNotify("已收藏文章")} className="grid h-8 w-8 place-items-center rounded-full bg-black/20"><MoreHorizontal size={20} /></button></div>
        </div>
        <article className="relative -mt-3 rounded-t-2xl bg-white px-5 pb-6 pt-5">
          <h1 className="text-[20px] font-medium leading-[1.45] tracking-[-0.01em] text-[#1D1D1D]">在风经过的地方，<br />重新理解日常</h1>
          <div className="mt-3 flex items-center gap-2"><span className="rounded-sm bg-[#EEF7F0] px-1.5 py-0.5 text-[8px] text-[#07A858]">生活观察</span><span className="text-[8.5px] text-[#AAA]">2026 年 7 月 18 日</span><button onClick={() => onNotify("已关注公众号")} className="ml-auto text-[8.5px] text-[#576B95]">设计手记 ›</button></div>
          <div className="mt-4 flex items-center gap-2.5 border-b border-[#F0F0F0] pb-4"><img src={px(20894157, 80, 80)} alt="" className="h-8 w-8 rounded-full object-cover" /><div><div className="text-[10px] text-[#555]">陈知夏</div><div className="mt-0.5 text-[8px] text-[#AAA]">摄影 / 文字</div></div><span className="ml-auto rounded-full border border-[#E6E6E6] px-2.5 py-1 text-[8px] text-[#666]">关注</span></div>
          <div className="mt-5 space-y-4 text-[12.5px] leading-[1.95] text-[#3E3E3E]">
            <p>清晨六点半，天光刚刚漫过山的轮廓。湖面还没有醒，风从很远的地方来，带来一点水汽，和青草被露水打湿的味道。</p>
            <p>我喜欢在这样的时刻出门。没有目的，也不需要赶路，只是走着，看光线一点点改变周围事物的样子。</p>
            <img src={assets.lake} alt="文章图片" className="w-full rounded-sm" loading="lazy" />
            <div className="text-center text-[8.5px] text-[#AAA]">清晨的湖面，安静得像一张尚未落笔的纸。</div>
            <p>朋友说，旅行的意义是抵达。但我觉得，很多时候它只是让我们在陌生的地方，重新看见习以为常的生活。</p>
            <p>比如一顿慢慢吃完的早餐，比如路边一只晒太阳的猫，比如一整天都不用回复消息的自在。</p>
            <p className="border-l-2 border-[#07C160] pl-3 text-[#777]">在风经过的地方，时间有了形状。</p>
            <p>回程的路上，我把窗户摇下来一点。风灌进来，吹乱了头发，也吹散了一整个下午的困倦。</p>
          </div>
          <div className="mt-5 border-t border-[#F0F0F0] pt-3 text-[8.5px] text-[#AAA]">阅读 2,318 <span className="mx-2">·</span> 在看 142</div>
        </article>
      </div>
      <div className="absolute inset-x-0 bottom-0 z-20 flex h-[56px] items-center gap-3 border-t border-[#EDEDED] bg-white/95 px-4 pb-[18px] pt-1 backdrop-blur">
        <div className="flex flex-1 items-center gap-1.5 rounded-full bg-[#F5F5F5] px-3 py-1.5"><FileText size={12} className="text-[#999]" /><span className="text-[9px] text-[#AAA]">写留言...</span></div>
        <button onClick={() => setLiked((v) => !v)} className={cn("flex flex-col items-center gap-0.5", liked ? "text-[#FA5151]" : "text-[#777]")}><Heart size={17} fill={liked ? "currentColor" : "none"} /><span className="text-[7.5px]">在看</span></button>
        <button onClick={() => onNotify("分享到聊天")} className="flex flex-col items-center gap-0.5 text-[#777]"><Share2 size={17} /><span className="text-[7.5px]">分享</span></button>
        <button onClick={() => onNotify("已收藏文章")} className="flex flex-col items-center gap-0.5 text-[#777]"><Star size={17} /><span className="text-[7.5px]">收藏</span></button>
      </div>
    </div>
  );
}
