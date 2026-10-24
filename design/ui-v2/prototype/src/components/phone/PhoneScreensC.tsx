import { useState } from "react";
import {
  BadgeCheck,
  Camera,
  Check,
  ChevronDown,
  Delete,
  Heart,
  Image as ImageIcon,
  LocateFixed,
  MapPin,
  MoreHorizontal,
  Navigation,
  Plus,
  Search,
  Share2,
  ShieldCheck,
  X,
} from "lucide-react";
import { assets, contacts, me, nearbyPlaces, officialAccounts, px, statuses, transferPresets } from "../../data";
import { Avatar, WeChatLogo } from "../shared";
import { cn } from "../../utils/cn";
import { GroupTitle, MobileTop, ScreenShell } from "./atoms";
import type { MobileScreen } from "./types";

type BaseProps = { onNavigate: (screen: MobileScreen) => void; onNotify: (message: string) => void };

/* ================= 登录 ================= */
export function LoginScreen({ onNavigate, onNotify }: BaseProps) {
  const [phone, setPhone] = useState("138 0000 0000");
  const [agreed, setAgreed] = useState(false);
  const [codeSent, setCodeSent] = useState(false);
  return (
    <ScreenShell className="bg-white">
      <div className="flex h-[30px] shrink-0" />
      <div className="flex items-center justify-between px-4 pt-1">
        <button onClick={() => onNavigate("me")} className="grid h-8 w-8 place-items-center rounded-full bg-[#F5F5F5]"><X size={15} /></button>
        <button onClick={() => onNotify("已切换语言：English")} className="text-[10px] text-[#576B95]">简体中文 ▾</button>
      </div>
      <div className="flex flex-col items-center px-8 pt-8">
        <div className="relative">
          <WeChatLogo className="h-[68px] w-[68px]" />
          <span className="absolute -right-1 -top-1 grid h-5 w-5 place-items-center rounded-full bg-[#07C160] text-white ring-2 ring-white"><Check size={11} strokeWidth={3} /></span>
        </div>
        <h1 className="mt-5 text-[24px] font-semibold tracking-[-0.01em] text-[#1a1a1a]">欢迎回到微信</h1>
        <p className="mt-2 text-center text-[11px] leading-5 text-[#999]">一个生活方式<br />连接 12 亿人的日常</p>
      </div>
      <div className="mt-8 px-8">
        <div className="text-[10px] font-medium text-[#888]">手机号登录</div>
        <div className="mt-2 flex items-center gap-2 border-b border-[#E8E8E8] pb-2.5 focus-within:border-[#07C160]">
          <button className="flex items-center gap-1 text-[15px] text-[#222]">+86 <ChevronDown size={13} className="text-[#aaa]" /></button>
          <span className="h-4 w-px bg-[#E5E5E5]" />
          <input
            value={phone}
            onChange={(e) => setPhone(e.target.value)}
            inputMode="tel"
            className="flex-1 bg-transparent text-[16px] tracking-wide text-[#1a1a1a] outline-none tabular-nums"
          />
          {phone && <button onClick={() => setPhone("")}><X size={13} className="rounded-full bg-[#E5E5E5] p-[2px] text-[#999]" /></button>}
        </div>
        <div className="mt-4 flex items-center gap-2 border-b border-[#E8E8E8] pb-2.5">
          <input placeholder="填写验证码" className="flex-1 bg-transparent text-[14px] outline-none placeholder:text-[#C2C2C2]" />
          <button
            onClick={() => { setCodeSent(true); onNotify(codeSent ? "验证码已重新发送" : "验证码已发送"); }}
            className={cn("rounded-full px-3 py-1.5 text-[10px]", codeSent ? "bg-[#F0F0F0] text-[#999]" : "bg-[#E9F8EF] text-[#07A858]")}
          >
            {codeSent ? "重新发送 58s" : "获取验证码"}
          </button>
        </div>
        <button
          onClick={() => {
            if (!agreed) { onNotify("请先阅读并同意服务协议"); return; }
            onNotify("登录成功，欢迎回来，苏禾 ✦");
            onNavigate("chats");
          }}
          className={cn(
            "pressable mt-6 w-full rounded-[10px] py-3.5 text-[15px] font-medium transition",
            agreed ? "bg-[#07C160] text-white shadow-[0_8px_20px_rgba(7,193,96,0.3)]" : "bg-[#F0F0F0] text-[#B5B5B5]",
          )}
        >
          登录
        </button>
        <button onClick={() => onNotify("更多登录方式：微信号 / QQ / 邮箱")} className="mt-3 w-full text-center text-[11px] text-[#576B95]">更多登录方式</button>
        <label className="mt-6 flex items-start gap-2">
          <button
            onClick={() => setAgreed(!agreed)}
            className={cn("mt-0.5 grid h-[15px] w-[15px] shrink-0 place-items-center rounded-full border", agreed ? "border-[#07C160] bg-[#07C160] text-white" : "border-[#C8C8C8] text-transparent")}
          >
            <Check size={10} strokeWidth={3} />
          </button>
          <span className="text-[9.5px] leading-4 text-[#999]">
            已阅读并同意<span className="text-[#576B95]">《微信软件许可及服务协议》《隐私保护指引》</span>与<span className="text-[#576B95]">《儿童隐私保护指引》</span>
          </span>
        </label>
      </div>
      <div className="mt-auto flex items-center justify-center gap-1.5 pb-8 text-[8px] text-[#B5B5B5]">
        <ShieldCheck size={10} /> 微信安全中心保护中
      </div>
    </ScreenShell>
  );
}

