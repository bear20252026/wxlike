import { useEffect, useState } from "react";
import {
  ArrowLeft,
  Bell,
  CalendarDays,
  Camera,
  Check,
  ChevronRight,
  CircleHelp,
  CreditCard,
  Eye,
  EyeOff,
  FileText,
  Folder,
  Gift,
  Globe,
  Heart,
  Image as ImageIcon,
  Link2,
  Lock,
  MapPin,
  MessageCircle,
  Mic,
  MicOff,
  MoreHorizontal,
  PhoneOff,
  Plus,
  QrCode,
  ReceiptText,
  Search,
  Settings2,
  Share2,
  ShieldCheck,
  Smartphone,
  Star,
  Sun,
  Ticket,
  UserRound,
  Video,
  VideoOff,
  Volume2,
  Wallet,
  X,
} from "lucide-react";
import { assets, contacts, favorites, me, px, transactions, type Person } from "../../data";
import { Avatar, Switch, stickerSets } from "../shared";
import { cn } from "../../utils/cn";
import { MobileCell, MobileTile, MobileTop, PhoneBack } from "./atoms";
import type { MobileScreen } from "./types";

type BaseProps = { onNavigate: (screen: MobileScreen) => void; onNotify: (message: string) => void };

export function WalletScreen({ onNavigate, onNotify }: BaseProps) {
  const [hidden, setHidden] = useState(false);
  return (
    <div className="flex h-full flex-col bg-[#F5F5F5]">
      <MobileTop title="服务" onBack={() => onNavigate("me")} right={<MoreHorizontal size={19} />} />
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pb-6 pt-2.5">
        <div className="relative overflow-hidden rounded-2xl bg-[#07C160] p-4 text-white shadow-[0_10px_28px_rgba(7,193,96,.22)]">
          <div className="absolute -right-7 -top-10 h-28 w-28 rounded-full border-[16px] border-white/[0.08]" />
          <div className="absolute -bottom-12 right-12 h-24 w-24 rounded-full border-[14px] border-white/[0.06]" />
          <div className="relative z-10">
            <div className="flex items-center justify-between"><span className="text-[9px] text-white/75">微信支付 · 零钱</span><button onClick={() => setHidden((v) => !v)}>{hidden ? <EyeOff size={14} /> : <Eye size={14} />}</button></div>
            <div className="mt-2 flex items-baseline gap-1"><span className="text-[12px] text-white/90">¥</span><span className="text-[27px] font-medium leading-none tracking-tight">{hidden ? "••••••" : "12,860.24"}</span></div>
            <div className="mt-4 flex items-center gap-2"><button onClick={() => onNavigate("pay")} className="rounded-full bg-white px-3 py-1.5 text-[9px] font-medium text-[#07A858]">收付款</button><button onClick={() => onNotify("充值页面已打开")} className="rounded-full bg-white/15 px-3 py-1.5 text-[9px] text-white">充值</button><button onClick={() => onNotify("提现已发起")} className="rounded-full bg-white/15 px-3 py-1.5 text-[9px] text-white">提现</button></div>
          </div>
        </div>

        <div className="mt-2.5 rounded-xl bg-white p-3.5">
          <div className="mb-3 flex items-center justify-between"><span className="text-[11px] font-medium text-[#333]">支付服务</span><button onClick={() => onNotify("管理支付服务")} className="text-[8.5px] text-[#AAA]">管理</button></div>
          <div className="grid grid-cols-4 gap-y-4">
            {[
              { icon: QrCode, label: "收付款", bg: "#E9F8EF", color: "#07B25A", onClick: () => onNavigate("pay") },
              { icon: Wallet, label: "钱包", bg: "#FFF5E4", color: "#E8A431", onClick: () => onNotify("钱包详情已打开") },
              { icon: CreditCard, label: "银行卡", bg: "#EAF3FF", color: "#4C8EDB", onClick: () => onNotify("已绑定 3 张银行卡") },
              { icon: ReceiptText, label: "账单", bg: "#F0EEFF", color: "#765FC7", onClick: () => onNotify("账单页面已打开") },
            ].map((item) => (
              <button key={item.label} onClick={item.onClick} className="flex flex-col items-center gap-1.5">
                <span className="grid h-10 w-10 place-items-center rounded-xl" style={{ background: item.bg, color: item.color }}><item.icon size={19} strokeWidth={1.7} /></span>
                <span className="text-[9px] text-[#555]">{item.label}</span>
              </button>
            ))}
          </div>
        </div>

        <div className="mt-2.5 rounded-xl bg-white">
          <div className="flex items-center justify-between border-b border-[#F3F3F3] px-3.5 py-2.5"><span className="text-[11px] font-medium text-[#333]">生活服务</span><span className="text-[8.5px] text-[#AAA]">全部</span></div>
          <div className="grid grid-cols-4 gap-y-4 px-2 pb-4 pt-3.5">
            {["手机充值", "生活缴费", "医疗健康", "出行服务", "火车票机票", "酒店", "电影演出", "公益"].map((label, i) => {
              const icons = [Smartphone, Sun, ShieldCheck, MapPin, Ticket, CalendarDays, Video, Heart];
              const I = icons[i];
              return <button key={label} onClick={() => onNotify(`正在打开「${label}」`)} className="flex flex-col items-center gap-1.5"><span className="grid h-9 w-9 place-items-center rounded-xl bg-[#F6F6F6] text-[#666]"><I size={17} strokeWidth={1.6} /></span><span className="text-[8.5px] text-[#666]">{label}</span></button>;
            })}
          </div>
        </div>

        <div className="mt-2.5 rounded-xl bg-white">
          <div className="flex items-center justify-between border-b border-[#F3F3F3] px-3.5 py-2.5"><span className="text-[11px] font-medium text-[#333]">最近交易</span><button onClick={() => onNotify("完整账单已打开")} className="text-[8.5px] text-[#AAA]">查看全部</button></div>
          {transactions.map((t) => (
            <button key={t.id} onClick={() => onNotify(`查看交易：${t.name}`)} className="flex w-full items-center gap-3 border-b border-[#F7F7F7] px-3.5 py-2.5 text-left last:border-0">
              <span className="grid h-8 w-8 place-items-center rounded-lg bg-[#F6F6F6] text-sm">{t.emoji}</span>
              <span className="min-w-0 flex-1"><span className="block text-[10.5px] text-[#333]">{t.name}</span><span className="mt-0.5 block text-[8px] text-[#AAA]">{t.time}</span></span>
              <span className={cn("text-[10.5px] font-medium", t.amount.startsWith("+") ? "text-[#07A858]" : "text-[#333]")}>{t.amount}</span>
            </button>
          ))}
        </div>
        <div className="mt-3 flex items-center justify-center gap-1.5 text-[7.5px] text-[#AAADA9]"><ShieldCheck size={10} /> 微信支付 · 安全保护中</div>
      </div>
    </div>
  );
}

