import { useState } from "react";
import {
  ArrowUpRight,
  Bell,
  ChevronRight,
  CircleHelp,
  CreditCard,
  Eye,
  EyeOff,
  FileText,
  Gift,
  Heart,
  Image as ImageIcon,
  Keyboard,
  LockKeyhole,
  MessageCircle,
  Monitor,
  Moon,
  MoreHorizontal,
  Palette,
  Play,
  Plus,
  QrCode,
  ReceiptText,
  Search,
  Settings2,
  ShieldCheck,
  Sparkles,
  Wallet,
  X,
} from "lucide-react";
import { assets, me, miniApps, moments, transactions } from "../../data";
import { Avatar, Switch, WeChatLogo } from "../shared";
import { cn } from "../../utils/cn";

function SidebarHeader({ title, right }: { title: string; right?: React.ReactNode }) {
  return (
    <div className="flex h-[55px] shrink-0 items-center justify-between border-b border-[#E9E9E9] bg-[#F7F7F7] px-4">
      <h2 className="text-[15px] font-medium text-[#1F1F1F]">{title}</h2>
      <div className="flex items-center gap-2 text-[#555]">{right}</div>
    </div>
  );
}

function MiniSearch({ placeholder = "搜索" }: { placeholder?: string }) {
  return (
    <div className="flex h-[29px] items-center gap-1.5 rounded-md bg-[#E9E9E9]/80 px-2 text-[#A2A2A2]">
      <Search size={13} />
      <span className="text-[10.5px]">{placeholder}</span>
    </div>
  );
}

