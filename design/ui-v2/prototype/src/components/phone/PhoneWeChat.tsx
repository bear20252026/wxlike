import { useMemo, useState } from "react";
import {
  Aperture,
  ChevronRight,
  FileText,
  Gamepad2,
  LayoutGrid,
  MapPin,
  Move,
  Plus,
  QrCode,
  Radio,
  ScanLine,
  Search,
  Settings,
  ShoppingBag,
  Star,
  Users,
  Video,
  Wallet,
  X,
  Eye,
  Camera,
} from "lucide-react";
import { chats, contacts, me, moments } from "../../data";
import { Avatar, GroupAvatar, MutedIcon, PhoneStatusBar, WeChatBadge, WeChatFileIcon, WeChatLogo } from "../shared";
import { cn } from "../../utils/cn";
import { MobileCell, MobileTile, SearchBar, TabBar } from "./atoms";
import type { MobileScreen } from "./types";
import { ChannelsScreen, ChatScreen, MiniProgramsScreen, MomentsScreen, ReadingScreen, ScanScreen, SearchScreen } from "./PhoneScreensA";
import { CallScreen, CardsScreen, FavoritesScreen, NewFriendsScreen, PayScreen, ProfileScreen, RedPacketScreen, SettingsScreen, StickersScreen, WalletScreen } from "./PhoneScreensB";
import { LocationScreen, LoginScreen, MyCodeScreen, OfficialScreen, StatusScreen, TransferScreen } from "./PhoneScreensC";

function HomeHeader({ title, right }: { title: string; right?: React.ReactNode }) {
  return (
    <div className="shrink-0 bg-[#EDEDED]/95 px-3 pt-[32px] backdrop-blur-xl">
      <div className="relative flex h-[42px] items-center justify-center">
        <h1 className="text-[16px] font-semibold tracking-[0.02em] text-[#1a1a1a]">{title}</h1>
        <div className="absolute right-0 flex items-center gap-1 text-[#242424]">{right}</div>
      </div>
    </div>
  );
}

function ChatRow({ chat, onClick }: { chat: (typeof chats)[number]; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "pressable group relative flex w-full items-center gap-3 px-3.5 py-[10px] text-left active:bg-[#EDEDED]",
        chat.pinned ? "bg-[#F1F1F1]" : "bg-white",
      )}
    >
      <div className="relative shrink-0">
        {chat.avatars ? (
          <GroupAvatar images={chat.avatars} className="h-[46px] w-[46px] rounded-[6px]" />
        ) : chat.id === "file" ? (
          <WeChatFileIcon className="h-[46px] w-[46px] rounded-[6px]" />
        ) : chat.icon === "wechat" ? (
          <span className="grid h-[46px] w-[46px] place-items-center rounded-[6px] bg-gradient-to-br from-[#0BD566] to-[#06A854] shadow-sm">
            <WeChatLogo className="h-8 w-8 !drop-shadow-none" />
          </span>
        ) : (
          <Avatar src={chat.avatar} name={chat.name} className="h-[46px] w-[46px] rounded-[6px]" online={chat.online} />
        )}
        {chat.unread ? (
          <span className="absolute -right-1.5 -top-1.5">
            <WeChatBadge count={chat.unread} />
          </span>
        ) : null}
      </div>
      <div className="min-w-0 flex-1 border-b border-[#F0F0F0] pb-[10px] group-last:border-0">
        <div className="flex items-baseline justify-between gap-2">
          <span className="truncate text-[14px] text-[#1a1a1a]">{chat.name}</span>
          <span className="shrink-0 text-[9.5px] tabular-nums text-[#b2b2b2]">{chat.time}</span>
        </div>
        <div className="mt-[4px] flex items-center gap-1.5">
          <span className="line-clamp-1 flex-1 text-[11px] text-[#9a9a9a]">
            {chat.muted && (
              <MutedIcon size={11} className="mr-1 inline-block translate-y-[1px] text-[#C4C4C4]" />
            )}
            {chat.draft ? <span className="text-[#FA5151]">[草稿] {chat.draft}</span> : chat.last}
          </span>
          {chat.muted && !chat.unread && <MutedIcon size={11} className="shrink-0 text-[#C9C9C9]" />}
        </div>
      </div>
    </button>
  );
}