/* ================= 我的状态 ================= */
export function StatusScreen({ onNavigate, onNotify }: BaseProps) {
  const [mine, setMine] = useState<string | null>("s1");
  return (
    <ScreenShell className="bg-[#F5F5F5]">
      <MobileTop title="我的状态" onBack={() => onNavigate("me")} right={<Search size={18} />} />
      <div className="min-h-0 flex-1 overflow-y-auto pb-6">
        <div className="relative overflow-hidden bg-gradient-to-br from-[#1E3A2A] via-[#254A34] to-[#1E3A2A] px-4 pb-5 pt-5 text-white">
          <div className="pointer-events-none absolute -right-10 -top-10 h-40 w-40 rounded-full bg-[#07C160]/20 blur-2xl" />
          <div className="flex items-center gap-3">
            <Avatar src={me.avatar} name={me.name} className="h-12 w-12 rounded-xl" />
            <div className="flex-1">
              <div className="text-[14px] font-medium">{me.name}</div>
              <div className="mt-1 flex items-center gap-1.5 text-[10px] text-white/70">
                <span className="text-[13px]">{mine ? statuses.find((s) => s.id === mine)?.emoji : "💭"}</span>
                {mine ? statuses.find((s) => s.id === mine)?.label : "还没有设置状态"}
              </div>
            </div>
            <button onClick={() => onNotify("更换状态背景")} className="grid h-8 w-8 place-items-center rounded-full bg-white/15"><Camera size={15} /></button>
          </div>
          <div className="mt-4 flex gap-2">
            <button onClick={() => onNotify("拍照分享此刻")} className="flex flex-1 items-center justify-center gap-1.5 rounded-full bg-white py-2 text-[10px] font-medium text-[#1E3A2A]"><Camera size={12} /> 拍一个状态</button>
            <button onClick={() => onNotify("从相册选择")} className="flex flex-1 items-center justify-center gap-1.5 rounded-full bg-white/15 py-2 text-[10px] text-white"><ImageIcon size={12} /> 从相册选</button>
          </div>
        </div>

        <GroupTitle action={<span className="text-[8.5px] text-[#AAA]">设一个状态，朋友们会看到</span>}>选择状态</GroupTitle>
        <div className="grid grid-cols-3 gap-2 px-3">
          {statuses.map((s) => {
            const active = mine === s.id;
            return (
              <button
                key={s.id}
                onClick={() => { setMine(active ? null : s.id); onNotify(active ? "已清除状态" : `状态已设为「${s.label}」`); }}
                className={cn(
                  "pressable rounded-xl border bg-white p-3 text-left transition",
                  active ? "border-[#07C160] shadow-[0_6px_18px_rgba(7,193,96,0.15)] ring-1 ring-[#07C160]/30" : "border-transparent",
                )}
              >
                <div className="text-[26px]">{s.emoji}</div>
                <div className="mt-2 text-[10.5px] font-medium text-[#333]">{s.label}</div>
                <div className="mt-0.5 line-clamp-1 text-[8px] text-[#AAA]">{s.desc}</div>
                {active && <div className="mt-1.5 flex items-center gap-1 text-[8px] text-[#07A858]"><Check size={9} strokeWidth={3} /> 使用中</div>}
              </button>
            );
          })}
        </div>

        <GroupTitle action={<span className="text-[8.5px] text-[#AAA]">刚刚更新</span>}>朋友的状态</GroupTitle>
        <div className="mx-3 overflow-hidden rounded-xl bg-white">
          {[
            [contacts[1], "🌿", "在春天里", "2 分钟前"],
            [contacts[3], "📷", "拍照", "18 分钟前"],
            [contacts[5], "☕", "喝咖啡", "1 小时前"],
            [contacts[4], "✈️", "在路上", "3 小时前"],
          ].map(([p, emoji, label, time]: any) => (
            <button key={p.id} onClick={() => onNotify(`查看 ${p.name} 的状态`)} className="flex w-full items-center gap-2.5 border-b border-[#F6F6F6] px-3 py-2.5 text-left last:border-0">
              <span className="relative">
                <Avatar src={p.avatar} name={p.name} className="h-9 w-9 rounded-full" />
                <span className="absolute -bottom-0.5 -right-0.5 grid h-5 w-5 place-items-center rounded-full bg-white text-[11px] shadow ring-1 ring-black/5">{emoji}</span>
              </span>
              <span className="flex-1"><span className="block text-[11px] text-[#333]">{p.name}</span><span className="mt-0.5 block text-[8.5px] text-[#AAA]">{label} · {time}</span></span>
              <Heart size={14} className="text-[#D5D5D5]" />
            </button>
          ))}
        </div>
      </div>
    </ScreenShell>
  );
}

