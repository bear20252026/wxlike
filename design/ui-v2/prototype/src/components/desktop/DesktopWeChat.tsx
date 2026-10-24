import { useEffect, useMemo, useState } from "react";
import {
  ChevronRight,
  Folder,
  Image as ImageIcon,
  LayoutGrid,
  MessageCircle,
  Plus,
  QrCode,
  Search,
  Settings,
  Smartphone,
  Star,
  Users,
  Video,
  Wallet,
  X,
} from "lucide-react";
import { chats, contacts, conversations, me, moments, type Chat, type Message, type Person } from "../../data";
import { Avatar, GroupAvatar, MutedIcon, WeChatBadge, WeChatFileIcon, WeChatLogo } from "../shared";
import { cn } from "../../utils/cn";
import { ChatView } from "./ChatView";
import { ContactsPage, FavoritesPage, FilesPage } from "./PanelsA";
import { ChannelsPage, DesktopLoginPage, MiniProgramsPage, MomentsPage, SettingsPage, WalletPage } from "./PanelsB";

export type DesktopPage =
  | "chats"
  | "contacts"
  | "favorites"
  | "files"
  | "moments"
  | "channels"
  | "mini"
  | "wallet"
  | "login"
  | "settings";

const railItems: { id: DesktopPage; label: string; icon: typeof MessageCircle; kbd: string }[] = [
  { id: "chats", label: "聊天", icon: MessageCircle, kbd: "1" },
  { id: "contacts", label: "通讯录", icon: Users, kbd: "2" },
  { id: "favorites", label: "收藏", icon: Star, kbd: "3" },
  { id: "files", label: "文件", icon: Folder, kbd: "4" },
  { id: "moments", label: "朋友圈", icon: ImageIcon, kbd: "5" },
  { id: "channels", label: "视频号", icon: Video, kbd: "6" },
  { id: "mini", label: "小程序", icon: LayoutGrid, kbd: "7" },
  { id: "wallet", label: "支付", icon: Wallet, kbd: "8" },
];

function ChatListItem({ chat, active, onClick }: { chat: Chat; active: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "group relative flex w-full items-center gap-2.5 px-4 py-[10px] text-left transition-colors duration-150",
        active ? "bg-[#DFDFDF]" : chat.pinned ? "bg-[#EFEFEF] hover:bg-[#E7E7E7]" : "hover:bg-[#EBEBEB]",
      )}
    >
      {active && <span className="absolute left-0 top-1/2 h-8 w-[3px] -translate-y-1/2 rounded-r-full bg-[#07C160]" />}
      {chat.avatars ? (
        <GroupAvatar images={chat.avatars} className="h-[40px] w-[40px] rounded-[6px]" />
      ) : chat.id === "file" ? (
        <WeChatFileIcon className="h-[40px] w-[40px] rounded-[6px]" />
      ) : chat.icon === "wechat" ? (
        <span className="grid h-[40px] w-[40px] shrink-0 place-items-center rounded-[6px] bg-gradient-to-br from-[#0BD566] to-[#06A854] shadow-sm">
          <WeChatLogo className="h-7 w-7 !drop-shadow-none" />
        </span>
      ) : (
        <Avatar src={chat.avatar} name={chat.name} className="h-[40px] w-[40px] rounded-[6px]" online={chat.online} />
      )}
      <span className="min-w-0 flex-1">
        <span className="flex items-baseline justify-between gap-1.5">
          <span className="truncate text-[12.5px] font-medium text-[#262626]">{chat.name}</span>
          <span className="shrink-0 text-[9px] tabular-nums text-[#A8A8A8]">{chat.time}</span>
        </span>
        <span className="mt-[4px] flex items-center justify-between gap-1.5">
          <span className="flex min-w-0 flex-1 items-center truncate text-[10px] text-[#9c9c9c]">
            {chat.muted && <MutedIcon size={10} className="mr-1 shrink-0 text-[#BFBFBF]" />}
            <span className="truncate">{chat.last}</span>
          </span>
          <span className="flex shrink-0 items-center gap-1">
            {chat.muted && !chat.unread && <MutedIcon size={10} className="text-[#C6C6C6]" />}
            {chat.unread ? <WeChatBadge count={chat.unread} /> : null}
          </span>
        </span>
      </span>
    </button>
  );
}