function FeedAvatarRow({ name, avatar, subtitle, onClick }: { name: string; avatar: string; subtitle: string; onClick?: () => void }) {
  return (
    <button onClick={onClick} className="flex w-full items-center gap-2.5 px-4 py-2.5 text-left transition hover:bg-[#EDEDED]">
      <Avatar src={avatar} name={name} className="h-8 w-8 rounded-[5px]" />
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[11.5px] text-[#333]">{name}</span>
        <span className="mt-0.5 block truncate text-[9px] text-[#A6A6A6]">{subtitle}</span>
      </span>
      <ChevronRight size={12} className="text-[#C3C3C3]" />
    </button>
  );
}

export function MomentsPage({ notify }: { notify: (s: string) => void }) {
  const [liked, setLiked] = useState<string[]>(["mo1"]);
  return (
    <div className="flex min-h-0 flex-1">
      <aside className="flex w-[258px] shrink-0 flex-col border-r border-[#E8E8E8] bg-[#F7F7F7]">
        <SidebarHeader title="朋友圈" right={<><button onClick={() => notify("发表新朋友圈")}><Plus size={16} /></button><button onClick={() => notify("朋友圈已刷新")}><MoreHorizontal size={16} /></button></>} />
        <div className="p-3"><MiniSearch placeholder="搜索朋友圈" /></div>
        <div className="mx-3 overflow-hidden rounded-xl bg-white ring-1 ring-black/[0.04]">
          <div className="relative h-[82px]">
            <img src={assets.cover} alt="朋友圈封面" className="h-full w-full object-cover" loading="lazy" />
            <img src={me.avatar} alt={me.name} className="absolute -bottom-4 right-3 h-11 w-11 rounded-md border-2 border-white object-cover" />
          </div>
          <div className="px-3 pb-2.5 pt-2 text-[12px] font-medium text-white/0 select-none">.</div>
          <div className="px-3 pb-3">
            <div className="text-[12px] font-medium text-[#2A2A2A]">{me.name}</div>
            <div className="mt-0.5 text-[9px] text-[#A6A6A6]">{me.note}</div>
          </div>
        </div>
        <div className="mt-4 flex items-center justify-between px-4 py-1.5">
          <span className="text-[9px] font-medium tracking-wider text-[#A6A6A6]">最近更新</span>
          <span className="text-[9px] text-[#B6B6B6]">3 位朋友</span>
        </div>
        <div className="space-y-0.5">
          {moments.map((m) => (
            <button key={m.id} onClick={() => notify(`正在查看 ${m.name} 的朋友圈`)} className="flex w-full items-center gap-2.5 px-4 py-2.5 text-left hover:bg-[#EDEDED]">
              <Avatar src={m.avatar} name={m.name} className="h-8 w-8 rounded-[5px]" />
              <span className="min-w-0 flex-1"><span className="block text-[11px] text-[#333]">{m.name}</span><span className="mt-0.5 block truncate text-[9px] text-[#A6A6A6]">{m.text}</span></span>
              {m.images?.[0] && <img src={m.images[0]} alt="" className="h-8 w-8 rounded-[3px] object-cover" />}
            </button>
          ))}
          <button onClick={() => notify("已打开朋友圈相册")} className="flex w-full items-center gap-2.5 px-4 py-2.5 text-left hover:bg-[#EDEDED]">
            <span className="grid h-8 w-8 place-items-center rounded-[5px] bg-[#E9E9E9] text-[#777]"><ImageIcon size={15} /></span>
            <span className="text-[11px] text-[#555]">我的朋友圈相册</span>
            <ChevronRight size={12} className="ml-auto text-[#C3C3C3]" />
          </button>
        </div>
        <div className="mt-auto border-t border-[#E9E9E9] p-3 text-[9px] leading-4 text-[#AAA]">朋友圈是生活的切片，<br />也是与朋友分享此刻的方式。</div>
      </aside>

      <section className="scrollbar-thin min-w-0 flex-1 overflow-y-auto bg-[#F7F7F7]">
        <div className="sticky top-0 z-10 flex h-[55px] items-center justify-between border-b border-[#E8E8E8] bg-[#F7F7F7]/95 px-6 backdrop-blur">
          <div><h2 className="text-[14px] font-medium text-[#222]">朋友圈</h2><p className="text-[9px] text-[#A6A6A6]">看看朋友们正在分享什么</p></div>
          <div className="flex items-center gap-1">
            <button onClick={() => notify("已切换朋友圈排序：最新")} className="rounded-md border border-[#E5E5E5] bg-white px-2.5 py-1.5 text-[9.5px] text-[#666]">最新</button>
            <button onClick={() => notify("正在刷新朋友圈")} className="grid h-7 w-7 place-items-center rounded-md border border-[#E5E5E5] bg-white text-[#777]"><Sparkles size={13} /></button>
          </div>
        </div>
        <div className="relative h-[200px] overflow-hidden">
          <img src={assets.cover} alt="朋友圈封面" className="h-full w-full object-cover" loading="lazy" />
          <div className="absolute inset-0 bg-gradient-to-t from-black/45 via-transparent to-transparent" />
          <div className="absolute bottom-4 right-6 flex items-end gap-3">
            <div className="text-right text-white">
              <div className="text-[14px] font-medium">{me.name}</div>
              <div className="mt-1 text-[9px] text-white/75">{me.note}</div>
            </div>
            <img src={me.avatar} alt={me.name} className="h-14 w-14 rounded-lg border-2 border-white/90 object-cover shadow-lg" />
          </div>
        </div>
        <div className="mx-auto max-w-[620px] space-y-5 px-6 py-6">
          {moments.map((m) => {
            const isLiked = liked.includes(m.id);
            return (
              <article key={m.id} className="rounded-xl border border-[#EBEBEB] bg-white p-5 shadow-[0_2px_12px_rgba(0,0,0,.025)]">
                <div className="flex gap-3">
                  <Avatar src={m.avatar} name={m.name} className="h-10 w-10 rounded-[5px]" />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-start justify-between">
                      <div>
                        <div className="text-[12px] font-medium text-[#2A2A2A]">{m.name}</div>
                        <p className="mt-1.5 text-[11.5px] leading-5 text-[#444]">{m.text}</p>
                      </div>
                      <button onClick={() => notify("朋友圈操作：收藏 / 举报 / 屏蔽")} className="text-[#C3C3C3] hover:text-[#666]"><MoreHorizontal size={16} /></button>
                    </div>
                    {m.images && <div className={cn("mt-2.5 gap-1", m.images.length === 1 ? "max-w-[300px]" : "grid max-w-[380px] grid-cols-3")}>
                      {m.images.map((img, i) => <img key={i} src={img} alt="朋友圈图片" className={cn("rounded-[3px] object-cover", m.images!.length === 1 ? "max-h-[240px] w-full" : "aspect-square w-full")} loading="lazy" />)}
                    </div>}
                    <div className="mt-3 flex items-center justify-between text-[9px] text-[#A6A6A6]">
                      <div className="flex items-center gap-2.5"><span>{m.time}</span>{m.location && <span className="text-[#7C9BC7]">{m.location}</span>}</div>
                      <div className="flex gap-2">
                        <button onClick={() => setLiked((arr) => isLiked ? arr.filter((id) => id !== m.id) : [...arr, m.id])} className={cn("flex items-center gap-1 rounded px-1.5 py-1 hover:bg-[#F5F5F5]", isLiked && "text-[#FA5151]")}><Heart size={12} fill={isLiked ? "currentColor" : "none"} /> {isLiked ? "已赞" : "赞"}</button>
                        <button onClick={() => notify("评论输入框已展开")} className="flex items-center gap-1 rounded px-1.5 py-1 hover:bg-[#F5F5F5]"><MessageCircle size={12} /> 评论</button>
                      </div>
                    </div>
                    <div className="mt-2 rounded-md bg-[#F7F7F7] px-3 py-2">
                      <div className="flex items-start gap-1 text-[9.5px] leading-4 text-[#777]"><Heart size={10} className="mt-0.5 shrink-0 text-[#7C9BC7]" /><span className="flex flex-wrap gap-1">{["苏禾", "Anna Chen", "陈知夏", ...(isLiked ? [] : ["林小满"])].join("、")}</span></div>
                      {m.comments.map((c, i) => <div key={i} className="mt-1.5 text-[9.5px] leading-4 text-[#666]"><span className="text-[#617E9E]">{c.name}：</span>{c.text}</div>)}
                    </div>
                  </div>
                </div>
              </article>
            );
          })}
          <div className="pb-6 pt-2 text-center text-[9.5px] text-[#B4B4B4]">没有更多了 · 朋友圈仅展示最近半年的内容</div>
        </div>
      </section>
    </div>
  );
}

const channelCards = [
  { id: 1, title: "把春天装进一间白房子", author: "林小满", avatar: "https://images.pexels.com/photos/14664508/pexels-photo-14664508.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=80&h=80", image: assets.gallery, likes: "2.4w", duration: "03:48" },
  { id: 2, title: "洱海边的第一百四十三天", author: "周予然", avatar: "https://images.pexels.com/photos/27688436/pexels-photo-27688436.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=80&h=80", image: assets.dawn, likes: "8,921", duration: "06:12" },
  { id: 3, title: "一杯咖啡的两种打开方式", author: "江屿的日常", avatar: "https://images.pexels.com/photos/9854302/pexels-photo-9854302.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=80&h=80", image: assets.coffee, likes: "1.2w", duration: "04:05" },
  { id: 4, title: "给小猫留一扇看得见风的窗", author: "有猫研究所", avatar: "https://images.pexels.com/photos/698532/pexels-photo-698532.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=80&h=80", image: assets.catBed, likes: "5.6w", duration: "02:36" },
  { id: 5, title: "摄影，是等待光的耐心", author: "陈知夏", avatar: "https://images.pexels.com/photos/20894157/pexels-photo-20894157.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=80&h=80", image: assets.cameras, likes: "9,330", duration: "08:21" },
  { id: 6, title: "周末，去山里走走吧", author: "葛一鸣", avatar: "https://images.pexels.com/photos/977418/pexels-photo-977418.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=80&h=80", image: assets.rocks, likes: "3,208", duration: "05:54" },
];

export function ChannelsPage({ notify }: { notify: (s: string) => void }) {
  const [tab, setTab] = useState("推荐");
  const [playing, setPlaying] = useState<(typeof channelCards)[number] | null>(null);
  return (
    <div className="relative flex min-h-0 flex-1">
      <aside className="flex w-[258px] shrink-0 flex-col border-r border-[#E8E8E8] bg-[#F7F7F7]">
        <SidebarHeader title="视频号" right={<><button onClick={() => notify("搜索视频号")}><Search size={15} /></button><button onClick={() => notify("发布视频")}><Plus size={16} /></button></>} />
        <div className="p-3"><MiniSearch placeholder="搜索视频号 / 内容" /></div>
        <div className="flex gap-2 px-3 pb-3">
          {["推荐", "关注", "朋友"].map((t) => <button key={t} onClick={() => setTab(t)} className={cn("rounded-full px-3 py-1.5 text-[10px]", tab === t ? "bg-[#1E1E1E] text-white" : "bg-[#EAEAEA] text-[#777]")}>{t}</button>)}
        </div>
        <div className="flex items-center justify-between px-4 py-1.5"><span className="text-[9px] font-medium tracking-wider text-[#A6A6A6]">正在直播</span><span className="text-[9px] text-[#FA5151]">2 场</span></div>
        <div className="mx-3 mb-2 overflow-hidden rounded-lg bg-white">
          <div className="relative h-[88px]">
            <img src={assets.gallery} alt="直播" className="h-full w-full object-cover" loading="lazy" />
            <span className="absolute left-2 top-2 flex items-center gap-1 rounded bg-[#FA5151] px-1.5 py-0.5 text-[8px] text-white"><span className="h-1 w-1 animate-pulse rounded-full bg-white" /> LIVE</span>
          </div>
          <div className="p-2.5"><div className="text-[10.5px] font-medium text-[#333]">春日回声 · 布展直播</div><div className="mt-1 flex items-center gap-1 text-[8.5px] text-[#A6A6A6]"><Eye size={9} /> 2,381 人正在看</div></div>
        </div>
        <div className="px-4 py-1.5 text-[9px] font-medium tracking-wider text-[#A6A6A6]">我关注的视频号</div>
        <div className="min-h-0 flex-1 space-y-0.5 overflow-y-auto">
          {["林小满的春天", "周予然在路上", "知夏摄影室", "有猫研究所", "江屿的日常"].map((name, i) => (
            <FeedAvatarRow key={name} name={name} avatar={channelCards[i].avatar} subtitle={i === 0 ? "刚刚更新 · 布展日记" : "朋友关注"} onClick={() => notify(`正在查看「${name}」`)} />
          ))}
        </div>
        <div className="mt-auto p-3 text-center text-[9px] text-[#B0B0B0]">视频号 · 记录真实生活</div>
      </aside>

      <section className="scrollbar-thin min-w-0 flex-1 overflow-y-auto bg-[#F7F7F7]">
        <div className="sticky top-0 z-10 flex h-[55px] items-center justify-between border-b border-[#E8E8E8] bg-[#F7F7F7]/95 px-6 backdrop-blur">
          <div className="flex items-center gap-5">
            {["推荐", "关注", "热门", "直播"].map((t) => <button key={t} onClick={() => setTab(t)} className={cn("relative py-1 text-[11px] transition", tab === t ? "font-medium text-[#1E1E1E]" : "text-[#999] hover:text-[#555]")}>{t}{tab === t && <span className="absolute -bottom-[18px] left-0 right-0 h-[2px] rounded-full bg-[#07C160]" />}</button>)}
          </div>
          <button onClick={() => notify("视频号内容已刷新")} className="grid h-7 w-7 place-items-center rounded border border-[#E5E5E5] bg-white text-[#777]"><Sparkles size={13} /></button>
        </div>
        <div className="p-6">
          <div className="relative overflow-hidden rounded-xl bg-[#1D2621] p-6 text-white shadow-lg">
            <img src={assets.dawn} alt="精选视频号" className="absolute inset-0 h-full w-full object-cover opacity-55" loading="lazy" />
            <div className="absolute inset-0 bg-gradient-to-r from-black/65 via-black/20 to-transparent" />
            <div className="relative z-10 max-w-[360px] py-3">
              <span className="rounded-full border border-white/25 px-2 py-1 text-[8.5px] tracking-wider text-white/80">本周精选 · FEATURED</span>
              <h2 className="mt-4 text-[22px] font-medium leading-8 tracking-wide">在风经过的地方，<br />记录生活的回响。</h2>
              <p className="mt-2 text-[10px] text-white/65">视频号创作者 · 周予然 <span className="mx-1.5">·</span> 第 143 天</p>
              <button onClick={() => setPlaying(channelCards[1])} className="mt-5 flex items-center gap-2 rounded-full bg-white px-4 py-2 text-[10px] font-medium text-[#222] transition hover:bg-white/90"><Play size={11} fill="currentColor" /> 立即观看</button>
            </div>
            <div className="absolute bottom-5 right-5 flex h-12 w-12 items-center justify-center rounded-full border border-white/30 bg-white/10 backdrop-blur"><Play size={16} fill="white" className="ml-0.5 text-white" /></div>
          </div>

          <div className="mt-5 flex items-center justify-between"><h3 className="text-[12px] font-medium text-[#333]">为你推荐</h3><span className="text-[9px] text-[#A6A6A6]">实时更新 · 基于你的兴趣</span></div>
          <div className="mt-3 grid grid-cols-2 gap-4 xl:grid-cols-3">
            {channelCards.map((card) => (
              <button key={card.id} onClick={() => setPlaying(card)} className="group text-left">
                <div className="relative overflow-hidden rounded-lg bg-[#DDD]">
                  <img src={card.image} alt={card.title} className="h-[120px] w-full object-cover transition duration-500 group-hover:scale-105" loading="lazy" />
                  <div className="absolute inset-0 bg-gradient-to-t from-black/50 via-transparent to-transparent" />
                  <span className="absolute bottom-2 right-2 rounded bg-black/45 px-1.5 py-0.5 text-[8.5px] text-white">{card.duration}</span>
                  <span className="absolute inset-0 grid place-items-center"><span className="grid h-8 w-8 place-items-center rounded-full bg-black/35 text-white opacity-0 backdrop-blur transition group-hover:opacity-100"><Play size={12} fill="white" className="ml-0.5" /></span></span>
                </div>
                <div className="mt-2 line-clamp-2 text-[11px] leading-4 text-[#333]">{card.title}</div>
                <div className="mt-1.5 flex items-center gap-2"><img src={card.avatar} alt="" className="h-4 w-4 rounded-full object-cover" /><span className="text-[8.5px] text-[#999]">{card.author}</span><span className="ml-auto flex items-center gap-0.5 text-[8.5px] text-[#AAA]"><Heart size={9} />{card.likes}</span></div>
              </button>
            ))}
          </div>
        </div>
      </section>

      {playing && (
        <div className="absolute inset-0 z-30 grid place-items-center bg-black/70 backdrop-blur-sm">
          <div className="relative w-[520px] overflow-hidden rounded-xl bg-[#111] shadow-2xl">
            <img src={playing.image} alt="" className="max-h-[300px] w-full object-cover" />
            <button onClick={() => setPlaying(null)} className="absolute right-3 top-3 grid h-7 w-7 place-items-center rounded-full bg-black/50 text-white"><X size={14} /></button>
            <div className="flex items-center gap-3 p-4 text-white">
              <img src={playing.avatar} alt="" className="h-8 w-8 rounded-full object-cover" />
              <div className="min-w-0 flex-1"><div className="text-[11px]">{playing.author}</div><div className="mt-0.5 truncate text-[9px] text-white/55">{playing.title}</div></div>
              <button onClick={() => notify("已关注该视频号")} className="rounded-full bg-[#07C160] px-3 py-1.5 text-[9px]">关注</button>
            </div>
            <div className="flex h-1 bg-white/10"><span className="w-1/3 bg-[#07C160]" /></div>
          </div>
        </div>
      )}
    </div>
  );
}

export function MiniProgramsPage({ notify }: { notify: (s: string) => void }) {
  return (
    <div className="flex min-h-0 flex-1">
      <aside className="flex w-[258px] shrink-0 flex-col border-r border-[#E8E8E8] bg-[#F7F7F7]">
        <SidebarHeader title="小程序" right={<><Search size={15} /><Plus size={16} /></>} />
        <div className="p-3"><MiniSearch placeholder="搜索小程序" /></div>
        <div className="px-4 py-1.5 text-[9px] font-medium tracking-wider text-[#A6A6A6]">最近使用</div>
        <div className="space-y-0.5">
          {miniApps.slice(0, 6).map((app) => (
            <button key={app.id} onClick={() => notify(`正在打开「${app.name}」`)} className="flex w-full items-center gap-2.5 px-4 py-2.5 text-left hover:bg-[#EDEDED]">
              <span className="grid h-8 w-8 place-items-center rounded-md text-base" style={{ background: app.bg }}>{app.emoji}</span>
              <span className="flex-1 text-[11px] text-[#333]">{app.name}</span>
              <ChevronRight size={12} className="text-[#C3C3C3]" />
            </button>
          ))}
        </div>
        <div className="mt-auto border-t border-[#E9E9E9] p-3"><button onClick={() => notify("已管理我的小程序")} className="flex w-full items-center justify-between text-[10px] text-[#666]">我的小程序 <ChevronRight size={12} /></button></div>
      </aside>
      <section className="scrollbar-thin min-w-0 flex-1 overflow-y-auto bg-[#F7F7F7]">
        <div className="sticky top-0 z-10 flex h-[55px] items-center justify-between border-b border-[#E8E8E8] bg-[#F7F7F7]/95 px-6 backdrop-blur">
          <div><h2 className="text-[14px] font-medium text-[#222]">发现小程序</h2><p className="text-[9px] text-[#A6A6A6]">无需安装，用完即走</p></div>
          <div className="flex h-7 w-[210px] items-center gap-1.5 rounded-md border border-[#E9E9E9] bg-white px-2.5 text-[#A6A6A6]"><Search size={12} /><span className="text-[9.5px]">搜索小程序名称或功能</span></div>
        </div>
        <div className="space-y-6 p-6">
          <div className="grid grid-cols-2 gap-3">
            <div className="relative overflow-hidden rounded-xl bg-[#1E2A24] p-5 text-white">
              <img src={assets.dawn} alt="" className="absolute inset-0 h-full w-full object-cover opacity-35" />
              <div className="relative z-10"><span className="text-[8px] tracking-wider text-white/60">TODAY'S PICK</span><h3 className="mt-2 text-[16px] font-medium">在文字里，去更远的地方</h3><p className="mt-1.5 text-[9px] text-white/60">微信读书 · 今日精选书单</p><button onClick={() => notify("已打开微信读书")} className="mt-4 rounded-full bg-white px-3 py-1.5 text-[9px] text-[#222]">去阅读</button></div>
            </div>
            <div className="relative overflow-hidden rounded-xl bg-[#F1E9DC] p-5">
              <div className="relative z-10"><span className="text-[8px] tracking-wider text-[#A48661]">WEEKEND PLAN</span><h3 className="mt-2 text-[16px] font-medium text-[#4A3B2A]">周末去哪儿？让城市给你答案。</h3><p className="mt-1.5 text-[9px] text-[#9C8467]">腾讯地图 · 城市漫游计划</p><button onClick={() => notify("已打开腾讯地图")} className="mt-4 rounded-full bg-[#4A3B2A] px-3 py-1.5 text-[9px] text-white">去探索</button></div>
              <div className="absolute -bottom-3 -right-3 grid h-20 w-20 place-items-center rounded-full bg-[#E3D4BF] text-4xl">🗺️</div>
            </div>
          </div>
          <div>
            <div className="mb-3 flex items-center justify-between"><h3 className="text-[12px] font-medium text-[#333]">常用小程序</h3><button onClick={() => notify("已查看全部小程序")} className="text-[9px] text-[#A6A6A6]">全部 12 个 <ChevronRight size={10} className="inline" /></button></div>
            <div className="grid grid-cols-4 gap-2.5">
              {miniApps.slice(0, 12).map((app) => (
                <button key={app.id} onClick={() => notify(`正在打开「${app.name}」`)} className="group rounded-lg border border-[#EEEEEE] bg-white px-3 py-4 text-center transition hover:-translate-y-0.5 hover:shadow-md">
                  <span className="mx-auto grid h-10 w-10 place-items-center rounded-lg text-xl transition group-hover:scale-105" style={{ background: app.bg }}>{app.emoji}</span>
                  <div className="mt-2 truncate text-[9.5px] text-[#555]">{app.name}</div>
                </button>
              ))}
            </div>
          </div>
          <div className="rounded-xl border border-[#EEEEEE] bg-white p-5">
            <div className="flex items-center justify-between"><div className="flex items-center gap-2 text-[11px] text-[#333]"><Sparkles size={14} className="text-[#07C160]" /> 最近使用记录</div><button onClick={() => notify("已清空小程序记录")} className="text-[9px] text-[#A6A6A6]">清空</button></div>
            <div className="mt-4 flex gap-5">
              {miniApps.slice(5, 10).map((app) => <div key={app.id} className="flex flex-col items-center gap-1.5"><span className="grid h-8 w-8 place-items-center rounded-lg text-base" style={{ background: app.bg }}>{app.emoji}</span><span className="text-[8.5px] text-[#888]">{app.name}</span></div>)}
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}

export function WalletPage({ notify }: { notify: (s: string) => void }) {
  const [hidden, setHidden] = useState(false);
  const [payOpen, setPayOpen] = useState(false);
  return (
    <div className="flex min-h-0 flex-1">
      <aside className="flex w-[258px] shrink-0 flex-col border-r border-[#E8E8E8] bg-[#F7F7F7]">
        <SidebarHeader title="支付" right={<MoreHorizontal size={16} />} />
        <div className="p-3"><MiniSearch placeholder="搜索支付记录" /></div>
        <div className="space-y-0.5 px-2">
          {[
            { icon: QrCode, label: "收付款", color: "#07C160" },
            { icon: Wallet, label: "钱包", color: "#F5B640" },
            { icon: CreditCard, label: "账单", color: "#5D9CE0" },
            { icon: Gift, label: "卡券", color: "#F07A7A" },
          ].map((item) => (
            <button key={item.label} onClick={() => item.label === "收付款" ? setPayOpen(true) : notify(`${item.label}页面已打开`)} className="flex w-full items-center gap-2.5 rounded-md px-2.5 py-2.5 text-left hover:bg-[#EDEDED]">
              <span className="grid h-7 w-7 place-items-center rounded-md text-white" style={{ background: item.color }}><item.icon size={14} /></span>
              <span className="text-[11px] text-[#333]">{item.label}</span>
              <ChevronRight size={12} className="ml-auto text-[#C3C3C3]" />
            </button>
          ))}
        </div>
        <div className="mx-4 my-2 border-t border-[#E9E9E9]" />
        <div className="px-4 py-1.5 text-[9px] font-medium tracking-wider text-[#A6A6A6]">支付服务</div>
        <div className="space-y-0.5 px-2">
          {["生活缴费", "手机充值", "出行服务", "医疗健康"].map((s) => <button key={s} onClick={() => notify(`${s}已打开`)} className="flex w-full items-center justify-between rounded-md px-2.5 py-2 text-[10.5px] text-[#666] hover:bg-[#EDEDED]">{s}<ChevronRight size={11} className="text-[#CCC]" /></button>)}
        </div>
        <div className="mt-auto px-4 py-3 text-[8.5px] leading-4 text-[#ABABAB]"><LockKeyhole size={10} className="mb-1 inline" /> 微信支付 · 安全、便捷</div>
      </aside>
      <section className="scrollbar-thin min-w-0 flex-1 overflow-y-auto bg-[#F7F7F7] p-6">
        <div className="mx-auto max-w-[670px]">
          <div className="mb-5 flex items-center justify-between"><div><h2 className="text-[16px] font-medium text-[#222]">微信支付</h2><p className="mt-1 text-[9.5px] text-[#A6A6A6]">便捷、安全，融入生活的每一次支付</p></div><button onClick={() => setPayOpen(true)} className="flex items-center gap-1.5 rounded-md border border-[#E5E5E5] bg-white px-3 py-1.5 text-[10px] text-[#666]"><QrCode size={12} /> 收付款</button></div>
          <div className="relative overflow-hidden rounded-2xl bg-[#07C160] p-6 text-white shadow-[0_14px_40px_rgba(7,193,96,.18)]">
            <div className="absolute -right-10 -top-16 h-44 w-44 rounded-full border-[24px] border-white/[0.07]" />
            <div className="absolute -bottom-16 right-16 h-36 w-36 rounded-full border-[20px] border-white/[0.06]" />
            <div className="relative z-10">
              <div className="flex items-center justify-between"><span className="flex items-center gap-1.5 text-[10px] text-white/75"><WeChatLogo className="h-4 w-4" /> 微信支付余额</span><button onClick={() => setHidden((v) => !v)} className="text-white/70 hover:text-white">{hidden ? <EyeOff size={14} /> : <Eye size={14} />}</button></div>
              <div className="mt-3 flex items-end gap-1"><span className="text-[15px] text-white/90">¥</span><span className="text-[34px] font-medium leading-none tracking-tight">{hidden ? "••••••" : "12,860.24"}</span></div>
              <div className="mt-7 flex gap-2.5">
                <button onClick={() => notify("充值页面已打开")} className="flex items-center gap-1.5 rounded-full bg-white px-3.5 py-1.5 text-[9.5px] font-medium text-[#07A858]"><Plus size={11} /> 充值</button>
                <button onClick={() => notify("提现已发起，预计 2 小时内到账")} className="flex items-center gap-1.5 rounded-full bg-white/15 px-3.5 py-1.5 text-[9.5px] text-white backdrop-blur"><ArrowUpRight size={11} /> 提现</button>
              </div>
            </div>
            <div className="relative z-10 mt-5 flex justify-between border-t border-white/15 pt-3 text-[8.5px] text-white/60"><span>本月支出 ¥ 1,024.80</span><span>零钱通收益 +¥ 16.32</span></div>
          </div>

          <div className="mt-4 grid grid-cols-4 gap-2.5">
            {[
              { icon: QrCode, label: "收付款", sub: "二维码" },
              { icon: Wallet, label: "钱包", sub: "零钱" },
              { icon: CreditCard, label: "银行卡", sub: "3 张" },
              { icon: Gift, label: "卡券", sub: "2 张可用" },
            ].map((item) => (
              <button key={item.label} onClick={() => setPayOpen(true)} className="rounded-xl border border-[#EDEDED] bg-white p-3.5 text-left transition hover:-translate-y-0.5 hover:shadow-md">
                <span className="grid h-7 w-7 place-items-center rounded-md bg-[#EDF9F1] text-[#07B25A]"><item.icon size={14} /></span>
                <div className="mt-2.5 text-[10px] font-medium text-[#333]">{item.label}</div>
                <div className="mt-0.5 text-[8.5px] text-[#A6A6A6]">{item.sub}</div>
              </button>
            ))}
          </div>

          <div className="mt-4 overflow-hidden rounded-xl border border-[#EEEEEE] bg-white">
            <div className="flex items-center justify-between border-b border-[#F2F2F2] px-5 py-3.5"><h3 className="text-[11.5px] font-medium text-[#333]">最近交易</h3><button onClick={() => notify("已打开完整账单")} className="flex items-center gap-0.5 text-[9px] text-[#999]">全部账单 <ChevronRight size={10} /></button></div>
            {transactions.map((t) => (
              <button key={t.id} onClick={() => notify(`查看交易详情：${t.name}`)} className="flex w-full items-center gap-3 border-b border-[#F6F6F6] px-5 py-3 text-left last:border-0 hover:bg-[#FAFAFA]">
                <span className="grid h-8 w-8 place-items-center rounded-full bg-[#F5F5F5] text-sm">{t.emoji}</span>
                <span className="min-w-0 flex-1"><span className="block text-[10.5px] text-[#333]">{t.name}</span><span className="mt-0.5 block text-[8.5px] text-[#A6A6A6]">{t.time}</span></span>
                <span className={cn("text-[11px] font-medium", t.amount.startsWith("+") ? "text-[#07A858]" : "text-[#333]")}>{t.amount}</span>
              </button>
            ))}
          </div>
          <div className="mt-4 flex items-center gap-2 rounded-lg border border-[#EDEDED] bg-white px-4 py-2.5 text-[9px] text-[#999]"><ShieldCheck size={13} className="text-[#07A858]" /> 微信支付受到安全技术保护，请放心使用。</div>
        </div>
      </section>

      {payOpen && (
        <div className="absolute inset-0 z-30 grid place-items-center bg-black/45 backdrop-blur-sm">
          <div className="relative w-[310px] overflow-hidden rounded-2xl bg-white shadow-2xl">
            <div className="flex items-center justify-between bg-[#07C160] px-5 py-4 text-white"><span className="text-[13px] font-medium">收付款</span><button onClick={() => setPayOpen(false)}><X size={16} /></button></div>
            <div className="p-6 text-center">
              <div className="text-[10px] text-[#999]">扫一扫，向我付款</div>
              <div className="mx-auto mt-4 grid h-[158px] w-[158px] place-items-center rounded-xl border border-[#EEE] bg-white">
                <div className="grid h-[130px] w-[130px] grid-cols-8 gap-[2px]">
                  {Array.from({ length: 64 }).map((_, i) => <span key={i} className={cn("rounded-[1px]", (i * 7 + Math.floor(i / 3)) % 3 === 0 ? "bg-[#1A1A1A]" : "bg-white")} />)}
                </div>
              </div>
              <div className="mt-3 text-[11px] text-[#333]">苏禾 <span className="ml-1 text-[9px] text-[#AAA]">微信支付</span></div>
              <div className="mt-4 flex items-center justify-center gap-6 border-t border-[#F2F2F2] pt-3 text-[9px] text-[#666]"><span className="flex items-center gap-1"><QrCode size={12} /> 二维码收款</span><span className="flex items-center gap-1"><ReceiptText size={12} /> 收款记录</span></div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export function SettingsPage({ notify }: { notify: (s: string) => void }) {
  const [autoStart, setAutoStart] = useState(true);
  const [sound, setSound] = useState(true);
  const [compact, setCompact] = useState(false);
  return (
    <div className="flex min-h-0 flex-1">
      <aside className="flex w-[258px] shrink-0 flex-col border-r border-[#E8E8E8] bg-[#F7F7F7]">
        <SidebarHeader title="设置" right={<Search size={15} />} />
        <div className="p-3">
          <div className="flex items-center gap-3 rounded-xl border border-[#EBEBEB] bg-white p-3">
            <Avatar src={me.avatar} name={me.name} className="h-10 w-10 rounded-md" />
            <div className="min-w-0 flex-1"><div className="text-[11.5px] font-medium text-[#333]">{me.name}</div><div className="mt-0.5 truncate text-[8.5px] text-[#A6A6A6]">微信号：suhe_2026</div></div>
            <QrCode size={15} className="text-[#B8B8B8]" />
          </div>
        </div>
        <div className="space-y-0.5 px-2">
          {[
            { icon: ShieldCheck, label: "账号与安全", color: "#07C160" },
            { icon: Settings2, label: "通用设置", color: "#7AA7E0" },
            { icon: Palette, label: "外观", color: "#B18BE0" },
            { icon: Bell, label: "消息通知", color: "#F5B640" },
            { icon: Keyboard, label: "快捷键", color: "#999" },
            { icon: CircleHelp, label: "帮助与反馈", color: "#8FA7B5" },
            { icon: FileText, label: "关于微信", color: "#A6A6A6" },
          ].map((item, i) => (
            <button key={item.label} onClick={() => notify(`${item.label}设置`)} className={cn("flex w-full items-center gap-2.5 rounded-md px-2.5 py-2.5 text-left hover:bg-[#EDEDED]", i === 0 && "bg-[#E8E8E8]")}>
              <span className="grid h-6 w-6 place-items-center rounded-md text-white" style={{ background: item.color }}><item.icon size={12} /></span>
              <span className="text-[10.5px] text-[#444]">{item.label}</span>
              <ChevronRight size={11} className="ml-auto text-[#C3C3C3]" />
            </button>
          ))}
        </div>
        <div className="mt-auto p-4 text-[8.5px] text-[#B3B3B3]">WeChat 4.0.6 · Design Study Edition</div>
      </aside>
      <section className="scrollbar-thin min-w-0 flex-1 overflow-y-auto bg-[#F7F7F7] p-7">
        <div className="mx-auto max-w-[620px]">
          <div className="flex items-center gap-3"><span className="grid h-9 w-9 place-items-center rounded-lg bg-[#EAF8EF] text-[#07B25A]"><Settings2 size={18} /></span><div><h2 className="text-[16px] font-medium text-[#222]">通用设置</h2><p className="mt-0.5 text-[9.5px] text-[#A6A6A6]">让微信更贴合你的使用习惯</p></div></div>

          <div className="mt-5 overflow-hidden rounded-xl border border-[#EBEBEB] bg-white">
            <div className="border-b border-[#F2F2F2] px-5 py-2.5 text-[9px] font-medium tracking-wider text-[#A6A6A6]">启动与运行</div>
            {[
              { label: "开机时自动启动微信", desc: "登录电脑后自动打开微信", on: autoStart, set: setAutoStart },
              { label: "消息提示音", desc: "收到新消息时播放提示音", on: sound, set: setSound },
              { label: "紧凑模式", desc: "在列表中显示更多聊天", on: compact, set: setCompact },
            ].map((row) => (
              <div key={row.label} className="flex items-center justify-between border-b border-[#F5F5F5] px-5 py-3.5 last:border-0">
                <div><div className="text-[11px] text-[#333]">{row.label}</div><div className="mt-1 text-[8.5px] text-[#A6A6A6]">{row.desc}</div></div>
                <Switch on={row.on} onChange={() => { row.set(!row.on); notify(`${row.label}已${row.on ? "关闭" : "开启"}`); }} />
              </div>
            ))}
          </div>

          <div className="mt-4 overflow-hidden rounded-xl border border-[#EBEBEB] bg-white">
            <div className="border-b border-[#F2F2F2] px-5 py-2.5 text-[9px] font-medium tracking-wider text-[#A6A6A6]">外观</div>
            <div className="flex items-center justify-between border-b border-[#F5F5F5] px-5 py-4">
              <div><div className="text-[11px] text-[#333]">主题模式</div><div className="mt-1 text-[8.5px] text-[#A6A6A6]">跟随系统，或手动选择</div></div>
              <div className="flex gap-1.5">
                {[{ icon: Monitor, label: "跟随" }, { icon: Moon, label: "深色" }].map((m, i) => <button key={m.label} onClick={() => notify(`外观已切换为${m.label}系统`)} className={cn("flex items-center gap-1 rounded-md border px-2.5 py-1.5 text-[9px]", i === 0 ? "border-[#07C160]/40 bg-[#F2FBF6] text-[#078A47]" : "border-[#E7E7E7] text-[#888]")}><m.icon size={11} />{m.label}</button>)}
              </div>
            </div>
            <button onClick={() => notify("切换聊天背景")} className="flex w-full items-center justify-between px-5 py-4 text-left hover:bg-[#FAFAFA]"><div><div className="text-[11px] text-[#333]">聊天背景</div><div className="mt-1 text-[8.5px] text-[#A6A6A6]">为所有聊天选择统一的背景</div></div><div className="flex items-center gap-2"><img src={assets.dawn} alt="" className="h-7 w-11 rounded object-cover" /><ChevronRight size={12} className="text-[#C3C3C3]" /></div></button>
          </div>

          <div className="mt-4 overflow-hidden rounded-xl border border-[#EBEBEB] bg-white">
            <div className="border-b border-[#F2F2F2] px-5 py-2.5 text-[9px] font-medium tracking-wider text-[#A6A6A6]">存储与数据</div>
            <div className="flex items-center justify-between px-5 py-4"><div><div className="text-[11px] text-[#333]">聊天记录迁移与备份</div><div className="mt-1 text-[8.5px] text-[#A6A6A6]">将记录迁移到另一台设备</div></div><button onClick={() => notify("已打开聊天记录迁移")} className="rounded-md border border-[#E6E6E6] px-3 py-1.5 text-[9px] text-[#666]">管理</button></div>
            <div className="flex items-center justify-between border-t border-[#F5F5F5] px-5 py-4"><div><div className="text-[11px] text-[#333]">清理存储空间</div><div className="mt-1 text-[8.5px] text-[#A6A6A6]">已使用 1.29 GB · 可用 8.71 GB</div></div><button onClick={() => notify("正在扫描可清理内容")} className="rounded-md border border-[#E6E6E6] px-3 py-1.5 text-[9px] text-[#666]">清理</button></div>
          </div>

          <button onClick={() => notify("已退出登录（演示）")} className="mt-4 flex w-full items-center justify-center gap-2 rounded-xl border border-[#F0DADA] bg-white py-3 text-[10px] text-[#D95353]"><LockKeyhole size={12} /> 退出登录</button>
          <div className="mt-5 text-center text-[8.5px] text-[#B8B8B8]">本页面为界面设计研究，不涉及真实账号与支付数据。</div>
        </div>
      </section>
    </div>
  );
}

export function DesktopLoginPage({ notify, onDone }: { notify: (s: string) => void; onDone: () => void }) {
  const [scanned, setScanned] = useState(false);
  const [autoLogin, setAutoLogin] = useState(true);
  return (
    <div className="screen-enter-soft flex min-h-0 flex-1 items-center justify-center bg-gradient-to-br from-[#F4F5F2] via-[#EEF2EB] to-[#E6EBE2] p-8">
      <div className="paper-grain relative flex w-full max-w-[660px] overflow-hidden rounded-2xl border border-black/[0.06] bg-white/90 shadow-[0_24px_70px_-20px_rgba(28,42,30,0.25)] backdrop-blur">
        <div className="flex flex-1 flex-col justify-center p-8">
          <div className="flex items-center gap-2.5">
            <WeChatLogo className="h-9 w-9" />
            <div>
              <div className="text-[17px] font-semibold tracking-tight text-[#1c1c1c]">微信 for Mac</div>
              <div className="mt-0.5 text-[9px] tracking-[0.14em] text-[#9aa39a]">VERSION 4.0.6 · DESIGN STUDY</div>
            </div>
          </div>
          <h2 className="mt-6 text-[22px] font-medium leading-snug tracking-[-0.02em] text-[#1e2a21]">
            在电脑上，
            <br />
            延续每一次连接。
          </h2>
          <div className="mt-5 space-y-2.5">
            {[
              "手机确认登录后，聊天记录实时同步",
              "大文件拖拽即传，最高支持 1GB",
              "支持深色模式与全局搜索 ⌘K",
            ].map((t) => (
              <div key={t} className="flex items-center gap-2 text-[10.5px] text-[#6b756c]">
                <span className="grid h-4 w-4 place-items-center rounded-full bg-[#E9F8EF] text-[9px] text-[#07A858]">✓</span>
                {t}
              </div>
            ))}
          </div>
          <div className="mt-6 flex items-center gap-2.5 rounded-xl bg-[#F5F6F3] px-3.5 py-3">
            <Avatar src={me.avatar} name={me.name} className="h-9 w-9 rounded-lg" />
            <div className="flex-1">
              <div className="text-[11px] font-medium text-[#2a2a2a]">{me.name}</div>
              <div className="mt-0.5 text-[8.5px] text-[#9a9a9a]">上次登录 · 今天 09:41 · 上海</div>
            </div>
            <button onClick={() => { notify("已切换账号（演示）"); }} className="text-[9px] text-[#576B95]">切换</button>
          </div>
          <label className="mt-4 flex cursor-pointer items-center gap-2" onClick={() => setAutoLogin(!autoLogin)}>
            <Switch small on={autoLogin} onChange={() => setAutoLogin(!autoLogin)} />
            <span className="text-[10px] text-[#777]">自动登录 · 进入后同步最近 7 天消息</span>
          </label>
        </div>
        <div className="flex w-[248px] shrink-0 flex-col items-center justify-center border-l border-black/[0.05] bg-[#FAFAF7] p-7">
          <div className="text-[11px] font-medium text-[#333]">手机扫码登录</div>
          <div className="mt-1 text-[8.5px] text-[#aaa]">打开微信 › 扫一扫</div>
          <button
            onClick={() => { setScanned(!scanned); if (!scanned) setTimeout(() => { notify("扫码成功，请在手机上确认"); }, 600); }}
            className="relative mt-4 grid h-[168px] w-[168px] place-items-center rounded-2xl border border-black/[0.06] bg-white p-3 shadow-[0_10px_30px_rgba(0,0,0,0.08)] transition hover:shadow-[0_14px_36px_rgba(7,193,96,0.15)]"
          >
            {!scanned ? (
              <div className="grid h-full w-full grid-cols-[repeat(21,minmax(0,1fr))]">
                {Array.from({ length: 441 }).map((_, i) => (
                  <span key={i} className={(i * 7 + Math.floor(i / 21) * 11) % 5 < 2 ? "aspect-square bg-[#1c1c1c]" : "aspect-square bg-white"} />
                ))}
              </div>
            ) : (
              <div className="flex h-full w-full flex-col items-center justify-center rounded-xl bg-[#F2FBF4]">
                <Avatar src={me.avatar} name={me.name} className="h-12 w-12 rounded-xl" />
                <div className="mt-2 text-[10px] font-medium text-[#333]">{me.name}</div>
                <div className="mt-1 flex items-center gap-1 text-[8.5px] text-[#07A858]"><span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[#07C160]" /> 扫码成功</div>
                <div className="mt-1 text-[8px] text-[#999]">请在手机上确认登录</div>
              </div>
            )}
            <span className="absolute -right-1.5 -top-1.5 grid h-6 w-6 place-items-center rounded-full bg-[#07C160] text-white shadow"><QrCode size={12} /></span>
          </button>
          <button onClick={() => { notify("登录成功，欢迎回来 ✦"); onDone(); }} className="pressable mt-4 w-full rounded-[10px] bg-[#07C160] py-2.5 text-[11px] font-medium text-white shadow-[0_8px_20px_rgba(7,193,96,0.3)]">
            {scanned ? "已确认 · 进入微信" : "模拟扫码确认"}
          </button>
          <button onClick={() => notify("更多登录选项：手机号 / 账号密码")} className="mt-2.5 text-[9px] text-[#999] hover:text-[#666]">更多登录方式</button>
        </div>
      </div>
    </div>
  );
}