/* ================= 转账 ================= */
export function TransferScreen({ onNavigate, onNotify }: BaseProps) {
  const [amount, setAmount] = useState("88.88");
  const [note, setNote] = useState("展览门票 AA");
  const [confirm, setConfirm] = useState(false);
  const person = contacts[1];
  const press = (k: string) => {
    if (k === "del") { setAmount((a) => a.slice(0, -1)); return; }
    if (k === "." && amount.includes(".")) return;
    if (amount.replace(".", "").length >= 7) return;
    setAmount((a) => (a === "0" && k !== "." ? k : a + k));
  };
  return (
    <ScreenShell className="bg-[#F5F5F5]">
      <MobileTop title="转账" onBack={() => onNavigate("chat")} right={<MoreHorizontal size={19} />} />
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="flex flex-col items-center bg-white px-6 pb-6 pt-6">
          <Avatar src={person.avatar} name={person.name} className="h-11 w-11 rounded-lg" />
          <div className="mt-2 text-[13px] font-medium text-[#222]">转账给 {person.name}</div>
          <div className="mt-1 text-[8.5px] text-[#AAA]">微信号 wxid_lin26 · 已实名</div>
          <div className="mt-5 flex w-full items-center justify-center gap-1 border-b border-[#EFEFEF] pb-3">
            <span className="text-[26px] font-medium text-[#1a1a1a]">¥</span>
            <span className="min-w-[40px] text-center text-[40px] font-medium leading-none tracking-tight text-[#1a1a1a] tabular-nums">
              {amount || <span className="text-[#D5D5D5]">0.00</span>}
            </span>
            <span className="h-8 w-[2px] animate-pulse bg-[#07C160]" />
          </div>
          <div className="mt-3 flex w-full items-center gap-2 rounded-lg bg-[#F7F7F7] px-3 py-2">
            <span className="text-[10px] text-[#999]">添加转账说明</span>
            <input value={note} onChange={(e) => setNote(e.target.value)} className="min-w-0 flex-1 bg-transparent text-[11px] text-[#333] outline-none" maxLength={20} />
            <span className="text-[8px] text-[#C5C5C5]">{note.length}/20</span>
          </div>
          <div className="mt-3 flex w-full flex-wrap gap-1.5">
            {transferPresets.map((p) => (
              <button key={p} onClick={() => setAmount(p)} className={cn("rounded-full border px-2.5 py-1 text-[9px]", amount === p ? "border-[#07C160] bg-[#EFFBF3] text-[#07A858]" : "border-[#EAEAEA] text-[#888]")}>¥{p}</button>
            ))}
          </div>
          <button
            onClick={() => { if (!amount || Number(amount) <= 0) { onNotify("请输入转账金额"); return; } setConfirm(true); }}
            className="pressable mt-5 w-full rounded-[10px] bg-[#07C160] py-3 text-[14px] font-medium text-white shadow-[0_8px_20px_rgba(7,193,96,0.3)]"
          >
            转账 ¥{amount || "0.00"}
          </button>
          <div className="mt-3 flex items-center gap-1 text-[8px] text-[#B5B5B5]"><ShieldCheck size={10} /> 资金由财付通提供安全保障 · 实时到账</div>
        </div>
        <div className="mx-3 mt-2.5 rounded-xl bg-white p-3">
          <div className="text-[10px] font-medium text-[#555]">转账记录</div>
          {[
            [person.name, "展览门票 AA", "-88.00", "昨天"],
            ["Anna Chen", "设计稿尾款", "+1,280.00", "7月16日"],
          ].map(([n, d, a, t], i) => (
            <div key={i} className="mt-2 flex items-center justify-between border-t border-[#F6F6F6] pt-2">
              <div><div className="text-[10.5px] text-[#333]">{n}</div><div className="mt-0.5 text-[8px] text-[#AAA]">{d} · {t}</div></div>
              <span className={cn("text-[11px] font-medium tabular-nums", String(a).startsWith("+") ? "text-[#07A858]" : "text-[#333]")}>{a}</span>
            </div>
          ))}
        </div>
      </div>
      {/* custom keypad */}
      <div className="shrink-0 border-t border-[#E5E5E5] bg-[#F0F0F0] pb-5">
        <div className="grid grid-cols-4">
          {["1", "2", "3", "del", "4", "5", "6", "0", "7", "8", "9", "."].map((k, i) => (
            <button
              key={i}
              onClick={() => k === "del" ? press("del") : press(k)}
              className={cn(
                "flex h-[44px] items-center justify-center border-b border-r border-[#E2E2E2] text-[17px] text-[#222] last:border-r-0 active:bg-[#E4E4E4]",
                k === "del" && "bg-[#E8E8E8]",
              )}
            >
              {k === "del" ? <Delete size={19} className="text-[#555]" /> : k}
            </button>
          ))}
        </div>
      </div>
      {confirm && (
        <div className="absolute inset-0 z-40 flex items-center justify-center bg-black/40 p-8 backdrop-blur-[2px]" onClick={() => setConfirm(false)}>
          <div className="pop-in w-full overflow-hidden rounded-2xl bg-white" onClick={(e) => e.stopPropagation()}>
            <div className="px-5 pb-4 pt-5 text-center">
              <div className="text-[12px] text-[#666]">转账给</div>
              <div className="mt-1 text-[14px] font-medium">{person.name}</div>
              <div className="mt-3 text-[34px] font-semibold tracking-tight tabular-nums">¥{amount}</div>
              <div className="mx-auto mt-3 flex max-w-[220px] items-center justify-between rounded-lg bg-[#F7F7F7] px-3 py-2 text-[9px] text-[#888]">
                <span>零钱</span><span className="text-[#576B95]">更换 ›</span>
              </div>
            </div>
            <div className="flex border-t border-[#EFEFEF]">
              <button onClick={() => setConfirm(false)} className="flex-1 py-3 text-[13px] text-[#333]">取消</button>
              <span className="w-px bg-[#EFEFEF]" />
              <button onClick={() => { setConfirm(false); onNotify(`已转账 ¥${amount} 给 ${person.name}`); onNavigate("chat"); }} className="flex-1 py-3 text-[13px] font-medium text-[#07A858]">确认转账</button>
            </div>
          </div>
        </div>
      )}
    </ScreenShell>
  );
}