function FakeQR({ className }: { className?: string }) {
  return (
    <div className={cn("relative bg-white p-[6px]", className)}>
      <div className="grid h-full w-full grid-cols-[repeat(21,minmax(0,1fr))] gap-0">
        {Array.from({ length: 441 }).map((_, i) => (
          <span key={i} className={(i * 7 + Math.floor(i / 21) * 11) % 5 < 2 ? "aspect-square bg-[#1A1A1A]" : "aspect-square bg-white"} />
        ))}
      </div>
      {[
        "left-0 top-0",
        "right-0 top-0",
        "left-0 bottom-0",
      ].map((pos) => (
        <div key={pos} className={cn("absolute h-[32%] w-[32%] bg-white p-[5%]", pos)}>
          <div className="h-full w-full border-[4px] border-[#1A1A1A] bg-white p-[15%]"><div className="h-full w-full bg-[#1A1A1A]" /></div>
        </div>
      ))}
      <div className="absolute bottom-0 right-0 grid h-[32%] w-[32%] grid-cols-3 gap-0.5 bg-white p-1.5">
        {Array.from({ length: 9 }).map((_, i) => <span key={i} className={(i * 3) % 2 === 0 ? "bg-[#1A1A1A]" : "bg-white"} />)}
      </div>
    </div>
  );
}

export function PayScreen({ onNavigate, onNotify }: BaseProps) {
  const [tab, setTab] = useState("扫码");
  return (
    <div className="scrollbar-thin h-full overflow-y-auto bg-[#F5F5F5]">
      <div className="relative overflow-hidden bg-[#07C160] pb-16 pt-[30px] text-white">
        <div className="absolute -right-8 -top-8 h-36 w-36 rounded-full border-[24px] border-white/[0.08]" />
        <div className="flex h-[44px] items-center justify-between px-3"><button onClick={() => onNavigate("wallet")} className="grid h-8 w-8 place-items-center"><ArrowLeft size={22} /></button><span className="text-[14px] font-medium">收付款</span><button onClick={() => onNotify("更多收付款设置")}><MoreHorizontal size={20} /></button></div>
        <div className="px-5 pt-3"><div className="text-[18px] font-medium leading-6">扫一扫，<br />向苏禾付款</div><div className="mt-1.5 text-[9px] text-white/70">微信支付 · 安全、便捷</div></div>
      </div>
      <div className="relative z-10 mx-3 -mt-11 rounded-2xl bg-white p-4 shadow-[0_12px_40px_rgba(0,0,0,.08)]">
        <div className="flex rounded-lg bg-[#F5F5F5] p-1">
          {["扫码", "收款"].map((t) => <button key={t} onClick={() => setTab(t)} className={cn("flex-1 rounded-md py-1.5 text-[10px]", tab === t ? "bg-white font-medium text-[#222] shadow-sm" : "text-[#888]")}>{t === "扫码" ? "向商家付款" : "二维码收款"}</button>)}
        </div>
        <div className="mx-auto mt-5 w-[185px] rounded-xl border border-[#F0F0F0] p-2.5">
          <FakeQR className="aspect-square w-full" />
        </div>
        <div className="mt-3 text-center"><div className="text-[11.5px] font-medium text-[#333]">{tab === "扫码" ? "苏禾的付款码" : "苏禾的收款码"}</div><div className="mt-1 text-[8.5px] text-[#AAA]">扫描二维码即可{tabe(tab)}</div></div>
        <div className="mt-4 flex items-center justify-between rounded-lg bg-[#F9F9F9] px-3 py-2.5"><div className="flex items-center gap-2"><span className="grid h-6 w-6 place-items-center rounded-full bg-[#EAF8EF] text-[#07B25A]"><Wallet size={13} /></span><span className="text-[9.5px] text-[#555]">优先使用 · 零钱</span></div><button onClick={() => onNotify("选择付款方式")} className="flex items-center gap-0.5 text-[8.5px] text-[#AAA]">更换 <ChevronRight size={10} /></button></div>
      </div>
      <div className="mx-3 mt-2.5 rounded-xl bg-white">
        {[
          { icon: ReceiptText, label: "收款记录", sub: "查看最近的收款明细" },
          { icon: CreditCard, label: "支付管理", sub: "扣费服务、免密支付" },
          { icon: QrCode, label: "保存收款码", sub: "保存到相册，方便收款" },
        ].map((row) => <MobileCell key={row.label} icon={<span className="grid h-7 w-7 place-items-center rounded-lg bg-[#F5F5F5] text-[#666]"><row.icon size={14} /></span>} label={row.label} sublabel={row.sub} onClick={() => row.label === "保存收款码" ? onNotify("收款码已保存到相册") : onNotify(`${row.label}已打开`)} />)}
      </div>
      <div className="mt-3 flex items-center justify-center gap-1.5 pb-6 text-[8px] text-[#A8A8A8]"><ShieldCheck size={10} /> 本页面为设计演示，二维码不可用于真实支付</div>
    </div>
  );
}