export function DesktopWeChat({
  page,
  onPageChange,
  onNotify,
}: {
  page: DesktopPage;
  onPageChange: (page: DesktopPage) => void;
  onNotify: (text: string) => void;
}) {
  const [selectedChatId, setSelectedChatId] = useState("lin");
  const [messages, setMessages] = useState<Record<string, Message[]>>(conversations);
  const [query, setQuery] = useState("");
  const [globalSearch, setGlobalSearch] = useState(false);
  const [searchTerm, setSearchTerm] = useState("");
  const [plusMenu, setPlusMenu] = useState(false);
  const [ephemeral, setEphemeral] = useState<Person | null>(null);
  const [filter, setFilter] = useState("全部");

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setGlobalSearch(true);
      }
      if (e.key === "Escape") {
        setGlobalSearch(false);
        setPlusMenu(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const activeChat = useMemo(() => {
    const found = chats.find((c) => c.id === selectedChatId);
    if (found) return found;
    if (ephemeral?.id === selectedChatId) {
      return {
        id: ephemeral.id,
        name: ephemeral.name,
        avatar: ephemeral.avatar,
        type: "direct" as const,
        last: "",
        time: "",
        online: ephemeral.online,
      };
    }
    return chats[0];
  }, [selectedChatId, ephemeral]);

  const filteredChats = useMemo(() => {
    let list = chats.filter(
      (c) => c.name.toLowerCase().includes(query.toLowerCase()) || c.last.toLowerCase().includes(query.toLowerCase()),
    );
    if (filter === "未读") list = list.filter((c) => c.unread);
    if (filter === "群聊") list = list.filter((c) => c.type === "group");
    return [...list].sort((a, b) => Number(Boolean(b.pinned)) - Number(Boolean(a.pinned)));
  }, [query, filter]);

  const openChat = (id: string) => {
    const existing = chats.find((c) => c.id === id);
    if (!existing) {
      const contact = contacts.find((c) => c.id === id);
      if (contact) {
        setEphemeral(contact);
        setMessages((prev) => (prev[id] ? prev : { ...prev, [id]: [{ id: "new-1", side: "system", kind: "system", text: "开始新的对话" }] }));
      }
    }
    setSelectedChatId(id);
    onPageChange("chats");
    setGlobalSearch(false);
    setPlusMenu(false);
  };

  const sendMessage = (chatId: string, message: Message) => {
    setMessages((prev) => ({ ...prev, [chatId]: [...(prev[chatId] || []), message] }));
  };

  const searchResults = useMemo(() => {
    const q = searchTerm.trim().toLowerCase();
    if (!q) {
      return {
        people: contacts.slice(0, 3),
        conversations: chats.slice(0, 3),
        moments: moments.slice(0, 2),
      };
    }
    return {
      people: contacts.filter((p) => p.name.toLowerCase().includes(q) || p.note?.toLowerCase().includes(q)).slice(0, 4),
      conversations: chats.filter((c) => c.name.toLowerCase().includes(q) || c.last.toLowerCase().includes(q)).slice(0, 4),
      moments: moments.filter((m) => m.text.toLowerCase().includes(q)).slice(0, 2),
    };
  }, [searchTerm]);

  const unreadTotal = chats.reduce((n, c) => n + (c.unread || 0), 0);

  return (
    <div className="relative flex h-[648px] min-w-[680px] flex-col overflow-hidden rounded-[14px] border border-black/[0.12] bg-[#F7F7F7] shadow-[0_36px_90px_-24px_rgba(24,30,26,0.5),0_2px_8px_rgba(0,0,0,0.12)]">
      {/* macOS title bar */}
      <div className="flex h-[34px] shrink-0 items-center gap-2 border-b border-black/[0.07] bg-gradient-to-b from-[#F0F0F0] to-[#E8E8E8] px-3.5">
        <div className="group flex items-center gap-[7px]">
          <button onClick={() => onNotify("窗口已关闭（演示）")} className="grid h-[11px] w-[11px] place-items-center rounded-full bg-[#FF5F57] shadow-[inset_0_0_2px_rgba(0,0,0,0.15)]"><X size={7} className="text-black/50 opacity-0 transition group-hover:opacity-100" strokeWidth={3} /></button>
          <button onClick={() => onNotify("窗口已最小化")} className="grid h-[11px] w-[11px] place-items-center rounded-full bg-[#FEBC2E] shadow-[inset_0_0_2px_rgba(0,0,0,0.15)]"><span className="h-[1px] w-[7px] bg-black/50 opacity-0 transition group-hover:opacity-100" /></button>
          <button onClick={() => onNotify("窗口已最大化")} className="grid h-[11px] w-[11px] place-items-center rounded-full bg-[#28C840] shadow-[inset_0_0_2px_rgba(0,0,0,0.15)]"><Plus size={7} className="text-black/50 opacity-0 transition group-hover:opacity-100" strokeWidth={3} /></button>
        </div>
        <div className="mx-auto flex items-center gap-1.5 text-[10px] text-[#7a7a7a]">
          <WeChatLogo className="h-3.5 w-3.5 !drop-shadow-none" />
          <span className="font-medium">微信</span>
          <ChevronRight size={9} className="text-[#B8B8B8]" />
          <span>苏禾的微信</span>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => setGlobalSearch(true)}
            className="flex items-center gap-1.5 rounded-md border border-black/[0.06] bg-white/70 px-2 py-[3px] text-[9px] text-[#999] transition hover:bg-white"
          >
            <Search size={10} /> 搜索 <kbd className="rounded bg-[#EFEFEF] px-1 text-[8px] text-[#999]">⌘K</kbd>
          </button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* rail */}
        <aside className="flex w-[62px] shrink-0 flex-col items-center bg-gradient-to-b from-[#2b2b2b] to-[#262626] py-3.5">
          <button onClick={() => onPageChange("settings")} className="group relative mb-3.5" title="苏禾 · 已登录">
            <Avatar src={me.avatar} name={me.name} className="h-[36px] w-[36px] rounded-[8px] ring-1 ring-white/15 transition group-hover:ring-white/30" online />
          </button>
          <div className="flex w-full flex-col items-center gap-[3px]">
            {railItems.map((item) => {
              const active = page === item.id;
              return (
                <button
                  key={item.id}
                  onClick={() => onPageChange(item.id)}
                  className={cn(
                    "group relative grid h-10 w-10 place-items-center rounded-[11px] transition-all duration-200",
                    active ? "bg-white/[0.14] text-[#0BD566] shadow-[inset_0_1px_0_rgba(255,255,255,0.08)]" : "text-[#8f8f8f] hover:bg-white/[0.07] hover:text-[#E5E5E5]",
                  )}
                >
                  {active && <span className="absolute -left-[11px] h-5 w-[3px] rounded-r-full bg-[#07C160] shadow-[0_0_8px_rgba(7,193,96,0.6)]" />}
                  <item.icon size={19} strokeWidth={active ? 1.9 : 1.65} />
                  {item.id === "chats" && unreadTotal > 0 && (
                    <span className="absolute right-[6px] top-[7px] grid h-[15px] min-w-[15px] place-items-center rounded-full bg-[#FA5151] px-[3px] text-[8px] font-bold text-white ring-2 ring-[#282828]">
                      {unreadTotal}
                    </span>
                  )}
                  <span className="pointer-events-none absolute left-[50px] z-30 flex items-center gap-1.5 rounded-lg border border-white/10 bg-[#171717]/95 px-2.5 py-1.5 text-[10px] whitespace-nowrap text-white opacity-0 shadow-xl backdrop-blur transition-all duration-150 group-hover:translate-x-0 group-hover:opacity-100 translate-x-[-4px]">
                    {item.label}
                    <kbd className="rounded bg-white/10 px-1 text-[8px] text-white/60">{item.kbd}</kbd>
                  </span>
                </button>
              );
            })}
          </div>
          <div className="mt-auto flex flex-col items-center gap-[3px]">
            <span className="mb-1 h-px w-8 bg-white/10" />
            <button onClick={() => onNotify("手机端已同步 · 消息实时互传")} title="已连接手机" className="group relative grid h-10 w-10 place-items-center rounded-[11px] text-[#8f8f8f] transition hover:bg-white/[0.07] hover:text-[#E5E5E5]">
              <Smartphone size={18} strokeWidth={1.65} />
              <span className="absolute right-[9px] top-[9px] h-[7px] w-[7px] rounded-full border-[1.5px] border-[#262626] bg-[#07C160]" />
            </button>
            <button onClick={() => onPageChange("settings")} title="设置" className={cn("grid h-10 w-10 place-items-center rounded-[11px] transition", page === "settings" ? "bg-white/[0.14] text-[#0BD566]" : "text-[#8f8f8f] hover:bg-white/[0.07] hover:text-[#E5E5E5]")}>
              <Settings size={18} strokeWidth={1.65} />
            </button>
          </div>
        </aside>

        {page === "chats" && (
          <div className="screen-enter-soft flex min-h-0 min-w-0 flex-1">
            <aside className="flex w-[264px] shrink-0 flex-col border-r border-black/[0.06] bg-[#F7F7F7]">
              <div className="shrink-0 px-4 pb-2 pt-3">
                <div className="flex h-7 items-center justify-between">
                  <h2 className="text-[16px] font-semibold tracking-[0.02em] text-[#1f1f1f]">微信</h2>
                  <div className="relative flex items-center gap-1 text-[#555]">
                    <button onClick={() => setGlobalSearch(true)} title="搜索 (⌘K)" className="grid h-7 w-7 place-items-center rounded-md transition hover:bg-[#E5E5E5]"><Search size={15} /></button>
                    <button onClick={() => setPlusMenu((v) => !v)} title="新建" className={cn("grid h-7 w-7 place-items-center rounded-md transition hover:bg-[#E5E5E5]", plusMenu && "bg-[#E0E0E0]")}><Plus size={17} /></button>
                    {plusMenu && (
                      <>
                        <div className="fixed inset-0 z-30" onClick={() => setPlusMenu(false)} />
                        <div className="pop-in absolute -right-1 top-8 z-40 w-[168px] overflow-hidden rounded-xl border border-black/[0.08] bg-white/98 py-1.5 shadow-[0_16px_44px_rgba(0,0,0,0.18)] backdrop-blur">
                          {[
                            ["发起群聊", Users, "⌘N"],
                            ["添加朋友", Plus, "⌘F"],
                            ["扫一扫", QrCode, "⌘S"],
                            ["收付款", Wallet, "⌘P"],
                          ].map(([label, Icon, kbd]) => {
                            const I = Icon as typeof Users;
                            return (
                              <button
                                key={label as string}
                                onClick={() => {
                                  setPlusMenu(false);
                                  if (label === "发起群聊") { onNotify("已选择 2 位联系人，正在创建群聊"); openChat("spring"); }
                                  else if (label === "扫一扫") onNotify("扫一扫窗口已打开");
                                  else if (label === "收付款") onPageChange("wallet");
                                  else onNotify("添加朋友：搜索微信号 / 手机号");
                                }}
                                className="flex w-full items-center gap-2.5 px-3 py-2 text-[11px] text-[#3a3a3a] transition hover:bg-[#F3F3F3]"
                              >
                                <span className="grid h-6 w-6 place-items-center rounded-md bg-[#F2F2F2] text-[#666]"><I size={13} /></span>
                                {label as string}
                                <kbd className="ml-auto text-[8px] text-[#bbb]">{kbd as string}</kbd>
                              </button>
                            );
                          })}
                        </div>
                      </>
                    )}
                  </div>
                </div>
                <div className="mt-2 flex h-[30px] items-center gap-1.5 rounded-[8px] bg-[#E8E8E8] px-2.5 ring-1 ring-transparent transition focus-within:bg-white focus-within:ring-[#07C160]/40 focus-within:shadow-[0_2px_8px_rgba(7,193,96,0.1)]">
                  <Search size={12} className="shrink-0 text-[#A2A2A2]" />
                  <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="搜索" className="w-full bg-transparent text-[11px] text-[#333] outline-none placeholder:text-[#A6A6A6]" />
                  {query ? <button onClick={() => setQuery("")} className="grid h-4 w-4 place-items-center rounded-full bg-[#C8C8C8] text-white"><X size={9} strokeWidth={3} /></button> : <kbd className="shrink-0 text-[8px] text-[#bbb]">⌘K</kbd>}
                </div>
              </div>
              <div className="flex min-h-0 flex-1 flex-col overflow-y-auto scrollbar-thin">
                <div className="flex items-center gap-1 border-b border-black/[0.05] px-4 pb-2 pt-1">
                  {["全部", "未读", "群聊"].map((t) => (
                    <button key={t} onClick={() => setFilter(t)} className={cn("rounded-full px-2.5 py-1 text-[9px] transition", filter === t ? "bg-[#1f1f1f] font-medium text-white" : "text-[#A6A6A6] hover:bg-[#ECECEC]")}>
                      {t}{t === "未读" && unreadTotal > 0 && <span className="ml-1 tabular-nums">{unreadTotal}</span>}
                    </button>
                  ))}
                </div>
                {filteredChats.map((chat) => (
                  <ChatListItem key={chat.id} chat={chat} active={chat.id === activeChat.id} onClick={() => { setEphemeral(null); setSelectedChatId(chat.id); }} />
                ))}
                {filteredChats.length === 0 && (
                  <div className="flex flex-col items-center px-8 py-12 text-center">
                    <span className="grid h-11 w-11 place-items-center rounded-xl bg-[#EFEFEF] text-[#aaa]"><Search size={18} /></span>
                    <div className="mt-2.5 text-[11px] text-[#888]">没有匹配的聊天</div>
                    <div className="mt-1 text-[9px] text-[#bbb]">换个关键词试试</div>
                  </div>
                )}
              </div>
              <div className="flex h-8 shrink-0 items-center justify-between border-t border-black/[0.06] bg-[#F2F2F2] px-3.5 text-[9px] text-[#9a9a9a]">
                <span className="tabular-nums">{unreadTotal} 条未读</span>
                <button onClick={() => onNotify("所有聊天已标记为已读")} className="rounded px-1.5 py-0.5 transition hover:bg-[#E5E5E5] hover:text-[#666]">全部已读</button>
              </div>
            </aside>
            <ChatView chat={activeChat} messages={messages[activeChat.id] || []} onSend={sendMessage} onNotify={onNotify} />
          </div>
        )}

        {page === "contacts" && <ContactsPage notify={onNotify} onOpenChat={openChat} />}
        {page === "favorites" && <FavoritesPage notify={onNotify} />}
        {page === "files" && <FilesPage notify={onNotify} />}
        {page === "moments" && <MomentsPage notify={onNotify} />}
        {page === "channels" && <ChannelsPage notify={onNotify} />}
        {page === "mini" && <MiniProgramsPage notify={onNotify} />}
        {page === "wallet" && <WalletPage notify={onNotify} />}
        {page === "login" && <DesktopLoginPage notify={onNotify} onDone={() => onPageChange("chats")} />}
        {page === "settings" && <SettingsPage notify={onNotify} />}
      </div>

      {globalSearch && (
        <div className="absolute inset-0 z-50 flex items-start justify-center bg-black/30 px-7 pt-[46px] backdrop-blur-[3px]" onClick={() => setGlobalSearch(false)}>
          <div className="pop-in w-full max-w-[540px] overflow-hidden rounded-2xl border border-black/10 bg-[#FBFBFB] shadow-[0_28px_80px_rgba(0,0,0,0.32)]" onClick={(e) => e.stopPropagation()}>
            <div className="flex h-[52px] items-center gap-2.5 border-b border-[#EAEAEA] bg-white px-4">
              <span className="grid h-7 w-7 place-items-center rounded-lg bg-[#F2F2F2]"><Search size={14} className="text-[#666]" /></span>
              <input autoFocus value={searchTerm} onChange={(e) => setSearchTerm(e.target.value)} placeholder="搜索聊天、联系人、朋友圈、收藏…" className="flex-1 bg-transparent text-[13px] text-[#333] outline-none placeholder:text-[#B0B0B0]" />
              <kbd className="rounded-md border border-[#E7E7E7] bg-[#F7F7F7] px-1.5 py-1 text-[8px] text-[#AAA]">ESC</kbd>
            </div>
            <div className="flex gap-1.5 border-b border-[#F0F0F0] bg-[#FAFAFA] px-4 py-2">
              {["综合", "聊天记录", "联系人", "朋友圈", "收藏"].map((t, i) => (
                <button key={t} onClick={() => onNotify(`搜索分类：${t}`)} className={cn("rounded-full px-2.5 py-1 text-[9px] transition", i === 0 ? "bg-[#07C160] font-medium text-white shadow-[0_2px_8px_rgba(7,193,96,0.35)]" : "text-[#888] hover:bg-[#F0F0F0]")}>{t}</button>
              ))}
            </div>
            <div className="scrollbar-thin max-h-[380px] overflow-y-auto p-2">
              <div className="px-2.5 py-1.5 text-[9px] font-semibold tracking-[0.1em] text-[#A6A6A6]">联系人</div>
              {searchResults.people.map((p) => (
                <button key={p.id} onClick={() => openChat(p.id)} className="group flex w-full items-center gap-2.5 rounded-[10px] px-2.5 py-2 text-left transition hover:bg-[#F0F0F0]">
                  <Avatar src={p.avatar} name={p.name} className="h-8 w-8 rounded-[6px]" />
                  <span className="min-w-0 flex-1">
                    <span className="block text-[11px] font-medium text-[#333]">{p.name}</span>
                    <span className="mt-0.5 block truncate text-[9px] text-[#AAA]">{p.note}</span>
                  </span>
                  <ChevronRight size={12} className="text-[#C6C6C6] transition-transform group-hover:translate-x-0.5" />
                </button>
              ))}
              <div className="mt-1 px-2.5 py-1.5 text-[9px] font-semibold tracking-[0.1em] text-[#A6A6A6]">聊天</div>
              {searchResults.conversations.map((c) => (
                <button key={c.id} onClick={() => openChat(c.id)} className="flex w-full items-center gap-2.5 rounded-[10px] px-2.5 py-2 text-left transition hover:bg-[#F0F0F0]">
                  <Avatar src={c.avatar} name={c.name} className="h-8 w-8 rounded-[6px]" />
                  <span className="min-w-0 flex-1"><span className="block text-[11px] font-medium text-[#333]">{c.name}</span><span className="mt-0.5 block truncate text-[9px] text-[#AAA]">{c.last}</span></span>
                  <span className="text-[8.5px] tabular-nums text-[#B0B0B0]">{c.time}</span>
                </button>
              ))}
              <div className="mt-1 px-2.5 py-1.5 text-[9px] font-semibold tracking-[0.1em] text-[#A6A6A6]">朋友圈</div>
              {searchResults.moments.map((m) => (
                <button key={m.id} onClick={() => { onPageChange("moments"); setGlobalSearch(false); }} className="flex w-full items-center gap-2.5 rounded-[10px] px-2.5 py-2 text-left transition hover:bg-[#F0F0F0]">
                  <Avatar src={m.avatar} name={m.name} className="h-8 w-8 rounded-[6px]" />
                  <span className="min-w-0 flex-1"><span className="block text-[10.5px] font-medium text-[#333]">{m.name}</span><span className="mt-0.5 block truncate text-[9px] text-[#AAA]">{m.text}</span></span>
                </button>
              ))}
              {searchResults.people.length + searchResults.conversations.length + searchResults.moments.length === 0 && (
                <div className="flex flex-col items-center py-10">
                  <span className="grid h-11 w-11 place-items-center rounded-xl bg-[#F0F0F0] text-[#bbb]"><Search size={17} /></span>
                  <div className="mt-2.5 text-[10.5px] text-[#999]">没有找到「{searchTerm}」相关内容</div>
                </div>
              )}
            </div>
            <div className="flex items-center justify-between border-t border-[#EDEDED] bg-[#FAFAFA] px-4 py-2.5 text-[8.5px] text-[#B0B0B0]">
              <span className="flex items-center gap-2"><kbd className="rounded border border-[#E5E5E5] bg-white px-1">↑↓</kbd> 选择 <kbd className="rounded border border-[#E5E5E5] bg-white px-1">Enter</kbd> 打开</span>
              <span>搜索范围：全部记录</span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