/* ================= 位置共享 ================= */
export function LocationScreen({ onNavigate, onNotify }: BaseProps) {
  const [tab, setTab] = useState("发送位置");
  return (
    <ScreenShell className="bg-[#F5F5F5]">
      <MobileTop title="位置" subtitle="杭州市西湖区 · 天目里" onBack={() => onNavigate("chat")} right={<MoreHorizontal size={19} />} />
      <div className="relative h-[240px] shrink-0 overflow-hidden bg-[#E8EDE6]">
        {/* stylized map */}
        <div className="absolute inset-0" style={{ backgroundImage: "linear-gradient(#D8DFD5 1px, transparent 1px), linear-gradient(90deg, #D8DFD5 1px, transparent 1px)", backgroundSize: "26px 26px" }} />
        <svg className="absolute inset-0 h-full w-full" viewBox="0 0 320 240" preserveAspectRatio="none">
          <path d="M-10 150 C60 130, 120 170, 180 140 S 300 120, 340 150" stroke="#fff" strokeWidth="14" fill="none" />
          <path d="M-10 150 C60 130, 120 170, 180 140 S 300 120, 340 150" stroke="#CBD3C6" strokeWidth="15" fill="none" opacity="0.4" />
          <path d="M80 -10 C90 80, 70 160, 90 250" stroke="#fff" strokeWidth="10" fill="none" />
          <path d="M230 -10 C220 70, 240 150, 225 250" stroke="#fff" strokeWidth="8" fill="none" />
          <ellipse cx="160" cy="105" rx="46" ry="30" fill="#BFD9C4" opacity="0.7" />
          <ellipse cx="60" cy="60" rx="26" ry="18" fill="#CFE3D2" opacity="0.8" />
        </svg>
        <div className="absolute left-1/2 top-[92px] -translate-x-1/2 -translate-y-full">
          <span className="relative block">
            <span className="absolute -inset-4 animate-ping rounded-full bg-[#07C160]/25" />
            <MapPin size={34} className="relative fill-[#07C160] text-white drop-shadow-lg" />
          </span>
        </div>
        <div className="absolute bottom-3 left-3 right-3 flex items-center gap-2 rounded-xl bg-white/95 px-3 py-2.5 shadow-lg backdrop-blur">
          <span className="grid h-8 w-8 place-items-center rounded-lg bg-[#E9F8EF] text-[#07A858]"><Navigation size={15} /></span>
          <div className="flex-1"><div className="text-[11px] font-medium text-[#222]">天目里美术馆</div><div className="mt-0.5 text-[8.5px] text-[#999]">杭州市西湖区天目山路 398 号</div></div>
          <button onClick={() => onNotify("正在导航前往")} className="rounded-full bg-[#07C160] px-3 py-1.5 text-[9px] font-medium text-white">导航</button>
        </div>
        <button onClick={() => onNotify("已定位到当前位置")} className="absolute right-3 top-3 grid h-8 w-8 place-items-center rounded-full bg-white shadow"><LocateFixed size={16} className="text-[#666]" /></button>
      </div>
      <div className="flex shrink-0 gap-2 bg-white px-3 py-2.5">
        {["发送位置", "共享实时位置"].map((t) => (
          <button key={t} onClick={() => setTab(t)} className={cn("flex-1 rounded-lg py-2 text-[10.5px] font-medium", tab === t ? "bg-[#07C160] text-white shadow-[0_4px_12px_rgba(7,193,96,0.3)]" : "bg-[#F3F3F3] text-[#666]")}>{t}</button>
        ))}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="flex h-8 items-center gap-1.5 bg-white px-4 text-[#AAA]"><Search size={12} /><span className="text-[10px]">搜索地点</span></div>
        {nearbyPlaces.map((p, i) => (
          <button key={p.id} onClick={() => onNotify(`已选择：${p.name}`)} className="flex w-full items-center gap-3 bg-white px-4 py-3 text-left">
            <span className={cn("grid h-8 w-8 shrink-0 place-items-center rounded-lg text-[11px] font-semibold", i === 0 ? "bg-[#E9F8EF] text-[#07A858]" : "bg-[#F5F5F5] text-[#888]")}>{i + 1}</span>
            <span className="min-w-0 flex-1 border-b border-[#F5F5F5] pb-3"><span className="block text-[11.5px] text-[#222]">{p.name}</span><span className="mt-0.5 block truncate text-[8.5px] text-[#AAA]">{p.addr}</span></span>
            <span className="rounded bg-[#F3F3F3] px-1.5 py-0.5 text-[7.5px] text-[#888]">{p.tag}</span>
          </button>
        ))}
      </div>
      <div className="shrink-0 bg-white px-4 pb-6 pt-2">
        <button onClick={() => { onNotify(tab === "发送位置" ? "位置已发送到聊天" : "已发起实时位置共享"); onNavigate("chat"); }} className="w-full rounded-[10px] bg-[#07C160] py-3 text-[13px] font-medium text-white">发送</button>
      </div>
    </ScreenShell>
  );
}