function tabe(tab: string) {
  return tab === "扫码" ? "向商家付款" : "向你付款";
}

export function RedPacketScreen({ onNavigate, onNotify }: BaseProps) {
  const [opened, setOpened] = useState(false);
  return (
    <div className="relative flex h-full flex-col overflow-hidden bg-[#CD4A3B] pt-[30px] text-white">
      <div className="pointer-events-none absolute inset-0 opacity-25" style={{ backgroundImage: "radial-gradient(circle, #F3C994 1px, transparent 1px)", backgroundSize: "19px 19px" }} />
      <div className="relative z-10 flex h-[44px] items-center justify-between px-3"><button onClick={() => onNavigate("chat")}><X size={21} /></button><span className="text-[13px] font-medium">微信红包</span><button onClick={() => onNotify("更多红包设置")}><MoreHorizontal size={20} /></button></div>

      {!opened ? (
        <div className="relative z-10 flex flex-1 flex-col items-center justify-center px-7 pb-14">
          <div className="flex flex-col items-center">
            <Avatar src={px(14664508, 160, 160)} name="林小满" className="h-12 w-12 rounded-md" />
            <div className="mt-2 text-[11px] text-white/90">林小满 发出的红包</div>
          </div>
          <div className="relative mt-8 w-full max-w-[270px]">
            <div className="rounded-2xl bg-[#C33B2D] px-6 pb-12 pt-9 text-center shadow-[0_22px_40px_rgba(90,24,17,.35)]">
              <div className="mx-auto grid h-16 w-16 place-items-center rounded-full bg-[#E3B163] text-[#A32D20] shadow-[0_0_0_5px_rgba(227,177,99,.15)]"><span className="text-[26px] font-medium">開</span></div>
              <div className="mt-6 text-[12px] text-[#F4D5A1]">恭喜发财，大吉大利</div>
            </div>
            <button onClick={() => setOpened(true)} className="absolute -bottom-5 left-1/2 grid h-16 w-16 -translate-x-1/2 place-items-center rounded-full bg-[#E8BD75] text-[21px] font-medium text-[#9D2C20] shadow-lg ring-4 ring-[#CD4A3B] transition active:scale-95">開</button>
          </div>
          <div className="mt-10 text-center text-[9px] leading-5 text-white/55">领取后，金额将存入你的零钱<br />微信红包 · 一份小小的心意</div>
        </div>
      ) : (
        <div className="relative z-10 flex flex-1 flex-col items-center px-7 pt-10">
          <Avatar src={px(14664508, 160, 160)} name="林小满" className="h-12 w-12 rounded-md" />
          <div className="mt-2 text-[10px] text-white/75">林小满 的红包</div>
          <div className="mt-9 flex items-end gap-1"><span className="mb-2 text-[16px]">¥</span><span className="text-[46px] font-medium leading-none tracking-tight">66.66</span></div>
          <div className="mt-4 flex items-center gap-1.5 rounded-full bg-white/10 px-3 py-1.5 text-[9px] text-[#F4D5A1]"><Check size={11} /> 已存入零钱</div>
          <div className="mt-8 w-full max-w-[268px] rounded-xl bg-white/10 px-4 py-3 text-center backdrop-blur"><div className="text-[10px] text-[#F4D5A1]">恭喜发财，大吉大利</div><div className="mt-1 text-[8px] text-white/50">给你的第 1 个红包 · 今天 14:36</div></div>
          <div className="mt-auto w-full pb-8">
            <button onClick={() => onNavigate("wallet")} className="w-full rounded-lg bg-[#E8BD75] py-3 text-[12px] font-medium text-[#8F281D]">查看我的零钱</button>
            <button onClick={() => onNotify("已准备好回一个红包 🧧")} className="mt-2.5 w-full rounded-lg border border-[#F4D5A1]/40 py-3 text-[11px] text-[#F4D5A1]">也回一个红包</button>
          </div>
        </div>
      )}
    </div>
  );
}

const favIcons: Record<string, React.ReactNode> = {
  链接: <Link2 size={15} />,
  图片: <ImageIcon size={15} />,
  笔记: <FileText size={15} />,
  语音: <Mic size={15} />,
  文件: <Folder size={15} />,
};
const favColors: Record<string, string> = { 链接: "#EAF8EF", 图片: "#EAF3FF", 笔记: "#FFF5E4", 语音: "#F0EEFF", 文件: "#F2F2F2" };
const favIconColors: Record<string, string> = { 链接: "#07A858", 图片: "#4C8EDB", 笔记: "#D79A2F", 语音: "#765FC7", 文件: "#777" };

