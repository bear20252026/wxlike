import { useEffect, useMemo, useRef, useState } from "react";
import {
  Bell,
  CheckCheck,
  Folder,
  Image as ImageIcon,
  Info,
  Mic,
  MoreHorizontal,
  Phone,
  Scissors,
  Search,
  Smile,
  Video,
  X,
} from "lucide-react";
import type { Chat, Message } from "../../data";
import { contacts, me } from "../../data";
import { Avatar, GroupAvatar, Switch, SystemPill, Waveform, stickerSets } from "../shared";
import { cn } from "../../utils/cn";

function BubbleTail({ mine }: { mine: boolean }) {
  return (
    <span
      className={cn(
        "absolute top-[12px] h-[9px] w-[9px] rotate-45",
        mine ? "-right-[3.5px] bg-[#95EC69]" : "-left-[3.5px] bg-white",
      )}
    />
  );
}

function MessageContent({ message, onNotify }: { message: Message; onNotify: (s: string) => void }) {
  const mine = message.side === "me";
  const [playing, setPlaying] = useState(false);

  if (message.kind === "sticker") {
    return (
      <div className="pop-in px-1 py-0.5 text-[46px] leading-none drop-shadow-sm transition hover:scale-110">{message.text}</div>
    );
  }
  if (message.kind === "image") {
    return (
      <button
        onClick={() => onNotify("图片预览 · 双击放大")}
        className="group relative overflow-hidden rounded-[6px] bg-white p-[3px] shadow-[0_2px_8px_rgba(0,0,0,0.08)] transition hover:shadow-[0_6px_20px_rgba(0,0,0,0.12)]"
      >
        <img src={message.image} alt="聊天图片" className="max-h-[172px] w-[232px] rounded-[4px] object-cover transition duration-500 group-hover:scale-[1.02]" loading="lazy" />
        <span className="absolute inset-[3px] rounded-[4px] ring-1 ring-inset ring-black/[0.04]" />
      </button>
    );
  }
  if (message.kind === "voice") {
    return (
      <button
        onClick={() => { setPlaying(!playing); if (!playing) setTimeout(() => setPlaying(false), 2500); }}
        className={cn(
          "group relative flex h-[36px] items-center gap-2.5 rounded-[5px] px-3.5 shadow-[0_1px_2px_rgba(0,0,0,0.05)] transition hover:shadow-[0_2px_8px_rgba(0,0,0,0.08)]",
          mine ? "bg-[#95EC69] hover:bg-[#8ae35f]" : "bg-white hover:bg-[#FAFAFA]",
        )}
      >
        <BubbleTail mine={mine} />
        <Mic size={13} className={mine ? "text-[#3d5a2e]" : "text-[#666]"} />
        <Waveform bars={10} playing={playing} />
        <span className="ml-1 text-[11px] tabular-nums text-[#666]">{message.duration}″</span>
        {!mine && <span className="absolute -right-1.5 top-1/2 h-1.5 w-1.5 -translate-y-1/2 rounded-full bg-[#FA5151]" />}
      </button>
    );
  }
  if (message.kind === "file") {
    return (
      <button
        onClick={() => onNotify(`正在打开 ${message.title}`)}
        className="group relative w-[258px] rounded-[7px] bg-white p-3 text-left shadow-[0_2px_8px_rgba(0,0,0,0.06)] transition hover:shadow-[0_6px_18px_rgba(0,0,0,0.1)]"
      >
        <BubbleTail mine={mine} />
        <div className="flex items-center gap-3">
          <div className="relative grid h-12 w-10 shrink-0 place-items-center overflow-hidden rounded-[5px] bg-gradient-to-br from-[#FF8A8A] to-[#F06060] text-[9px] font-bold text-white shadow-sm">
            <span className="absolute inset-x-0 top-0 h-1/2 bg-gradient-to-b from-white/25 to-transparent" />
            <span className="relative">PDF</span>
          </div>
          <div className="min-w-0 flex-1">
            <div className="truncate text-[12.5px] font-medium text-[#222]">{message.title}</div>
            <div className="mt-1 text-[10px] tabular-nums text-[#999]">{message.size}</div>
            <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-[#F0F0F0]">
              <div className="h-full w-full rounded-full bg-gradient-to-r from-[#07C160] to-[#0BD566]" />
            </div>
          </div>
        </div>
        <div className="mt-2.5 flex items-center justify-between border-t border-[#F2F2F2] pt-1.5 text-[9.5px] text-[#A3A3A3]">
          <span>微信电脑版</span>
          <span className="text-[#07A858] opacity-0 transition group-hover:opacity-100">打开 ›</span>
        </div>
      </button>
    );
  }
  if (message.kind === "link") {
    return (
      <button
        onClick={() => onNotify("正在打开链接")}
        className="group relative w-[280px] rounded-[7px] bg-white p-3 text-left shadow-[0_2px_8px_rgba(0,0,0,0.06)] transition hover:shadow-[0_6px_18px_rgba(0,0,0,0.1)]"
      >
        <BubbleTail mine={mine} />
        <div className="mb-2 line-clamp-2 text-[12.5px] font-medium leading-5 text-[#1a1a1a]">{message.title}</div>
        <div className="flex gap-2.5">
          <div className="min-w-0 flex-1">
            <div className="line-clamp-2 text-[10.5px] leading-4 text-[#999]">{message.description}</div>
            <div className="mt-2 flex items-center gap-1 text-[9px] text-[#B7B7B7]">
              <span className="grid h-3.5 w-3.5 place-items-center rounded-[3px] bg-[#07C160] text-[7px] text-white">微</span>
              微信派
            </div>
          </div>
          {message.image && (
            <img src={message.image} alt="" className="h-[46px] w-[46px] rounded-[5px] object-cover shadow-sm" />
          )}
        </div>
      </button>
    );
  }
  if (message.kind === "transfer") {
    return (
      <button onClick={() => onNotify("转账详情 · 待收款")} className="w-[216px] overflow-hidden rounded-[7px] bg-gradient-to-br from-[#F7B733] to-[#F0962E] text-left text-white shadow-[0_4px_14px_rgba(240,150,46,0.35)] transition hover:shadow-[0_8px_22px_rgba(240,150,46,0.45)]">
        <div className="relative flex items-center gap-2.5 p-3">
          <span className="absolute -right-3 -top-3 h-14 w-14 rounded-full border-[8px] border-white/10" />
          <span className="grid h-9 w-9 place-items-center rounded-full bg-white/25 text-lg shadow-inner">¥</span>
          <div>
            <div className="text-[13px] font-medium">转账给 {message.title}</div>
            <div className="mt-0.5 text-[10px] text-white/85">待收款 · 请及时确认</div>
          </div>
        </div>
        <div className="border-t border-white/25 bg-black/[0.06] px-3 py-1.5 text-[9px] text-white/80">微信支付 ›</div>
      </button>
    );
  }
  if (message.kind === "redpacket") {
    return (
      <button onClick={() => onNotify("红包详情 · 恭喜发财")} className="group w-[216px] overflow-hidden rounded-[7px] bg-gradient-to-br from-[#F6A94B] to-[#EE7F2D] text-left text-white shadow-[0_4px_14px_rgba(238,127,45,0.35)] transition hover:shadow-[0_8px_22px_rgba(238,127,45,0.45)]">
        <div className="relative flex items-center gap-2.5 p-3">
          <span className="absolute inset-0 opacity-20" style={{ backgroundImage: "radial-gradient(circle, #fff 0.8px, transparent 0.8px)", backgroundSize: "12px 12px" }} />
          <span className="relative grid h-10 w-10 place-items-center rounded-xl bg-[#D9532B] text-2xl shadow-inner transition group-hover:scale-105">🧧</span>
          <div className="relative">
            <div className="line-clamp-1 text-[13px] font-medium">{message.text || "恭喜发财，大吉大利"}</div>
            <div className="mt-0.5 text-[10px] text-white/85">微信红包 · 待领取</div>
          </div>
        </div>
      </button>
    );
  }
  return (
    <div
      className={cn(
        "relative max-w-[400px] rounded-[5px] px-3 py-[9px] text-[13px] leading-[1.6] text-[#1a1a1a]",
        mine
          ? "bg-[#95EC69] shadow-[0_1px_2px_rgba(110,160,60,0.25)]"
          : "bg-white shadow-[0_1px_3px_rgba(0,0,0,0.07)]",
      )}
    >
      <BubbleTail mine={mine} />
      <span className="whitespace-pre-wrap break-words">{message.text}</span>
    </div>
  );
}

export function ChatView({
  chat,
  messages,
  onSend,
  onNotify,
}: {
  chat: Chat;
  messages: Message[];
  onSend: (chatId: string, message: Message) => void;
  onNotify: (text: string) => void;
}) {
  const [draft, setDraft] = useState("");
  const [showEmoji, setShowEmoji] = useState(false);
  const [showInfo, setShowInfo] = useState(false);
  const [call, setCall] = useState<null | "video" | "voice">(null);
  const [callSecs, setCallSecs] = useState(0);
  const scrollRef = useRef<HTMLDivElement>(null);
  const messagesForChat = useMemo(() => messages, [messages]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messagesForChat, chat.id]);

  useEffect(() => {
    if (!call) { setCallSecs(0); return; }
    const t = setInterval(() => setCallSecs((s) => s + 1), 1000);
    return () => clearInterval(t);
  }, [call]);

  const send = () => {
    if (!draft.trim()) return;
    onSend(chat.id, {
      id: `${Date.now()}`,
      side: "me",
      kind: "text",
      text: draft.trim(),
      time: "刚刚",
    });
    setDraft("");
    setShowEmoji(false);
  };

  const formatCallTime = `${String(Math.floor(callSecs / 60)).padStart(2, "0")}:${String(callSecs % 60).padStart(2, "0")}`;

  const subtitle =
    chat.type === "group" ? "4 位成员" : chat.id === "file" ? "与手机互传文件 · 已连接" : chat.id === "wechat" ? "官方服务号 · 已认证" : chat.online ? "在线" : "微信用户";

  return (
    <section className="relative flex min-w-0 flex-1 flex-col bg-[#F5F5F5]">
      <header className="flex h-[58px] shrink-0 items-center justify-between border-b border-black/[0.06] bg-[#F7F7F7]/95 px-5 backdrop-blur">
        <div className="flex min-w-0 items-center gap-3">
          {chat.avatars ? (
            <GroupAvatar images={chat.avatars} className="h-9 w-9 rounded-[7px]" />
          ) : chat.avatar ? (
            <Avatar src={chat.avatar} name={chat.name} className="h-9 w-9 rounded-[7px]" online={chat.online} />
          ) : null}
          <div className="min-w-0">
            <div className="flex items-center gap-1.5">
              <h3 className="truncate text-[14px] font-semibold text-[#1d1d1d]">{chat.name}</h3>
              {chat.type === "group" && (
                <span className="rounded bg-[#ECECEC] px-1 py-0.5 text-[8.5px] tabular-nums text-[#888]">4</span>
              )}
              {chat.muted && <span className="text-[9px] text-[#bbb]">免打扰</span>}
            </div>
            <p className="mt-0.5 flex items-center gap-1 text-[10px] text-[#9a9a9a]">
              {chat.online && chat.type === "direct" && <span className="h-1.5 w-1.5 rounded-full bg-[#07C160]" />}
              {subtitle}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-1 text-[#5a5a5a]">
          {[
            { icon: Phone, label: "语音通话", fn: () => setCall("voice") },
            { icon: Video, label: "视频通话", fn: () => setCall("video") },
          ].map((b) => (
            <button key={b.label} onClick={b.fn} title={b.label} className="group relative grid h-8 w-8 place-items-center rounded-lg transition hover:bg-[#E7E7E7]">
              <b.icon size={17} strokeWidth={1.8} />
              <span className="pointer-events-none absolute -bottom-8 left-1/2 z-30 -translate-x-1/2 rounded-md bg-[#222] px-2 py-1 text-[9px] whitespace-nowrap text-white opacity-0 shadow-lg transition group-hover:opacity-100">{b.label}</span>
            </button>
          ))}
          <span className="mx-0.5 h-4 w-px bg-[#E0E0E0]" />
          <button
            onClick={() => setShowInfo((v) => !v)}
            className={cn("grid h-8 w-8 place-items-center rounded-lg transition hover:bg-[#E7E7E7]", showInfo && "bg-[#E2E2E2] text-[#07A858]")}
            title="聊天信息"
          >
            <Info size={17} strokeWidth={1.8} />
          </button>
          <button onClick={() => onNotify("更多操作：消息免打扰、置顶聊天、清空记录")} className="grid h-8 w-8 place-items-center rounded-lg transition hover:bg-[#E7E7E7]" title="更多">
            <MoreHorizontal size={17} />
          </button>
        </div>
      </header>

      <div className="flex min-h-0 flex-1">
        <div className="flex min-w-0 flex-1 flex-col">
          <div ref={scrollRef} className="chat-canvas scrollbar-thin min-h-0 flex-1 overflow-y-auto px-6 py-5">
            <div className="mx-auto flex w-full max-w-[640px] flex-col gap-[14px]">
              {messagesForChat.map((message) => {
                if (message.side === "system") {
                  return <SystemPill key={message.id}>{message.text}</SystemPill>;
                }
                const mine = message.side === "me";
                const sender = message.sender?.replace(/\s/g, "");
                const senderAvatar = sender
                  ? contacts.find((c) => c.name.replace(/\s/g, "") === sender || c.name.replace(/\s/g, "").startsWith(sender))?.avatar
                  : undefined;
                return (
                  <div key={message.id} className={cn("group/msg flex items-start gap-2.5", mine && "flex-row-reverse")}>
                    <button onClick={() => onNotify(mine ? "我的资料" : `查看 ${message.sender || chat.name} 的资料`)}>
                      <Avatar
                        src={mine ? me.avatar : senderAvatar || chat.avatar}
                        className="mt-0.5 h-[36px] w-[36px] rounded-[6px] transition group-hover/msg:shadow-md"
                        name={mine ? me.name : message.sender || chat.name}
                      />
                    </button>
                    <div className={cn("flex min-w-0 max-w-[75%] flex-col gap-1", mine ? "items-end" : "items-start")}>
                      {chat.type === "group" && !mine && message.sender && (
                        <span className="px-1 text-[10px] text-[#9a9a9a]">{message.sender}</span>
                      )}
                      <MessageContent message={message} onNotify={onNotify} />
                      {mine && message.kind === "text" && (
                        <span className="flex items-center gap-1 px-1 text-[8.5px] text-[#bbb]">
                          {message.time || "14:32"} <CheckCheck size={11} className="text-[#07C160]" />
                        </span>
                      )}
                    </div>
                  </div>
                );
              })}
              <div className="h-1" />
            </div>
          </div>

          <div className="shrink-0 border-t border-black/[0.06] bg-[#F7F7F7]">
            <div className="flex h-[38px] items-center gap-1 px-4 text-[#5f5f5f]">
              {[
                { icon: Smile, label: "表情", fn: () => setShowEmoji((v) => !v), active: showEmoji },
                { icon: Scissors, label: "截图 (⇧⌘A)", fn: () => onNotify("截图功能已就绪，可捕捉当前窗口") },
                { icon: Folder, label: "发送文件（支持拖拽）", fn: () => onNotify("已打开文件选择器 · 支持拖拽发送") },
                { icon: Search, label: "聊天记录", fn: () => onNotify("聊天记录已按时间整理") },
              ].map((b) => (
                <button key={b.label} onClick={b.fn} title={b.label} className={cn("grid h-8 w-8 place-items-center rounded-lg transition hover:bg-[#E7E7E7] hover:text-[#07A858]", b.active && "bg-[#E2E2E2] text-[#07A858]")}>
                  <b.icon size={18} strokeWidth={1.7} />
                </button>
              ))}
              <span className="ml-auto hidden items-center gap-1.5 text-[9.5px] text-[#aaa] lg:flex">
                <kbd className="rounded border border-[#E0E0E0] bg-white px-1.5 py-0.5 text-[8px]">Enter</kbd> 发送
                <kbd className="rounded border border-[#E0E0E0] bg-white px-1.5 py-0.5 text-[8px]">⇧+Enter</kbd> 换行
              </span>
            </div>

            {showEmoji && (
              <div className="pop-in mx-4 mb-2 overflow-hidden rounded-xl border border-black/[0.06] bg-white shadow-[0_8px_28px_rgba(0,0,0,0.1)]">
                <div className="flex items-center justify-between border-b border-[#F2F2F2] px-3 py-2">
                  <div className="flex gap-1">
                    {["最近", "收藏", "热门"].map((t, i) => (
                      <span key={t} className={cn("rounded-full px-2 py-0.5 text-[9px]", i === 0 ? "bg-[#07C160] font-medium text-white" : "text-[#999]")}>{t}</span>
                    ))}
                  </div>
                  <span className="text-[9px] text-[#bbb]">点击添加到输入框</span>
                </div>
                <div className="grid grid-cols-10 gap-0.5 p-2">
                  {stickerSets.flat().slice(0, 30).map((emoji, i) => (
                    <button key={`${emoji}-${i}`} className="grid h-9 place-items-center rounded-lg text-[22px] transition hover:scale-125 hover:bg-[#F5F5F5]" onClick={() => setDraft((d) => d + emoji)}>
                      {emoji}
                    </button>
                  ))}
                </div>
              </div>
            )}

            <div className="flex items-end gap-3 px-4 pb-3.5 pt-1">
              <div className="relative min-h-[64px] flex-1 rounded-[10px] bg-white px-3 py-2 shadow-[0_1px_3px_rgba(0,0,0,0.04)] ring-1 ring-black/[0.04] transition focus-within:ring-[#07C160]/40 focus-within:shadow-[0_2px_10px_rgba(7,193,96,0.1)]">
                <textarea
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      send();
                    }
                  }}
                  rows={2}
                  placeholder="输入消息…"
                  className="max-h-[110px] min-h-[44px] w-full resize-none bg-transparent text-[13px] leading-6 text-[#222] outline-none placeholder:text-[#BEBEBE]"
                />
                <span className="pointer-events-none absolute right-2.5 bottom-1.5 text-[8.5px] tabular-nums text-[#ccc]">{draft.length}/500</span>
              </div>
              <button
                onClick={send}
                disabled={!draft.trim()}
                className={cn(
                  "pressable mb-0.5 flex h-[34px] items-center gap-1.5 rounded-[9px] px-4 text-[12px] font-medium transition",
                  draft.trim()
                    ? "bg-gradient-to-b from-[#0BD566] to-[#06B058] text-white shadow-[0_4px_14px_rgba(7,193,96,0.35)] hover:shadow-[0_6px_18px_rgba(7,193,96,0.45)]"
                    : "bg-[#E8E8E8] text-[#B5B5B5]",
                )}
              >
                发送
              </button>
            </div>
          </div>
        </div>

        {showInfo && (
          <aside className="screen-enter-soft w-[224px] shrink-0 overflow-y-auto scrollbar-thin border-l border-black/[0.06] bg-[#F7F7F7] px-4 py-5">
            <div className="flex flex-col items-center">
              {chat.avatars ? (
                <GroupAvatar images={chat.avatars} className="h-15 w-15 rounded-xl" />
              ) : (
                <Avatar src={chat.avatar} name={chat.name} className="h-[60px] w-[60px] rounded-xl" />
              )}
              <div className="mt-2.5 text-center text-[13px] font-semibold text-[#222]">{chat.name}</div>
              <div className="mt-1 text-center text-[9.5px] text-[#9a9a9a]">
                {chat.type === "group" ? "春日设计小组 · 4 人" : chat.id === "file" ? "文件传输助手" : "微信号：wxid_suhe2026"}
              </div>
              {chat.online && <span className="mt-1.5 flex items-center gap-1 rounded-full bg-[#E9F8EF] px-2 py-0.5 text-[8.5px] text-[#07A858]"><span className="h-1 w-1 rounded-full bg-[#07C160]" /> 在线</span>}
            </div>
            <div className="mt-4 flex justify-around border-b border-black/[0.05] pb-4">
              {[
                { icon: ImageIcon, label: "图片 24" },
                { icon: Folder, label: "文件 8" },
                { icon: Search, label: "搜索" },
              ].map((item) => (
                <button key={item.label} onClick={() => onNotify(`${item.label}记录已同步`)} className="group flex flex-col items-center gap-1.5 text-[#777]">
                  <span className="grid h-9 w-9 place-items-center rounded-xl bg-white shadow-sm ring-1 ring-black/[0.03] transition group-hover:text-[#07A858] group-hover:ring-[#07C160]/30">
                    <item.icon size={16} strokeWidth={1.7} />
                  </span>
                  <span className="text-[9px]">{item.label}</span>
                </button>
              ))}
            </div>
            <div className="space-y-3.5 border-b border-black/[0.05] py-4">
              {[
                { label: "消息免打扰", on: chat.muted || false, desc: "接收但不提醒" },
                { label: "置顶聊天", on: chat.pinned || false, desc: "保持在列表顶部" },
              ].map((row) => (
                <div key={row.label} className="flex items-center justify-between">
                  <div>
                    <div className="text-[11px] text-[#333]">{row.label}</div>
                    <div className="mt-0.5 text-[8.5px] text-[#aaa]">{row.desc}</div>
                  </div>
                  <Switch small on={row.on} onChange={() => onNotify(`${row.label}设置已更新`)} />
                </div>
              ))}
            </div>
            <div className="pt-4">
              <div className="mb-2.5 flex items-center justify-between text-[10px] text-[#8e8e8e]">
                <span className="font-medium">聊天文件</span>
                <button onClick={() => onNotify("已打开全部聊天文件")} className="text-[#576B95] hover:underline">全部 ›</button>
              </div>
              <div className="grid grid-cols-3 gap-1.5">
                {[assetsGallery, assetsDesk, assetsTulips].map((img, i) => (
                  <button key={i} onClick={() => onNotify("图片预览")} className="group relative overflow-hidden rounded-md">
                    <img src={img} alt="聊天文件" className="aspect-square w-full object-cover transition duration-300 group-hover:scale-110" loading="lazy" />
                  </button>
                ))}
              </div>
            </div>
            <button onClick={() => onNotify("已打开聊天设置")} className="mt-5 flex w-full items-center justify-center gap-1.5 rounded-[9px] border border-black/[0.06] bg-white py-2 text-[10px] text-[#666] shadow-sm transition hover:bg-[#FAFAFA]">
              <Bell size={11} /> 聊天设置
            </button>
          </aside>
        )}
      </div>

      {call && (
        <div className="pop-in absolute inset-0 z-30 overflow-hidden bg-[#141f1a]">
          <img src={call === "video" ? assetsLakeWide : assetsDawnWide} alt="" className="h-full w-full scale-105 object-cover opacity-50 blur-[3px]" />
          <div className="absolute inset-0 bg-gradient-to-b from-black/50 via-black/15 to-black/75" />
          <div className="absolute inset-x-0 top-0 flex items-center justify-between px-7 py-5 text-white">
            <div className="flex items-center gap-2.5">
              <span className="flex items-center gap-1.5 rounded-full bg-black/30 px-3 py-1.5 text-[10px] backdrop-blur">
                <span className="relative flex h-1.5 w-1.5">
                  <span className="absolute h-full w-full animate-ping rounded-full bg-[#07C160] opacity-60" />
                  <span className="relative h-1.5 w-1.5 rounded-full bg-[#07C160]" />
                </span>
                {call === "video" ? "视频通话" : "语音通话"} · {formatCallTime}
              </span>
              <span className="text-[9px] text-white/50">端到端加密</span>
            </div>
            <button onClick={() => setCall(null)} className="grid h-8 w-8 place-items-center rounded-full bg-black/30 text-white backdrop-blur transition hover:bg-black/50">
              <X size={15} />
            </button>
          </div>
          <div className="absolute inset-0 flex flex-col items-center justify-center pb-10">
            <div className="relative">
              <span className="absolute inset-0 rounded-2xl bg-[#07C160]/30" style={{ animation: "ringPulse 2s ease-out infinite" }} />
              <Avatar src={chat.avatar} name={chat.name} className="relative h-[88px] w-[88px] rounded-2xl shadow-2xl ring-2 ring-white/20" />
            </div>
            <div className="mt-4 text-[18px] font-medium text-white">{chat.name}</div>
            <div className="mt-1.5 flex items-center gap-1.5 text-[11px] text-white/65">
              <span className="flex gap-[3px]">
                <span className="typing-dot h-1.5 w-1.5 rounded-full bg-[#0BD566]" />
                <span className="typing-dot h-1.5 w-1.5 rounded-full bg-[#0BD566]" />
                <span className="typing-dot h-1.5 w-1.5 rounded-full bg-[#0BD566]" />
              </span>
              正在等待对方接受邀请...
            </div>
          </div>
          <div className="absolute bottom-7 left-1/2 flex -translate-x-1/2 items-end gap-5">
            {[
              { icon: Mic, label: "静音", fn: () => onNotify("麦克风已静音") },
              { icon: null, label: "挂断", fn: () => setCall(null), hangup: true },
              { icon: Video, label: call === "video" ? "关闭摄像头" : "切换视频", fn: () => setCall(call === "video" ? "voice" : "video") },
            ].map((b) => (
              <div key={b.label} className="flex flex-col items-center gap-1.5">
                <button
                  onClick={b.fn}
                  className={cn(
                    "pressable grid place-items-center rounded-full shadow-xl backdrop-blur transition",
                    b.hangup ? "h-[54px] w-[54px] bg-gradient-to-b from-[#FB5E5E] to-[#E84040] text-white shadow-[0_8px_24px_rgba(250,81,81,0.45)]" : "h-12 w-12 bg-white/15 text-white hover:bg-white/25",
                  )}
                >
                  {b.hangup ? <Phone size={22} className="rotate-[135deg]" /> : b.icon && <b.icon size={20} />}
                </button>
                <span className="text-[9px] text-white/60">{b.label}</span>
              </div>
            ))}
          </div>
          <div className="absolute bottom-7 right-6 h-[120px] w-[86px] overflow-hidden rounded-xl border border-white/25 bg-black/30 shadow-2xl">
            <Avatar src={me.avatar} className="h-full w-full !rounded-xl" name="我" />
            <span className="absolute bottom-1.5 left-2 rounded bg-black/40 px-1.5 py-0.5 text-[8px] text-white/85 backdrop-blur">我</span>
          </div>
        </div>
      )}
    </section>
  );
}

const assetsGallery = "https://images.pexels.com/photos/35436850/pexels-photo-35436850.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=300&h=300";
const assetsDesk = "https://images.pexels.com/photos/2842457/pexels-photo-2842457.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=300&h=300";
const assetsTulips = "https://images.pexels.com/photos/35983785/pexels-photo-35983785.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=300&h=300";
const assetsLakeWide = "https://images.pexels.com/photos/4084851/pexels-photo-4084851.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=900&h=700";
const assetsDawnWide = "https://images.pexels.com/photos/28593673/pexels-photo-28593673.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=900&h=700";

export { assetsDesk, assetsGallery, assetsLakeWide, assetsTulips };