/* ================= 公众号 ================= */
export function OfficialScreen({ onNavigate, onNotify }: BaseProps) {
  return (
    <ScreenShell className="bg-[#F5F5F5]">
      <MobileTop title="订阅号消息" onBack={() => onNavigate("contacts")} right={<><Search size={18} /><MoreHorizontal size={19} /></>} />
      <div className="min-h-0 flex-1 overflow-y-auto pb-6">
        <div className="flex gap-3 overflow-x-auto bg-white px-4 py-3 no-scrollbar">
          {officialAccounts.map((o) => (
            <button key={o.id} onClick={() => onNotify(`打开「${o.name}」`)} className="flex w-[62px] shrink-0 flex-col items-center gap-1.5">
              <span className="relative">
                {o.avatar ? <img src={o.avatar} alt="" className="h-12 w-12 rounded-full object-cover ring-1 ring-black/5" /> : <span className="grid h-12 w-12 place-items-center rounded-full bg-[#07C160]"><WeChatLogo mono className="h-6 w-6 text-white" /></span>}
                {!!o.unread && <span className="absolute -right-0.5 -top-0.5 grid h-4 min-w-4 place-items-center rounded-full bg-[#FA5151] px-1 text-[8px] text-white ring-2 ring-white">{o.unread}</span>}
              </span>
              <span className="w-full truncate text-center text-[9px] text-[#555]">{o.name}</span>
            </button>
          ))}
          <button onClick={() => onNotify("查看全部订阅号")} className="flex w-[62px] shrink-0 flex-col items-center gap-1.5">
            <span className="grid h-12 w-12 place-items-center rounded-full bg-[#F3F3F3] text-[#888]"><Plus size={18} /></span>
            <span className="text-[9px] text-[#888]">添加</span>
          </button>
        </div>
        <div className="mt-2.5 space-y-2.5 px-3">
          <button onClick={() => onNavigate("reading")} className="block w-full overflow-hidden rounded-xl bg-white text-left shadow-sm">
            <img src={assets.gallery} alt="" className="h-[150px] w-full object-cover" />
            <div className="p-3.5">
              <div className="text-[13px] font-medium leading-5 text-[#1a1a1a]">在风经过的地方，重新理解日常</div>
              <div className="mt-1.5 line-clamp-2 text-[10px] leading-4 text-[#888]">清晨六点半，天光刚刚漫过山的轮廓。湖面还没有醒，风从很远的地方来……</div>
              <div className="mt-2.5 flex items-center gap-2">
                <img src={px(20894157, 60, 60)} alt="" className="h-5 w-5 rounded-full object-cover" />
                <span className="text-[9px] text-[#888]">设计手记</span>
                <span className="ml-auto text-[8.5px] text-[#B5B5B5]">12:30 · 阅读 2,318</span>
              </div>
            </div>
          </button>
          {[
            [assets.dawn, "「春日回声」展览开幕 · 限时预约中", "天目里美术馆", "昨天"],
            [assets.coffee, "城市咖啡地图：三家值得专程前往的手冲", "看理想", "周二"],
          ].map(([img, title, from, time]) => (
            <button key={title as string} onClick={() => onNavigate("reading")} className="flex w-full gap-3 rounded-xl bg-white p-3 text-left shadow-sm">
              <div className="min-w-0 flex-1">
                <div className="line-clamp-2 text-[12px] font-medium leading-5 text-[#222]">{title}</div>
                <div className="mt-2 text-[8.5px] text-[#AAA]">{from} · {time}</div>
              </div>
              <img src={img as string} alt="" className="h-[62px] w-[62px] shrink-0 rounded-lg object-cover" />
            </button>
          ))}
        </div>
      </div>
    </ScreenShell>
  );
}