export function FavoritesScreen({ onNavigate, onNotify }: BaseProps) {
  const [type, setType] = useState("全部");
  const list = type === "全部" ? favorites : favorites.filter((f) => f.type === type);
  return (
    <div className="flex h-full flex-col bg-[#F5F5F5]">
      <MobileTop title="收藏" onBack={() => onNavigate("me")} right={<><Search size={18} /><Plus size={20} /></>} />
      <div className="flex shrink-0 gap-2 overflow-x-auto bg-[#EDEDED] px-3 pb-2.5">
        {["全部", "图片", "笔记", "链接", "文件", "语音"].map((t) => <button key={t} onClick={() => setType(t)} className={cn("shrink-0 rounded-full px-3 py-1.5 text-[9.5px]", type === t ? "bg-[#07C160] text-white" : "bg-white text-[#666]")}>{t}</button>)}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pb-6 pt-2.5">
        {list.map((item) => (
          <button key={item.id} onClick={() => item.type === "链接" ? onNavigate("reading") : onNotify(`正在查看收藏：${item.title}`)} className="mb-2 flex w-full gap-3 rounded-xl bg-white p-3 text-left first:mt-0">
            <span className="grid h-10 w-10 shrink-0 place-items-center rounded-lg" style={{ background: favColors[item.type], color: favIconColors[item.type] }}>{favIcons[item.type]}</span>
            <span className="min-w-0 flex-1"><span className="line-clamp-2 text-[11.5px] leading-4 text-[#333]">{item.title}</span><span className="mt-1.5 flex items-center justify-between text-[8.5px] text-[#AAA]"><span>{item.source}</span><span>{item.date}</span></span></span>
            {item.image && <img src={item.image} alt="" className="h-10 w-10 shrink-0 rounded-md object-cover" />}
          </button>
        ))}
        {list.length === 0 && <div className="py-20 text-center text-[10px] text-[#AAA]">暂无该类型的收藏</div>}
      </div>
    </div>
  );
}

export function StickersScreen({ onNavigate, onNotify }: BaseProps) {
  const [tab, setTab] = useState("我的表情");
  return (
    <div className="flex h-full flex-col bg-[#F5F5F5]">
      <MobileTop title="表情" onBack={() => onNavigate("me")} right={<><Search size={18} /><Settings2 size={18} /></>} />
      <div className="flex shrink-0 border-b border-[#EAEAEA] bg-white">
        {["我的表情", "表情包", "表情商店"].map((t) => <button key={t} onClick={() => setTab(t)} className={cn("relative flex-1 py-2.5 text-[10px]", tab === t ? "font-medium text-[#1F1F1F]" : "text-[#999]")}>{t}{tab === t && <span className="absolute bottom-0 left-1/2 h-[2px] w-5 -translate-x-1/2 rounded bg-[#07C160]" />}</button>)}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto p-3">
        {tab === "我的表情" ? (
          <>
            <div className="flex items-center justify-between px-1 pb-2"><span className="text-[9px] text-[#999]">最近使用</span><button onClick={() => onNotify("管理表情")} className="text-[8.5px] text-[#576B95]">管理</button></div>
            <div className="grid grid-cols-4 gap-2">
              {stickerSets.flat().map((emoji, i) => <button key={`${emoji}-${i}`} onClick={() => onNotify(`已发送表情 ${emoji}`)} className="grid aspect-square place-items-center rounded-xl bg-white text-[30px] shadow-sm active:bg-[#F0F0F0]">{emoji}</button>)}
            </div>
            <div className="mt-3 grid grid-cols-2 gap-2">
              {["春日来信", "小猫日常"].map((name, i) => <button key={name} onClick={() => onNotify(`打开表情包「${name}」`)} className="rounded-xl bg-white p-2.5 text-left"><div className="grid grid-cols-3 gap-1 rounded-lg bg-[#F7F7F7] p-2 text-[20px]">{stickerSets[i].slice(0, 6).map((s) => <span key={s}>{s}</span>)}</div><div className="mt-2 flex items-center justify-between"><span className="text-[9px] text-[#555]">{name}</span><span className="text-[8px] text-[#AAA]">已添加</span></div></button>)}
            </div>
            <button onClick={() => onNotify("已打开表情商店")} className="mt-3 flex w-full items-center justify-center gap-1.5 rounded-xl border border-[#E5E5E5] bg-white py-3 text-[9.5px] text-[#666]"><Plus size={12} /> 添加更多表情</button>
          </>
        ) : (
          <div className="grid grid-cols-2 gap-2">
            {["春日来信", "小猫日常", "可爱日常", "今天也要加油"].map((name, i) => <div key={name} className="overflow-hidden rounded-xl bg-white"><div className="grid aspect-[4/3] grid-cols-3 place-items-center bg-[#F4F2EC] text-[25px]">{stickerSets[i % 3].slice(0, 6).map((s) => <span key={s}>{s}</span>)}</div><div className="p-2.5"><div className="text-[10px] text-[#444]">{name}</div><div className="mt-1 flex items-center justify-between"><span className="text-[8px] text-[#AAA]">{(i + 2) * 34} 个表情</span><button onClick={() => onNotify(`已添加「${name}」`)} className="rounded-full bg-[#07C160] px-2 py-0.5 text-[8px] text-white">添加</button></div></div></div>)}
          </div>
        )}
      </div>
    </div>
  );
}