function ChatsScreen({
  onOpenChat,
  onNavigate,
  onNotify,
}: {
  onOpenChat: (id: string) => void;
  onNavigate: (screen: MobileScreen) => void;
  onNotify: (s: string) => void;
}) {
  const [sheet, setSheet] = useState(false);
  const sorted = useMemo(() => [...chats].sort((a, b) => Number(Boolean(b.pinned)) - Number(Boolean(a.pinned))), []);
  const pinned = sorted.filter((c) => c.pinned);
  const rest = sorted.filter((c) => !c.pinned);
  return (
    <div className="screen-enter relative flex h-full flex-col bg-[#EDEDED]">
      <HomeHeader
        title="微信"
        right={
          <>
            <button onClick={() => onNavigate("search")} className="pressable grid h-8 w-8 place-items-center rounded-full active:bg-black/5" aria-label="搜索">
              <Search size={20} strokeWidth={1.8} />
            </button>
            <button onClick={() => setSheet(true)} className="pressable grid h-8 w-8 place-items-center rounded-full active:bg-black/5" aria-label="发起">
              <Plus size={23} strokeWidth={1.6} />
            </button>
          </>
        }
      />
      <div className="shrink-0 bg-[#EDEDED] px-3 pb-2.5 pt-0.5">
        <SearchBar onClick={() => onNavigate("search")} />
      </div>
      <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto bg-white">
        {pinned.length > 0 && (
          <div className="bg-[#F7F7F7] px-3.5 pb-1 pt-2 text-[9px] font-medium tracking-[0.1em] text-[#9a9a9a]">置顶</div>
        )}
        {pinned.map((chat) => (
          <ChatRow key={chat.id} chat={chat} onClick={() => onOpenChat(chat.id)} />
        ))}
        {rest.map((chat) => (
          <ChatRow key={chat.id} chat={chat} onClick={() => onOpenChat(chat.id)} />
        ))}
        <button onClick={() => onNotify("折叠的群聊 · 2 个群")} className="flex w-full items-center justify-between bg-white px-3.5 py-3 text-left">
          <div className="flex items-center gap-2.5">
            <span className="grid h-8 w-8 place-items-center rounded-[7px] bg-[#F4F4F4] text-[#8a8a8a]">
              <LayoutGrid size={15} />
            </span>
            <span className="text-[11px] text-[#8a8a8a]">折叠的群聊</span>
          </div>
          <span className="text-[9px] tabular-nums text-[#c0c0c0]">2 个群</span>
        </button>
        <div className="flex items-center gap-2 bg-white px-8 py-5">
          <span className="h-px flex-1 bg-[#EFEFEF]" />
          <span className="text-[8px] tracking-[0.2em] text-[#cfcfcf]">已同步全部消息</span>
          <span className="h-px flex-1 bg-[#EFEFEF]" />
        </div>
      </div>
      <TabBar active="chats" onChange={(t) => onNavigate(t)} />
      {sheet && (
        <div className="absolute inset-0 z-40 flex flex-col bg-black/30 backdrop-blur-[1px]" onClick={() => setSheet(false)}>
          <div className="mt-auto rounded-t-[20px] bg-[#F7F7F7]/98 pb-8 shadow-2xl" style={{ animation: "sheetUp 0.38s cubic-bezier(0.22,1,0.36,1) both" }} onClick={(e) => e.stopPropagation()}>
            <div className="mx-auto mt-2 h-1 w-9 rounded-full bg-[#D4D4D4]" />
            <div className="flex items-center justify-between px-4 py-3">
              <span className="text-[13px] font-semibold">快捷操作</span>
              <button onClick={() => setSheet(false)} className="grid h-7 w-7 place-items-center rounded-full bg-[#ECECEC]"><X size={14} className="text-[#666]" /></button>
            </div>
            <div className="grid grid-cols-4 gap-y-4 px-4 pb-2">
              {[
                { icon: Users, label: "发起群聊", bg: "linear-gradient(135deg,#0BD566,#06A854)", action: () => { setSheet(false); onOpenChat("spring"); } },
                { icon: Plus, label: "添加朋友", bg: "linear-gradient(135deg,#F7BE55,#EE9E2E)", action: () => { setSheet(false); onNavigate("newfriends"); } },
                { icon: ScanLine, label: "扫一扫", bg: "linear-gradient(135deg,#5CA3EF,#3B7CD4)", action: () => { setSheet(false); onNavigate("scan"); } },
                { icon: QrCode, label: "收付款", bg: "linear-gradient(135deg,#12C971,#05934C)", action: () => { setSheet(false); onNavigate("pay"); } },
              ].map((item) => (
                <button key={item.label} onClick={item.action} className="pressable flex flex-col items-center gap-1.5">
                  <span className="grid h-[52px] w-[52px] place-items-center rounded-[15px] text-white shadow-[0_6px_16px_rgba(0,0,0,0.12)]" style={{ background: item.bg }}>
                    <item.icon size={22} strokeWidth={1.7} />
                  </span>
                  <span className="text-[9.5px] text-[#555]">{item.label}</span>
                </button>
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function ContactsScreen({
  onOpenContact,
  onNavigate,
  onNotify,
}: {
  onOpenContact: (id: string) => void;
  onNavigate: (screen: MobileScreen) => void;
  onNotify: (s: string) => void;
}) {
  const sorted = useMemo(() => [...contacts].sort((a, b) => a.initial.localeCompare(b.initial)), []);
  const quick = [
    { label: "新的朋友", bg: "#F5B640", gradient: "linear-gradient(135deg,#F8C85E,#EE9E2E)", icon: <Users size={16} />, action: () => onNavigate("newfriends"), badge: 3 },
    { label: "仅聊天的朋友", bg: "#A7D36A", gradient: "linear-gradient(135deg,#B5DE7C,#8CBC4E)", icon: <Users size={16} />, action: () => onNotify("仅聊天的朋友 · 2 位") },
    { label: "群聊", bg: "#67A3E8", gradient: "linear-gradient(135deg,#7AB5F0,#4E8FD8)", icon: <Users size={16} />, action: () => onNotify("群聊 · 4 个") },
    { label: "标签", bg: "#67A3E8", gradient: "linear-gradient(135deg,#7AB5F0,#4E8FD8)", icon: <Star size={15} />, action: () => onNotify("标签管理 · 朋友 / 家人 / 设计") },
    { label: "公众号", bg: "#8D7CDF", gradient: "linear-gradient(135deg,#A394EA,#7563D4)", icon: <FileText size={15} />, action: () => onNavigate("official") },
  ];
  const starred = sorted.filter((p) => p.star);
  return (
    <div className="screen-enter flex h-full flex-col bg-[#EDEDED]">
      <HomeHeader title="通讯录" right={<button onClick={() => onNavigate("newfriends")} className="pressable grid h-8 w-8 place-items-center"><Plus size={23} strokeWidth={1.6} /></button>} />
      <div className="shrink-0 bg-[#EDEDED] px-3 pb-2.5"><SearchBar onClick={() => onNavigate("search")} /></div>
      <div className="scrollbar-thin relative min-h-0 flex-1 overflow-y-auto bg-[#F5F5F5]">
        <div className="bg-white">
          {quick.map((q) => (
            <button key={q.label} onClick={q.action} className="pressable flex w-full items-center gap-3 border-b border-[#F5F5F5] px-3.5 py-[9px] pr-6 text-left last:border-0 active:bg-[#F5F5F5]">
              <MobileTile icon={q.icon} color={q.bg} gradient={q.gradient} />
              <span className="flex-1 text-[13px] text-[#1a1a1a]">{q.label}</span>
              {q.badge ? <WeChatBadge count={q.badge} /> : null}
            </button>
          ))}
        </div>
        {starred.length > 0 && (
          <>
            <div className="px-3.5 pb-1 pt-2.5 text-[9px] font-medium tracking-[0.1em] text-[#9a9a9a]">星标朋友</div>
            <div className="bg-white">
              {starred.map((p) => (
                <button key={p.id} onClick={() => onOpenContact(p.id)} className="pressable flex w-full items-center gap-3 border-b border-[#F7F7F7] px-3.5 py-[9px] pr-6 text-left last:border-0 active:bg-[#F5F5F5]">
                  <Avatar src={p.avatar} name={p.name} className="h-9 w-9 rounded-[6px]" online={p.online} />
                  <span className="flex-1 text-[13px] text-[#1a1a1a]">{p.name}</span>
                  <Star size={11} className="fill-[#F5B640] text-[#F5B640]" />
                </button>
              ))}
            </div>
          </>
        )}
        {["A", "G", "H", "J", "L", "M", "Q", "T", "Z"].map((letter) => {
          const people = sorted.filter((p) => p.initial === letter);
          if (!people.length) return null;
          return (
            <div key={letter}>
              <div className="sticky top-0 z-10 bg-[#F5F5F5]/95 px-3.5 py-1 text-[9px] font-semibold tracking-[0.08em] text-[#8e8e93] backdrop-blur">{letter}</div>
              <div className="bg-white">
                {people.map((p) => (
                  <button key={p.id} onClick={() => onOpenContact(p.id)} className="pressable flex w-full items-center gap-3 border-b border-[#F7F7F7] px-3.5 py-[9px] pr-6 text-left last:border-0 active:bg-[#F5F5F5]">
                    <Avatar src={p.avatar} name={p.name} className="h-9 w-9 rounded-[6px]" online={p.online} />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate text-[13px] text-[#1a1a1a]">{p.name}</span>
                      {p.note && <span className="mt-0.5 block truncate text-[9px] text-[#aaa]">{p.note}</span>}
                    </span>
                  </button>
                ))}
              </div>
            </div>
          );
        })}
        <div className="flex items-center justify-center gap-2 py-4 text-[8.5px] text-[#b4b4b4]">
          <span className="h-px w-8 bg-[#E2E2E2]" /> 共 {contacts.length} 位联系人 <span className="h-px w-8 bg-[#E2E2E2]" />
        </div>
        <div className="absolute bottom-16 right-[3px] top-[180px] flex flex-col items-center justify-start gap-[2px] text-[7px] font-medium text-[#8e8e93]">
          {["A", "G", "H", "J", "L", "M", "Q", "T", "Z", "#"].map((l) => (
            <button key={l} onClick={() => onNotify(`定位到 ${l}`)} className="grid h-[13px] w-[13px] place-items-center rounded-full active:bg-[#07C160] active:text-white">{l}</button>
          ))}
        </div>
      </div>
      <TabBar active="contacts" onChange={(t) => onNavigate(t)} />
    </div>
  );
}

function DiscoverRow({
  icon,
  color,
  gradient,
  label,
  desc,
  right,
  onClick,
  border = true,
}: {
  icon: React.ReactNode;
  color: string;
  gradient?: string;
  label: string;
  desc?: string;
  right?: React.ReactNode;
  onClick: () => void;
  border?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={cn("pressable group flex min-h-[54px] w-full items-center gap-3 bg-white px-3.5 text-left active:bg-[#F5F5F5]", border && "border-b border-[#F3F3F3]")}
    >
      <MobileTile icon={icon} color={color} gradient={gradient} size="md" />
      <span className="min-w-0 flex-1">
        <span className="block text-[13.5px] text-[#1a1a1a]">{label}</span>
        {desc && <span className="mt-0.5 block truncate text-[9px] text-[#aaa]">{desc}</span>}
      </span>
      {right}
      <ChevronRight size={15} className="shrink-0 text-[#C7C7CC] transition-transform group-active:translate-x-0.5" strokeWidth={1.8} />
    </button>
  );
}

function DiscoverScreen({ onNavigate, onNotify }: { onNavigate: (screen: MobileScreen) => void; onNotify: (s: string) => void }) {
  return (
    <div className="screen-enter flex h-full flex-col bg-[#EDEDED]">
      <HomeHeader title="发现" right={<button onClick={() => onNavigate("search")} className="pressable grid h-8 w-8 place-items-center"><Search size={20} strokeWidth={1.8} /></button>} />
      <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto bg-[#F5F5F5] pb-3 pt-2.5">
        <div className="overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <DiscoverRow
            icon={<Aperture size={17} />}
            color="#5B9BD5"
            gradient="linear-gradient(135deg,#6FAEE6,#4A87C4)"
            label="朋友圈"
            desc="林小满等 3 位朋友更新了"
            right={
              <div className="flex items-center gap-1.5">
                <div className="flex -space-x-1.5">
                  {moments.slice(0, 3).map((m) => (
                    <img key={m.id} src={m.avatar} alt="" className="h-[22px] w-[22px] rounded-[5px] border-[1.5px] border-white object-cover shadow-sm" />
                  ))}
                </div>
                <span className="h-[7px] w-[7px] rounded-full bg-[#FA5151] shadow-[0_0_6px_rgba(250,81,81,0.6)]" />
              </div>
            }
            onClick={() => onNavigate("moments")}
            border={false}
          />
        </div>
        <div className="mt-2 overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <DiscoverRow
            icon={<Video size={17} />}
            color="#E97860"
            gradient="linear-gradient(135deg,#F08B72,#DE5F46)"
            label="视频号"
            desc="2 场直播正在进行"
            right={<span className="flex items-center gap-1 rounded-full bg-[#FFF1EE] px-2 py-1 text-[8px] font-medium text-[#E8604C]"><span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[#FA5151]" /> LIVE</span>}
            onClick={() => onNavigate("channels")}
          />
          <DiscoverRow icon={<Radio size={16} />} color="#E97860" gradient="linear-gradient(135deg,#F08B72,#DE5F46)" label="直播" onClick={() => onNotify("正在直播 · 2 场关注")} border={false} />
        </div>
        <div className="mt-2 overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <DiscoverRow icon={<ScanLine size={17} />} color="#4C8EDB" gradient="linear-gradient(135deg,#5FA1EC,#3B7CD4)" label="扫一扫" onClick={() => onNavigate("scan")} />
          <DiscoverRow icon={<Move size={16} />} color="#67A3E8" gradient="linear-gradient(135deg,#7AB5F0,#4E8FD8)" label="摇一摇" onClick={() => onNotify("摇一摇 · 正在寻找同时摇动的人")} border={false} />
        </div>
        <div className="mt-2 overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <DiscoverRow icon={<Eye size={17} />} color="#E8A431" gradient="linear-gradient(135deg,#F0B64E,#DE9626)" label="看一看" right={<span className="h-[7px] w-[7px] rounded-full bg-[#FA5151]" />} onClick={() => onNotify("看一看 · 朋友都在读")} />
          <DiscoverRow icon={<Search size={16} />} color="#F09A50" gradient="linear-gradient(135deg,#F4AB63,#E5853A)" label="搜一搜" onClick={() => onNavigate("search")} />
          <DiscoverRow icon={<MapPin size={16} />} color="#67A3E8" gradient="linear-gradient(135deg,#7AB5F0,#4E8FD8)" label="附近" desc="天目里美术馆 · 220m" onClick={() => onNavigate("location")} border={false} />
        </div>
        <div className="mt-2 overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <DiscoverRow icon={<ShoppingBag size={16} />} color="#F09090" gradient="linear-gradient(135deg,#F4A3A3,#E87B7B)" label="购物" onClick={() => onNotify("购物 · 今日好物")} />
          <DiscoverRow icon={<Gamepad2 size={16} />} color="#8D7CDF" gradient="linear-gradient(135deg,#A394EA,#7563D4)" label="游戏" onClick={() => onNotify("微信游戏中心")} border={false} />
        </div>
        <div className="mt-2 overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <DiscoverRow icon={<LayoutGrid size={15} />} color="#4DB6AC" gradient="linear-gradient(135deg,#5FC6BB,#3AA296)" label="小程序" right={<span className="text-[9px] tabular-nums text-[#b6b6b6]">12 个最近使用</span>} onClick={() => onNavigate("mini")} border={false} />
        </div>
        <div className="flex items-center gap-2 px-10 py-4">
          <span className="h-px flex-1 bg-[#E2E2E2]" />
          <span className="text-[8px] tracking-[0.14em] text-[#b5b5b5]">发现更多生活</span>
          <span className="h-px flex-1 bg-[#E2E2E2]" />
        </div>
      </div>
      <TabBar active="discover" onChange={(t) => onNavigate(t)} />
    </div>
  );
}

function MeScreen({ onNavigate, onNotify }: { onNavigate: (screen: MobileScreen) => void; onNotify: (s: string) => void }) {
  return (
    <div className="screen-enter flex h-full flex-col bg-[#EDEDED]">
      <HomeHeader title="我" right={<button onClick={() => onNavigate("settings")} className="pressable grid h-8 w-8 place-items-center"><Settings size={20} strokeWidth={1.7} /></button>} />
      <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto bg-[#F5F5F5]">
        <div className="relative overflow-hidden bg-white">
          <div className="pointer-events-none absolute inset-0 bg-[radial-gradient(ellipse_70%_60%_at_85%_0%,rgba(7,193,96,0.08),transparent_60%)]" />
          <button onClick={() => onNavigate("mycode")} className="pressable relative z-10 flex w-full items-center gap-3 px-4 pb-3 pt-4 text-left">
            <span className="relative">
              <Avatar src={me.avatar} name={me.name} className="h-[60px] w-[60px] rounded-[13px]" online />
              <button
                onClick={(e) => { e.stopPropagation(); onNavigate("status"); }}
                className="absolute -bottom-1 -right-1 grid h-5 w-5 place-items-center rounded-full bg-[#07C160] text-[11px] text-white shadow ring-2 ring-white"
              >
                🌿
              </button>
            </span>
            <span className="min-w-0 flex-1">
              <span className="block text-[19px] font-semibold tracking-wide text-[#1a1a1a]">{me.name}</span>
              <span className="mt-1 block text-[10px] tabular-nums text-[#a2a2a2]">微信号：suhe_2026</span>
              <span className="mt-1.5 inline-flex max-w-full items-center gap-1 truncate rounded-full bg-[#F2F8F3] px-2 py-[3px] text-[9px] text-[#5f7a66]">
                🌿 在春天里 · 在生活里，慢慢走
              </span>
            </span>
            <span className="flex flex-col items-center gap-1 text-[#a6a6a6]">
              <QrCode size={19} />
              <ChevronRight size={13} className="text-[#C8C8C8]" />
            </span>
          </button>
          <div className="relative z-10 mx-4 flex items-center gap-2 border-t border-[#F3F3F3] py-2.5">
            {[
              ["12", "朋友"],
              ["4", "群聊"],
              ["6", "收藏"],
            ].map(([v, l], i) => (
              <button key={l} onClick={() => (i === 2 ? onNavigate("favorites") : onNotify(`${l}：${v} 个`))} className="flex flex-1 flex-col items-center gap-0.5">
                <span className="text-[14px] font-semibold tabular-nums text-[#333]">{v}</span>
                <span className="text-[8.5px] text-[#aaa]">{l}</span>
              </button>
            ))}
            <span className="h-7 w-px bg-[#F0F0F0]" />
            <button onClick={() => onNavigate("status")} className="flex flex-1 flex-col items-center gap-1">
              <Camera size={15} className="text-[#777]" />
              <span className="text-[8.5px] text-[#aaa]">状态</span>
            </button>
          </div>
        </div>

        <div className="mt-2 overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <button onClick={() => onNavigate("wallet")} className="pressable flex w-full items-center gap-3 px-4 py-3.5 text-left">
            <span className="relative grid h-9 w-9 place-items-center overflow-hidden rounded-[10px] bg-gradient-to-br from-[#0BD566] to-[#06A854] text-white shadow-[0_4px_10px_rgba(7,193,96,0.3)]">
              <span className="absolute inset-x-0 top-0 h-1/2 bg-gradient-to-b from-white/25 to-transparent" />
              <Wallet size={17} className="relative" />
            </span>
            <span className="flex-1">
              <span className="block text-[13.5px] text-[#1a1a1a]">服务</span>
              <span className="mt-0.5 block text-[9px] text-[#aaa]">收付款 · 钱包 · 生活服务</span>
            </span>
            <span className="rounded-full bg-[#E9F8EF] px-2 py-1 text-[8px] font-medium tabular-nums text-[#07A858]">¥12,860</span>
            <ChevronRight size={15} className="text-[#C7C7CC]" />
          </button>
        </div>

        <div className="mt-2 overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <MobileCell icon={<MobileTile icon={<Star size={15} />} color="#E8A431" gradient="linear-gradient(135deg,#F0B64E,#DE9626)" />} label="收藏" sublabel="链接、图片、笔记与文件" right={<span className="text-[9px] tabular-nums text-[#aaa]">6</span>} onClick={() => onNavigate("favorites")} />
          <MobileCell icon={<MobileTile icon={<Aperture size={15} />} color="#5B9BD5" gradient="linear-gradient(135deg,#6FAEE6,#4A87C4)" />} label="朋友圈" sublabel="记录此刻，分享生活" onClick={() => onNavigate("moments")} />
          <MobileCell icon={<MobileTile icon={<ShoppingBag size={15} />} color="#E97860" gradient="linear-gradient(135deg,#F08B72,#DE5F46)" />} label="卡包" sublabel="优惠券、会员卡与票证" right={<span className="rounded-full bg-[#FA5151] px-1.5 py-0.5 text-[7.5px] text-white">2</span>} onClick={() => onNavigate("cards")} />
          <MobileCell icon={<MobileTile icon={<span className="text-[15px]">🙂</span>} color="#8D7CDF" gradient="linear-gradient(135deg,#A394EA,#7563D4)" />} label="表情" sublabel="我的表情与表情商店" onClick={() => onNavigate("stickers")} border={false} />
        </div>

        <div className="mt-2 overflow-hidden rounded-xl bg-white shadow-[0_1px_3px_rgba(0,0,0,0.03)]">
          <MobileCell icon={<MobileTile icon={<Settings size={15} />} color="#7C8B84" gradient="linear-gradient(135deg,#93A69B,#687B70)" />} label="设置" sublabel="账号与安全、隐私、通用" onClick={() => onNavigate("settings")} border={false} />
        </div>
        <div className="flex justify-center py-5">
          <div className="flex items-center gap-1.5 text-[7.5px] tracking-[0.1em] text-[#b3b3b3]">
            <WeChatLogo className="h-3.5 w-3.5 !drop-shadow-none" /> 微信 · 一个生活方式
          </div>
        </div>
      </div>
      <TabBar active="me" onChange={(t) => onNavigate(t)} />
    </div>
  );
}

export function PhoneWeChat({
  screen,
  onScreenChange,
  onNotify,
}: {
  screen: MobileScreen;
  onScreenChange: (screen: MobileScreen) => void;
  onNotify: (text: string) => void;
}) {
  const [chatId, setChatId] = useState("lin");
  const [contactId, setContactId] = useState("lin");
  const openChat = (id: string) => {
    setChatId(id);
    onScreenChange("chat");
  };
  const openContact = (id: string) => {
    setContactId(id);
    onScreenChange("profile");
  };

  const lightStatusScreens: MobileScreen[] = ["channels", "call", "moments", "redpacket", "scan", "reading"];
  const isDark = lightStatusScreens.includes(screen);

  const renderScreen = () => {
    switch (screen) {
      case "chats":
        return <ChatsScreen onOpenChat={openChat} onNavigate={onScreenChange} onNotify={onNotify} />;
      case "contacts":
        return <ContactsScreen onOpenContact={openContact} onNavigate={onScreenChange} onNotify={onNotify} />;
      case "discover":
        return <DiscoverScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "me":
        return <MeScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "chat":
        return <ChatScreen chatId={chatId} onNavigate={onScreenChange} onNotify={onNotify} />;
      case "profile":
        return <ProfileScreen contactId={contactId} onOpenChat={openChat} onBack={() => onScreenChange("contacts")} onNotify={onNotify} />;
      case "newfriends":
        return <NewFriendsScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "moments":
        return <MomentsScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "channels":
        return <ChannelsScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "mini":
        return <MiniProgramsScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "search":
        return <SearchScreen onBack={() => onScreenChange("chats")} onNotify={onNotify} />;
      case "scan":
        return <ScanScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "reading":
        return <ReadingScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "favorites":
        return <FavoritesScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "cards":
        return <CardsScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "stickers":
        return <StickersScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "settings":
        return <SettingsScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "wallet":
        return <WalletScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "pay":
        return <PayScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "redpacket":
        return <RedPacketScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "call":
        return <CallScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "login":
        return <LoginScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "status":
        return <StatusScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "transfer":
        return <TransferScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "location":
        return <LocationScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "official":
        return <OfficialScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      case "mycode":
        return <MyCodeScreen onNavigate={onScreenChange} onNotify={onNotify} />;
      default:
        return <ChatsScreen onOpenChat={openChat} onNavigate={onScreenChange} onNotify={onNotify} />;
    }
  };

  return (
    <div className="relative shrink-0 select-none">
      {/* titanium side buttons */}
      <div className="pointer-events-none absolute -left-[2.5px] top-[130px] h-[24px] w-[3px] rounded-l-[2px] bg-gradient-to-b from-[#4a4a4a] via-[#2a2a2a] to-[#3a3a3a] shadow-sm" />
      <div className="pointer-events-none absolute -left-[2.5px] top-[168px] h-[44px] w-[3px] rounded-l-[2px] bg-gradient-to-b from-[#4a4a4a] via-[#2a2a2a] to-[#3a3a3a] shadow-sm" />
      <div className="pointer-events-none absolute -left-[2.5px] top-[220px] h-[44px] w-[3px] rounded-l-[2px] bg-gradient-to-b from-[#4a4a4a] via-[#2a2a2a] to-[#3a3a3a] shadow-sm" />
      <div className="pointer-events-none absolute -right-[2.5px] top-[180px] h-[72px] w-[3px] rounded-r-[2px] bg-gradient-to-b from-[#4a4a4a] via-[#2a2a2a] to-[#3a3a3a] shadow-sm" />
      {/* frame */}
      <div className="relative rounded-[50px] bg-gradient-to-br from-[#3d3d3d] via-[#1c1c1c] to-[#0e0e0e] p-[2.5px] shadow-[0_40px_90px_-24px_rgba(20,32,24,0.6),0_4px_10px_rgba(0,0,0,0.3)]">
        <div className="relative h-[706px] w-[340px] rounded-[48px] bg-[#0a0a0a] p-[9px]">
          <div className="relative h-full w-full overflow-hidden rounded-[40px] bg-white">
            <div key={`${screen}-${chatId}-${contactId}`} className="relative h-full w-full overflow-hidden">
              {renderScreen()}
            </div>
            {/* status bar + island */}
            <div className="pointer-events-none absolute inset-x-0 top-0 z-[60]">
              <PhoneStatusBar dark={!isDark} />
              <div className="absolute left-1/2 top-[10px] flex h-[24px] w-[94px] -translate-x-1/2 items-center justify-end gap-1.5 rounded-full bg-black px-3 shadow-[inset_0_0_4px_rgba(255,255,255,0.08)]">
                <span className="h-[7px] w-[7px] rounded-full bg-[#0d1a14] ring-1 ring-[#2a3a31]">
                  <span className="mx-auto mt-[1px] block h-[2px] w-[2px] rounded-full bg-[#1d3a5f]/80" />
                </span>
                <span className="h-[5px] w-[5px] rounded-full bg-[#101418]" />
              </div>
            </div>
            {/* screen glare */}
            <div className="screenglare pointer-events-none absolute inset-0 z-[55]" />
            <div className="device-glare pointer-events-none absolute inset-0 z-[55]" />
            {/* home indicator */}
            <div className="pointer-events-none absolute inset-x-0 bottom-[7px] z-[60] flex justify-center">
              <span className={cn("h-[4.5px] w-[110px] rounded-full backdrop-blur", isDark ? "bg-white/90" : "bg-black/85")} />
            </div>
          </div>
        </div>
      </div>
      {/* reflection */}
      <div className="mx-auto mt-2 h-[14px] w-[240px] rounded-[50%] bg-black/15 blur-[10px]" />
      <div className="mt-2 flex items-center justify-center gap-2 text-[8px] font-medium tracking-[0.18em] text-[#9E9E96]">
        <span className="relative flex h-1.5 w-1.5">
          <span className="absolute h-full w-full animate-ping rounded-full bg-[#07C160] opacity-50" />
          <span className="relative h-1.5 w-1.5 rounded-full bg-[#07C160]" />
        </span>
        WECHAT MOBILE · 8.0.62
      </div>
    </div>
  );
}