/* ================= 我的二维码 ================= */
export function MyCodeScreen({ onNavigate, onNotify }: BaseProps) {
  const [style, setStyle] = useState(0);
  const styles = [
    { bg: "linear-gradient(135deg,#E9F3EA,#D5E7D8)", fg: "#1E3A2A", label: "春日绿" },
    { bg: "linear-gradient(135deg,#1E2A22,#33463A)", fg: "#EAF3EA", label: "墨绿" },
    { bg: "linear-gradient(135deg,#F6ECD8,#EBD3AC)", fg: "#5C4416", label: "暖阳" },
    { bg: "linear-gradient(135deg,#E4E9F5,#C9D4EC)", fg: "#2A3A5E", label: "雾蓝" },
  ];
  const s = styles[style];
  return (
    <ScreenShell className="bg-[#F5F5F5]">
      <MobileTop title="我的二维码" onBack={() => onNavigate("me")} right={<MoreHorizontal size={19} />} />
      <div className="flex min-h-0 flex-1 flex-col items-center overflow-y-auto px-6 pb-6 pt-4">
        <div className="w-full overflow-hidden rounded-2xl shadow-[0_16px_44px_rgba(30,50,38,0.16)]" style={{ background: s.bg }}>
          <div className="flex items-center gap-3 p-5" style={{ color: s.fg }}>
            <Avatar src={me.avatar} name={me.name} className="h-12 w-12 rounded-xl" />
            <div><div className="text-[15px] font-semibold">{me.name}</div><div className="mt-1 text-[9px] opacity-70">杭州 · {me.note}</div></div>
            <BadgeCheck size={18} className="ml-auto opacity-60" />
          </div>
          <div className="mx-5 rounded-2xl bg-white p-4">
            <div className="grid aspect-square w-full grid-cols-[repeat(23,minmax(0,1fr))]">
              {Array.from({ length: 529 }).map((_, i) => (
                <span key={i} className={(i * 7 + Math.floor(i / 23) * 13) % 5 < 2 ? "aspect-square bg-[#1a1a1a]" : "aspect-square bg-white"} />
              ))}
            </div>
            <div className="mx-auto -mt-[76%] grid h-[24%] w-[24%] place-items-center rounded-xl bg-white shadow ring-1 ring-black/5">
              <WeChatLogo className="h-[70%] w-[70%]" />
            </div>
          </div>
          <div className="p-5 text-center text-[9px]" style={{ color: s.fg, opacity: 0.75 }}>扫一扫上面的二维码，加我为朋友</div>
        </div>
        <div className="mt-4 text-[10px] text-[#888]">换个样式</div>
        <div className="mt-2 flex gap-2.5">
          {styles.map((st, i) => (
            <button
              key={st.label}
              onClick={() => { setStyle(i); onNotify(`已切换为「${st.label}」`); }}
              className={cn("h-10 w-10 rounded-full ring-2 ring-offset-2 transition", style === i ? "ring-[#07C160]" : "ring-transparent")}
              style={{ background: st.bg }}
              aria-label={st.label}
            />
          ))}
        </div>
        <div className="mt-5 flex w-full gap-2.5">
          <button onClick={() => onNotify("二维码已保存到相册")} className="flex flex-1 items-center justify-center gap-1.5 rounded-[10px] bg-white py-3 text-[11px] text-[#333] shadow-sm"><ImageIcon size={14} /> 保存图片</button>
          <button onClick={() => onNotify("分享名片")} className="flex flex-1 items-center justify-center gap-1.5 rounded-[10px] bg-[#07C160] py-3 text-[11px] font-medium text-white shadow-[0_6px_16px_rgba(7,193,96,0.3)]"><Share2 size={14} /> 分享名片</button>
        </div>
      </div>
    </ScreenShell>
  );
}