export function CardsScreen({ onNavigate, onNotify }: BaseProps) {
  return (
    <div className="flex h-full flex-col bg-[#F5F5F5]">
      <MobileTop title="卡包" onBack={() => onNavigate("me")} right={<><QrCode size={18} /><MoreHorizontal size={19} /></>} />
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pb-6 pt-2.5">
        <div className="rounded-xl bg-white p-3">
          <div className="mb-2.5 flex items-center justify-between"><span className="text-[11px] font-medium text-[#333]">我的卡券</span><span className="text-[8.5px] text-[#AAA]">管理</span></div>
          <div className="grid grid-cols-3 gap-2">
            {[["优惠券", "2", Ticket, "#FFF1E8"], ["礼品卡", "1", Gift, "#F2EBFF"], ["会员卡", "3", Star, "#EAF8EF"]].map(([label, count, Icon, bg]) => {
              const I = Icon as typeof Ticket;
              return <button key={label as string} onClick={() => onNotify(`${label}详情`)} className="rounded-lg p-2.5 text-center" style={{ background: bg as string }}><I size={18} className="mx-auto text-[#555]" /><div className="mt-1.5 text-[10px] text-[#444]">{label as string} <span className="text-[8px] text-[#999]">{count as string}</span></div></button>;
            })}
          </div>
        </div>

        <div className="mt-3 flex items-center justify-between px-1"><span className="text-[10px] font-medium text-[#333]">优惠券</span><span className="text-[8px] text-[#AAA]">2 张可用</span></div>
        <div className="relative mt-2 overflow-hidden rounded-xl bg-gradient-to-br from-[#F8D7BB] to-[#F3BE97] p-4 text-[#5C3A24]">
          <div className="absolute -left-2 -top-2 h-16 w-16 rounded-full bg-white/15" />
          <div className="flex items-start justify-between"><div><span className="text-[8px] tracking-wider opacity-70">ARABICA COFFEE</span><div className="mt-1.5 text-[22px] font-medium leading-none">¥ 20<span className="ml-1 text-[9px] font-normal opacity-70">无门槛</span></div><div className="mt-2 text-[9px]">任意门店 · 手冲咖啡可用</div></div><span className="rounded-full bg-[#5C3A24]/10 px-2 py-1 text-[7.5px]">即将过期</span></div>
          <div className="mt-4 flex items-center justify-between border-t border-dashed border-[#A9785B]/30 pt-2 text-[8px] opacity-75"><span>有效期至 2026.08.31</span><button onClick={() => onNotify("正在使用优惠券")} className="rounded-full bg-[#5C3A24] px-2.5 py-1 text-[8px] text-white">去使用</button></div>
        </div>

        <div className="mt-3 rounded-xl bg-white p-3">
          <div className="mb-2.5 flex items-center justify-between"><span className="text-[11px] font-medium text-[#333]">会员卡</span><span className="text-[8.5px] text-[#AAA]">全部 3 张</span></div>
          {[
            ["天目里美术馆", "年度会员 · 剩余 186 天", "ART PASS", "#20362C", "🎨"],
            ["%Arabica", "银卡会员 · 8 杯印章", "COFFEE CLUB", "#D9C8AD", "☕"],
          ].map(([name, sub, tag, color, emoji]) => (
            <button key={name} onClick={() => onNotify(`打开会员卡：${name}`)} className="mt-2 flex w-full items-center gap-3 rounded-lg border border-[#F0F0F0] p-2.5 text-left">
              <span className="grid h-10 w-10 place-items-center rounded-lg text-lg" style={{ background: `${color}18` }}>{emoji}</span>
              <span className="min-w-0 flex-1"><span className="block text-[10px] font-medium text-[#333]">{name}</span><span className="mt-0.5 block text-[8px] text-[#AAA]">{sub}</span></span>
              <span className="rounded px-1.5 py-1 text-[6.5px] tracking-wider text-white" style={{ background: color as string }}>{tag}</span>
            </button>
          ))}
        </div>

        <div className="mt-3 rounded-xl bg-white p-3">
          <div className="mb-2 text-[11px] font-medium text-[#333]">票证</div>
          <div className="flex items-center gap-3 rounded-lg border border-[#F0F0F0] p-3">
            <div className="flex flex-col items-center border-r border-dashed border-[#E2E2E2] pr-3"><span className="text-[9px] text-[#8C6D3B]">JUL</span><span className="text-[16px] font-medium leading-4 text-[#333]">26</span></div>
            <div className="flex-1"><div className="text-[10px] text-[#333]">「春日回声」展览门票</div><div className="mt-1 text-[8px] text-[#AAA]">周六 15:00 · 天目里美术馆</div></div>
            <button onClick={() => onNotify("已打开验票二维码")} className="grid h-8 w-8 place-items-center rounded-md bg-[#F5F5F5]"><QrCode size={15} className="text-[#777]" /></button>
          </div>
        </div>
      </div>
    </div>
  );
}

export function SettingsScreen({ onNavigate, onNotify }: BaseProps) {
  const [sound, setSound] = useState(true);
  const [youth, setYouth] = useState(false);
  const [searchFriend, setSearchFriend] = useState(true);
  const [wallet, setWallet] = useState(false);
  return (
    <div className="scrollbar-thin h-full overflow-y-auto bg-[#F5F5F5] pb-8">
      <MobileTop title="设置" onBack={() => onNavigate("me")} right={<Search size={18} />} />
      <button onClick={() => onNotify("账号详情")} className="flex w-full items-center gap-3 border-b border-[#F0F0F0] bg-white px-4 py-4 text-left">
        <Avatar src={me.avatar} name={me.name} className="h-11 w-11 rounded-md" />
        <span className="flex-1"><span className="block text-[12.5px] font-medium text-[#222]">{me.name}</span><span className="mt-1 block text-[9px] text-[#AAA]">微信号：suhe_2026</span></span>
        <div className="flex items-center gap-1.5"><span className="h-2 w-2 rounded-full bg-[#07C160]" /><span className="text-[8px] text-[#999]">已登录</span></div>
        <ChevronRight size={15} className="text-[#C5C5C5]" />
      </button>
      <div className="mt-2 bg-white">
        {[
          { icon: ShieldCheck, label: "账号与安全", color: "#07B25A", sub: "手机号、微信密码、声音锁" },
          { icon: Lock, label: "隐私", color: "#4C8EDB", sub: "朋友权限、添加我的方式" },
          { icon: Bell, label: "新消息通知", color: "#E8A431", sub: "声音、震动与通知显示" },
          { icon: Settings2, label: "通用", color: "#8B8B8B", sub: "字体大小、聊天背景" },
        ].map((row) => (
          <MobileCell key={row.label} icon={<MobileTile icon={<row.icon size={15} />} color={row.color} />} label={row.label} sublabel={row.sub} onClick={() => onNotify(`${row.label}设置`)} />
        ))}
      </div>

      <div className="mt-2 bg-white">
        {[
          { icon: Sun, label: "青少年模式", color: "#77BDA6", on: youth, set: setYouth },
          { icon: MessageCircle, label: "通过手机号找到我", color: "#67A3E8", on: searchFriend, set: setSearchFriend },
          { icon: Wallet, label: "钱包安全锁", color: "#E8A431", on: wallet, set: setWallet },
          { icon: Volume2, label: "消息提示音", color: "#8D7CDF", on: sound, set: setSound },
        ].map((row) => (
          <div key={row.label} className="flex min-h-[52px] items-center gap-3 border-b border-[#F3F3F3] bg-white px-4 last:border-0">
            <MobileTile icon={<row.icon size={15} />} color={row.color} />
            <span className="flex-1 text-[13px] text-[#222]">{row.label}</span>
            <Switch on={row.on} onChange={() => { row.set(!row.on); onNotify(`${row.label}已${row.on ? "关闭" : "开启"}`); }} />
          </div>
        ))}
      </div>

      <div className="mt-2 bg-white">
        {[
          { icon: Globe, label: "多语言", sub: "简体中文" },
          { icon: CircleHelp, label: "帮助与反馈", sub: "" },
          { icon: FileText, label: "关于微信", sub: "Version 8.0.62" },
        ].map((row) => <MobileCell key={row.label} icon={<MobileTile icon={<row.icon size={15} />} color="#9A9A9A" />} label={row.label} sublabel={row.sub || undefined} right={<span className="text-[9px] text-[#AAA]">{row.label === "多语言" ? "简体中文" : row.label === "关于微信" ? "8.0.62" : ""}</span>} onClick={() => onNotify(`${row.label}`)} />)}
      </div>

      <div className="mt-2 bg-white">
        <button onClick={() => onNotify("切换账号")} className="w-full border-b border-[#F3F3F3] py-3.5 text-center text-[12px] text-[#222]">切换账号</button>
        <button onClick={() => onNotify("退出登录（演示）")} className="w-full py-3.5 text-center text-[12px] text-[#D95353]">退出登录</button>
      </div>
      <div className="mt-4 text-center text-[8px] leading-4 text-[#B2B2B2]">微信 · 一个生活方式<br />本界面仅用于设计研究</div>
    </div>
  );
}

export function ProfileScreen({ contactId, onOpenChat, onBack, onNotify }: { contactId: string; onOpenChat: (id: string) => void; onBack: () => void; onNotify: (s: string) => void }) {
  const person: Person = contacts.find((c) => c.id === contactId) || contacts[1];
  return (
    <div className="scrollbar-thin h-full overflow-y-auto bg-[#F5F5F5] pb-7">
      <div className="relative overflow-hidden bg-gradient-to-br from-[#E9F3EC] via-[#F4F2EA] to-[#E5EEE7] px-4 pb-5 pt-[30px]">
        <div className="absolute -right-10 -top-8 h-32 w-32 rounded-full border-[20px] border-white/40" />
        <div className="relative z-10 flex h-[44px] items-center justify-between"><PhoneBack onClick={onBack} /><button onClick={() => onNotify("更多资料设置")} className="grid h-8 w-8 place-items-center"><MoreHorizontal size={20} /></button></div>
        <div className="relative z-10 mt-4 flex items-start gap-4">
          <Avatar src={person.avatar} name={person.name} className="h-[64px] w-[64px] rounded-lg shadow-md" online={person.online} />
          <div className="min-w-0 flex-1 pt-0.5"><h1 className="text-[19px] font-medium tracking-wide text-[#24352C]">{person.name}</h1><div className="mt-1.5 text-[9px] text-[#8C988F]">微信号：wxid_{person.id}26</div><div className="mt-1 text-[9px] text-[#8C988F]">{person.region} · {person.online ? "在线" : "2 小时前活跃"}</div></div>
        </div>
        <div className="relative z-10 mt-4 flex items-center gap-2 rounded-lg bg-white/70 px-3 py-2"><span className="text-[9.5px] text-[#6C7C71]">备注</span><span className="flex-1 text-[11px] text-[#3A4A3E]">{person.note}</span><button onClick={() => onNotify("编辑备注和标签")} className="text-[#8D9A90]"><ChevronRight size={13} /></button></div>
      </div>

      <div className="flex gap-2.5 bg-white p-3">
        <button onClick={() => onOpenChat(person.id)} className="flex flex-1 items-center justify-center gap-1.5 rounded-lg bg-[#07C160] py-2.5 text-[11px] font-medium text-white"><MessageCircle size={15} /> 发消息</button>
        <button onClick={() => onNotify(`正在呼叫 ${person.name}`)} className="flex flex-1 items-center justify-center gap-1.5 rounded-lg bg-[#F5F5F5] py-2.5 text-[11px] text-[#333]"><Video size={15} /> 音视频通话</button>
      </div>

      <div className="mt-2 bg-white">
        {[
          { icon: UserRound, label: "朋友权限", right: "聊天、朋友圈" },
          { icon: Star, label: "标签", right: "朋友、设计" },
          { icon: FileText, label: "描述", right: "通过群聊添加" },
          { icon: ImageIcon, label: "来源", right: "春日设计小组" },
        ].map((row, i) => <MobileCell key={row.label} icon={<span className="grid h-6 w-6 place-items-center text-[#888]"><row.icon size={15} /></span>} label={row.label} right={<span className="text-[9.5px] text-[#A6A6A6]">{row.right}</span>} border={i < 3} onClick={() => onNotify(`${row.label}详情`)} />)}
      </div>

      <div className="mt-2 bg-white p-4">
        <div className="mb-3 flex items-center justify-between"><span className="text-[11px] font-medium text-[#333]">朋友圈</span><button onClick={() => onNavigateMomentsFallback(onNotify)} className="text-[8.5px] text-[#576B95]">全部 24 张</button></div>
        <div className="grid grid-cols-4 gap-1">
          {[assets.gallery, assets.dawn, assets.coffee, assets.tulips].map((img, i) => <button key={i} onClick={() => onNotify("查看朋友圈图片")}><img src={img} alt="" className="aspect-square w-full rounded-sm object-cover" loading="lazy" /></button>)}
        </div>
      </div>

      <div className="mt-2 bg-white">
        <div className="flex items-center justify-between border-b border-[#F3F3F3] px-4 py-3"><span className="text-[11px] font-medium text-[#333]">共同群聊</span><span className="text-[8.5px] text-[#AAA]">4 个</span></div>
        {["春日设计小组", "周末去哪儿 · 2", "Studio 2026"].map((name, i) => <MobileCell key={name} icon={<Avatar src={[assets.gallery, assets.rocks, assets.desk][i]} name={name} className="h-8 w-8 rounded-md" />} label={name} border={i < 2} onClick={() => onNotify(`打开群聊：${name}`)} />)}
      </div>
      <div className="mt-2 flex gap-2 px-3">
        <button onClick={() => onNotify("已发送名片")} className="flex-1 rounded-lg bg-white py-2.5 text-[10px] text-[#555]">发送名片</button>
        <button onClick={() => onNotify("已设置星标朋友")} className="flex-1 rounded-lg bg-white py-2.5 text-[10px] text-[#555]">设为星标朋友</button>
        <button onClick={() => onNotify("更多操作")} className="grid w-11 place-items-center rounded-lg bg-white text-[#777]"><MoreHorizontal size={16} /></button>
      </div>
    </div>
  );
}

function onNavigateMomentsFallback(notify: (s: string) => void) {
  notify("正在查看 TA 的朋友圈（仅展示共同可见内容）");
}

export function NewFriendsScreen({ onNavigate, onNotify }: BaseProps) {
  const [accepted, setAccepted] = useState<string[]>([]);
  return (
    <div className="flex h-full flex-col bg-[#F5F5F5]">
      <MobileTop title="新的朋友" onBack={() => onNavigate("contacts")} right={<><Search size={18} /><Plus size={20} /></>} />
      <div className="shrink-0 border-b border-[#E9E9E9] bg-white px-3 py-2.5">
        <div className="flex h-8 items-center gap-1.5 rounded-md bg-[#F5F5F5] px-2.5 text-[#AAA]"><Search size={13} /><span className="text-[10px]">搜索微信号 / 手机号</span></div>
      </div>
      <div className="flex-1 overflow-y-auto">
        <div className="bg-white">
          <div className="border-b border-[#F3F3F3] px-4 py-2.5 text-[9px] font-medium text-[#999]">新的朋友 · 3</div>
          {[contacts[4], contacts[5], contacts[7]].map((person) => {
            const isAccepted = accepted.includes(person.id);
            return (
              <div key={person.id} className="flex items-center gap-3 border-b border-[#F5F5F5] px-4 py-3">
                <Avatar src={person.avatar} name={person.name} className="h-10 w-10 rounded-md" />
                <div className="min-w-0 flex-1"><div className="text-[12px] text-[#222]">{person.name}</div><div className="mt-0.5 truncate text-[8.5px] text-[#AAA]">我是 {person.id === "jiang" ? "江屿" : "通过「春日设计小组」添加"} · {person.region}</div></div>
                <button onClick={() => { setAccepted((v) => isAccepted ? v.filter((id) => id !== person.id) : [...v, person.id]); onNotify(isAccepted ? "已忽略好友申请" : `已添加 ${person.name} 为朋友`); }} className={cn("shrink-0 rounded-md px-2.5 py-1.5 text-[9px]", isAccepted ? "border border-[#E5E5E5] bg-white text-[#888]" : "bg-[#07C160] text-white")}>{isAccepted ? "已添加" : "接受"}</button>
              </div>
            );
          })}
          <div className="px-4 py-2.5 text-[8.5px] text-[#AAA]">当对方通过你的朋友验证后，即可开始聊天。</div>
        </div>
        <div className="mt-2 bg-white">
          <MobileCell icon={<span className="grid h-8 w-8 place-items-center rounded-md bg-[#F5B640] text-white"><Search size={15} /></span>} label="添加朋友" sublabel="搜索微信号、手机号或 QQ 号" onClick={() => onNotify("添加朋友页面")} />
          <MobileCell icon={<span className="grid h-8 w-8 place-items-center rounded-md bg-[#4C8EDB] text-white"><QrCode size={15} /></span>} label="扫一扫" sublabel="扫描二维码名片" onClick={() => onNavigate("scan")} />
          <MobileCell icon={<span className="grid h-8 w-8 place-items-center rounded-md bg-[#07B25A] text-white"><UserRound size={15} /></span>} label="群聊" sublabel="加入或创建群聊" onClick={() => onNotify("群聊列表")} />
        </div>
        <div className="mt-2 bg-white">
          <div className="border-b border-[#F3F3F3] px-4 py-2.5 text-[9px] text-[#999]">最近添加</div>
          {[contacts[1], contacts[2], contacts[6]].map((person) => <MobileCell key={person.id} icon={<Avatar src={person.avatar} name={person.name} className="h-8 w-8 rounded-md" />} label={person.name} sublabel={person.note} onClick={() => onNotify(`查看 ${person.name} 的资料`)} />)}
        </div>
      </div>
    </div>
  );
}

export function CallScreen({ onNavigate, onNotify }: BaseProps) {
  const [muted, setMuted] = useState(false);
  const [camera, setCamera] = useState(true);
  const [speaker, setSpeaker] = useState(true);
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => setSeconds((s) => s + 1), 1000);
    return () => clearInterval(timer);
  }, []);
  const format = `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
  return (
    <div className="relative flex h-full flex-col overflow-hidden bg-[#1B2621] pt-[30px] text-white">
      <img src={assets.lake} alt="" className={cn("absolute inset-0 h-full w-full object-cover transition", camera ? "opacity-80" : "opacity-100 blur-xl ms-[-10px]")} />
      <div className="absolute inset-0 bg-gradient-to-b from-black/55 via-black/10 to-black/80" />
      <div className="relative z-10 flex h-[44px] items-center justify-between px-4"><span className="flex items-center gap-1.5 rounded-full bg-black/20 px-2.5 py-1 text-[9px] backdrop-blur"><span className="h-1.5 w-1.5 rounded-full bg-[#07C160]" /> 视频通话中</span><button onClick={() => onNotify("通话设置")}><MoreHorizontal size={20} /></button></div>
      <div className="relative z-10 flex flex-1 flex-col items-center justify-center pb-8">
        <div className="relative">
          <Avatar src={px(14664508, 200, 200)} name="林小满" className="h-[92px] w-[92px] rounded-full border-2 border-white/30 shadow-2xl" />
          <span className="absolute bottom-1 right-1 h-3.5 w-3.5 rounded-full border-2 border-[#1B2621] bg-[#07C160]" />
        </div>
        <div className="mt-4 text-[19px] font-medium">林小满</div>
        <div className="mt-1.5 text-[11px] tracking-wider text-white/75">{format}</div>
        <div className="mt-1 text-[8.5px] text-white/50">端到端加密，通话内容仅你与对方可见</div>
      </div>
      <div className="absolute right-4 top-[92px] z-10 h-[148px] w-[104px] overflow-hidden rounded-xl border border-white/20 bg-black/20 shadow-xl">
        <img src={me.avatar} alt="我" className="h-full w-full object-cover" />
        <span className="absolute bottom-1.5 left-2 text-[8px] text-white/80">我</span>
      </div>
      <div className="relative z-10 px-6 pb-7">
        <div className="mb-7 grid grid-cols-3 gap-4">
          {[
            { label: muted ? "已静音" : "静音", icon: muted ? MicOff : Mic, on: muted, toggle: () => { setMuted(!muted); onNotify(muted ? "麦克风已开启" : "麦克风已静音"); } },
            { label: camera ? "关闭摄像头" : "开启摄像头", icon: camera ? Video : VideoOff, on: !camera, toggle: () => { setCamera(!camera); onNotify(camera ? "摄像头已关闭" : "摄像头已开启"); } },
            { label: "扬声器", icon: Volume2, on: speaker, toggle: () => { setSpeaker(!speaker); onNotify(speaker ? "已切换至听筒" : "已开启扬声器"); } },
          ].map((item) => (
            <button key={item.label} onClick={item.toggle} className="flex flex-col items-center gap-2"><span className={cn("grid h-12 w-12 place-items-center rounded-full backdrop-blur transition", item.on ? "bg-white text-[#222]" : "bg-white/15 text-white")}><item.icon size={21} /></span><span className="text-[8.5px] text-white/80">{item.label}</span></button>
          ))}
        </div>
        <div className="flex justify-center gap-10">
          <button onClick={() => onNotify("已切换到后置摄像头")} className="grid h-12 w-12 place-items-center rounded-full bg-white/15"><Camera size={21} /></button>
          <button onClick={() => { onNotify("通话已结束 · 时长 " + format); onNavigate("chat"); }} className="grid h-14 w-14 place-items-center rounded-full bg-[#FA5151] shadow-lg shadow-black/20"><PhoneOff size={23} /></button>
          <button onClick={() => onNotify("通话最小化")} className="grid h-12 w-12 place-items-center rounded-full bg-white/15"><Share2 size={21} /></button>
        </div>
      </div>
    </div>
  );
}
