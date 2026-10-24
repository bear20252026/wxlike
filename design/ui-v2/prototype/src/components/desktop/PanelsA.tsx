import { useMemo, useState } from "react";
import {
  BadgeCheck,
  ChevronRight,
  Download,
  File,
  FileText,
  Folder,
  Image as ImageIcon,
  Link2,
  MessageCircle,
  Mic,
  MoreHorizontal,
  Plus,
  Search,
  Star,
  Users,
  Video,
  X,
} from "lucide-react";
import { assets, contacts, favorites, files, me, type Person } from "../../data";
import { Avatar, SectionLabel, WeChatLogo } from "../shared";
import { cn } from "../../utils/cn";

function PageHeader({
  title,
  count,
  right,
  search,
  onSearch,
  searchPlaceholder = "搜索",
}: {
  title: string;
  count?: string;
  right?: React.ReactNode;
  search?: string;
  onSearch?: (v: string) => void;
  searchPlaceholder?: string;
}) {
  return (
    <div className="shrink-0 border-b border-black/[0.05] bg-[#F7F7F7] px-4 pb-2.5 pt-3">
      <div className="flex h-7 items-center justify-between">
        <h2 className="flex items-baseline gap-2 text-[16px] font-semibold tracking-[0.02em] text-[#1f1f1f]">
          {title}
          {count && <span className="text-[9px] font-normal tabular-nums text-[#aaa]">{count}</span>}
        </h2>
        <div className="flex items-center gap-1 text-[#555]">{right}</div>
      </div>
      {search !== undefined && (
        <div className="mt-2 flex h-[30px] items-center gap-1.5 rounded-[8px] bg-[#E8E8E8] px-2.5 ring-1 ring-transparent transition focus-within:bg-white focus-within:ring-[#07C160]/40">
          <Search size={12} className="shrink-0 text-[#A2A2A2]" />
          <input
            value={search}
            onChange={(e) => onSearch?.(e.target.value)}
            placeholder={searchPlaceholder}
            className="w-full bg-transparent text-[11px] text-[#333] outline-none placeholder:text-[#A2A2A2]"
          />
          {search ? (
            <button onClick={() => onSearch?.("")} className="grid h-4 w-4 place-items-center rounded-full bg-[#C8C8C8] text-white">
              <X size={9} strokeWidth={3} />
            </button>
          ) : null}
        </div>
      )}
    </div>
  );
}

function IconTile({ bg, gradient, children }: { bg: string; gradient?: string; children: React.ReactNode }) {
  return (
    <span
      className="relative grid h-[34px] w-[34px] shrink-0 place-items-center overflow-hidden rounded-[8px] text-white shadow-[0_2px_6px_rgba(0,0,0,0.1)]"
      style={{ background: gradient ?? bg }}
    >
      <span className="absolute inset-x-0 top-0 h-1/2 bg-gradient-to-b from-white/25 to-transparent" />
      <span className="relative">{children}</span>
    </span>
  );
}

function HeroAction({ icon: Icon, label, primary, onClick }: { icon: typeof Users; label: string; primary?: boolean; onClick: () => void }) {
  return (
    <button onClick={onClick} className="pressable group flex flex-col items-center gap-1.5">
      <span
        className={cn(
          "grid h-10 w-10 place-items-center rounded-full transition",
          primary
            ? "bg-gradient-to-b from-[#0BD566] to-[#06B058] text-white shadow-[0_6px_16px_rgba(7,193,96,0.35)] group-hover:shadow-[0_8px_20px_rgba(7,193,96,0.45)]"
            : "border border-black/[0.06] bg-white text-[#555] shadow-sm group-hover:border-[#07C160]/40 group-hover:text-[#07A858]",
        )}
      >
        <Icon size={16} strokeWidth={1.8} />
      </span>
      <span className={cn("text-[10px]", primary ? "font-medium text-[#333]" : "text-[#666]")}>{label}</span>
    </button>
  );
}

/* ================= 通讯录 ================= */
export function ContactsPage({ notify, onOpenChat }: { notify: (s: string) => void; onOpenChat: (id: string) => void }) {
  const [selected, setSelected] = useState<Person>(contacts[1]);
  const [query, setQuery] = useState("");
  const [starred, setStarred] = useState<string[]>(["anna", "lin", "mama"]);

  const filtered = useMemo(() => {
    if (!query) return contacts;
    return contacts.filter((c) => c.name.toLowerCase().includes(query.toLowerCase()) || c.note?.includes(query));
  }, [query]);

  const sections = useMemo(() => {
    const groups: Record<string, Person[]> = {};
    filtered.forEach((c) => {
      const key = c.initial;
      groups[key] = [...(groups[key] || []), c];
    });
    return Object.entries(groups).sort(([a], [b]) => a.localeCompare(b));
  }, [filtered]);

  const toggleStar = () => {
    setStarred((s) => (s.includes(selected.id) ? s.filter((id) => id !== selected.id) : [...s, selected.id]));
    notify(starred.includes(selected.id) ? "已取消星标" : "已设为星标朋友");
  };

  return (
    <div className="screen-enter-soft flex min-h-0 flex-1">
      <aside className="flex w-[264px] shrink-0 flex-col border-r border-black/[0.06] bg-[#F7F7F7]">
        <PageHeader
          title="通讯录"
          count={`${contacts.length} 位联系人`}
          search={query}
          onSearch={setQuery}
          searchPlaceholder="搜索联系人"
          right={
            <>
              <button onClick={() => notify("添加朋友：微信号、手机号或扫码")} title="添加朋友" className="grid h-7 w-7 place-items-center rounded-md transition hover:bg-[#E5E5E5]"><Plus size={16} /></button>
              <button onClick={() => notify("通讯录管理")} title="管理" className="grid h-7 w-7 place-items-center rounded-md transition hover:bg-[#E5E5E5]"><MoreHorizontal size={16} /></button>
            </>
          }
        />
        <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto pb-4 pt-1">
          {[
            { label: "新的朋友", bg: "#F5B640", gradient: "linear-gradient(135deg,#F8C85E,#EE9E2E)", icon: <Users size={15} />, badge: 3 },
            { label: "仅聊天的朋友", bg: "#A7D36A", gradient: "linear-gradient(135deg,#B5DE7C,#8CBC4E)", icon: <Users size={15} /> },
            { label: "群聊", bg: "#6DB7FF", gradient: "linear-gradient(135deg,#7FC3FF,#4E9BE8)", icon: <Users size={15} /> },
            { label: "标签", bg: "#6DB7FF", gradient: "linear-gradient(135deg,#7FC3FF,#4E9BE8)", icon: <Star size={14} /> },
            { label: "公众号", bg: "#7B8CFF", gradient: "linear-gradient(135deg,#8F9EFF,#6878EE)", icon: <FileText size={14} /> },
            { label: "企业微信联系人", bg: "#5CA8FF", gradient: "linear-gradient(135deg,#6FB6FF,#4A90EE)", icon: <WeChatLogo mono className="h-4 w-4 text-white" /> },
          ].map((row: any) => (
            <button
              key={row.label}
              onClick={() => notify(`${row.label} · 页面已加载`)}
              className="group flex w-full items-center gap-2.5 px-4 py-[7px] text-left transition hover:bg-[#EBEBEB]"
            >
              <IconTile bg={row.bg} gradient={row.gradient}>{row.icon}</IconTile>
              <span className="flex-1 text-[12px] text-[#303030]">{row.label}</span>
              {row.badge && <span className="grid h-[16px] min-w-[16px] place-items-center rounded-full bg-[#FA5151] px-1 text-[9px] font-bold text-white">{row.badge}</span>}
              <ChevronRight size={12} className="text-[#C0C0C0] transition-transform group-hover:translate-x-0.5" />
            </button>
          ))}
          <div className="mx-4 my-2 border-t border-black/[0.05]" />
          {sections.length === 0 && (
            <div className="flex flex-col items-center px-8 py-10">
              <span className="grid h-10 w-10 place-items-center rounded-xl bg-[#EFEFEF] text-[#aaa]"><Search size={16} /></span>
              <div className="mt-2 text-[11px] text-[#888]">没有找到联系人</div>
            </div>
          )}
          {sections.map(([letter, people]) => (
            <div key={letter}>
              <div className="sticky top-0 z-10 bg-[#F7F7F7]/95 px-4 py-1 text-[9px] font-semibold tracking-[0.12em] text-[#9a9a9a] backdrop-blur">{letter}</div>
              {people.map((person) => (
                <button
                  key={person.id}
                  onClick={() => setSelected(person)}
                  className={cn(
                    "relative flex w-full items-center gap-2.5 px-4 py-[7px] text-left transition",
                    person.id === selected.id ? "bg-[#E0E0E0]" : "hover:bg-[#EBEBEB]",
                  )}
                >
                  {person.id === selected.id && <span className="absolute left-0 top-1/2 h-6 w-[3px] -translate-y-1/2 rounded-r-full bg-[#07C160]" />}
                  <Avatar src={person.avatar} name={person.name} className="h-8 w-8 rounded-[6px]" online={person.online} />
                  <span className="min-w-0 flex-1">
                    <span className="flex items-center gap-1.5 truncate text-[12px] text-[#2a2a2a]">
                      {person.name}
                      {starred.includes(person.id) && <Star size={9} className="shrink-0 fill-[#F5B640] text-[#F5B640]" />}
                    </span>
                    {person.note && <span className="mt-0.5 block truncate text-[9px] text-[#aaa]">{person.note}</span>}
                  </span>
                </button>
              ))}
            </div>
          ))}
        </div>
      </aside>

      <section className="scrollbar-thin min-w-0 flex-1 overflow-y-auto bg-gradient-to-b from-[#F7F7F7] to-[#F1F1EC] px-8 py-7">
        <div className="mx-auto max-w-[580px]">
          <div className="relative overflow-hidden rounded-2xl border border-black/[0.05] bg-white p-7 shadow-[0_8px_32px_rgba(0,0,0,0.05)]">
            <div className="pointer-events-none absolute inset-x-0 top-0 h-[86px] bg-gradient-to-br from-[#E9F3EA] via-[#EFF4ED] to-transparent" />
            <div className="relative flex items-start gap-6">
              <div className="relative">
                <Avatar src={selected.avatar} name={selected.name} className="h-[76px] w-[76px] rounded-2xl shadow-md" online={selected.online} />
                {starred.includes(selected.id) && (
                  <span className="absolute -right-1.5 -top-1.5 grid h-6 w-6 place-items-center rounded-full bg-[#F5B640] text-white shadow ring-2 ring-white">
                    <Star size={11} fill="currentColor" />
                  </span>
                )}
              </div>
              <div className="min-w-0 flex-1 pt-0.5">
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0">
                    <h1 className="flex items-center gap-1.5 text-[20px] font-semibold tracking-[0.01em] text-[#1a1a1a]">
                      <span className="truncate">{selected.name}</span>
                      {selected.id === "mama" && <BadgeCheck size={15} className="shrink-0 text-[#07C160]" />}
                    </h1>
                    <p className="mt-1 text-[10px] tabular-nums text-[#9a9a9a]">微信号：wxid_{selected.id}26 · {selected.region}</p>
                  </div>
                  <button onClick={toggleStar} title={starred.includes(selected.id) ? "取消星标" : "设为星标"} className={cn("grid h-8 w-8 shrink-0 place-items-center rounded-full transition", starred.includes(selected.id) ? "bg-[#FFF6E3] text-[#F5B640]" : "bg-[#F3F3F3] text-[#B8B8B8] hover:text-[#F5B640]")}>
                    <Star size={15} fill={starred.includes(selected.id) ? "currentColor" : "none"} />
                  </button>
                </div>
                <p className="mt-2.5 line-clamp-2 text-[11.5px] leading-5 text-[#666]">{selected.note}</p>
                <div className="mt-5 flex items-center gap-8">
                  <HeroAction icon={MessageCircle} label="发消息" primary onClick={() => onOpenChat(selected.id)} />
                  <HeroAction icon={Video} label="音视频通话" onClick={() => notify(`正在呼叫 ${selected.name}`)} />
                  <HeroAction icon={MoreHorizontal} label="更多" onClick={() => notify("更多资料：备注、权限、投诉")} />
                </div>
              </div>
            </div>
          </div>

          <div className="mt-4 rounded-2xl border border-black/[0.05] bg-white p-6 shadow-[0_8px_32px_rgba(0,0,0,0.04)]">
            <div className="flex items-center justify-between">
              <SectionLabel>朋友圈</SectionLabel>
              <button onClick={() => notify("已展开全部朋友圈")} className="text-[10px] text-[#9a9a9a] hover:text-[#576B95]">全部 24 张 ›</button>
            </div>
            <div className="mt-3 grid grid-cols-4 gap-2">
              {[assets.gallery, assets.dawn, assets.coffee, assets.tulips].map((image, i) => (
                <button key={i} onClick={() => notify("正在查看该联系人的朋友圈")} className="group relative overflow-hidden rounded-[8px]">
                  <img src={image} alt="朋友圈照片" className="aspect-square w-full object-cover transition duration-500 group-hover:scale-110" loading="lazy" />
                  <span className="absolute inset-0 bg-gradient-to-t from-black/25 to-transparent opacity-0 transition group-hover:opacity-100" />
                </button>
              ))}
            </div>
            <div className="mt-5 grid grid-cols-2 gap-x-8 gap-y-4 border-t border-[#F0F0F0] pt-5">
              {[
                ["个性签名", selected.note || "这个人很懒，什么都没留下"],
                ["标签", "朋友 · 设计"],
                ["来源", "通过群聊「周末去哪儿」添加"],
                ["朋友权限", "聊天、朋友圈、视频号"],
              ].map(([k, v]) => (
                <div key={k}>
                  <div className="text-[9px] font-medium tracking-[0.08em] text-[#a6a6a6]">{k}</div>
                  <div className="mt-1 line-clamp-1 text-[11px] text-[#444]">{v}</div>
                </div>
              ))}
            </div>
          </div>

          <button onClick={() => notify("共同群聊：春日设计小组、周末去哪儿...")} className="mt-4 flex w-full items-center justify-between rounded-2xl border border-black/[0.05] bg-white px-5 py-3.5 text-[11px] text-[#555] shadow-[0_4px_16px_rgba(0,0,0,0.03)] transition hover:shadow-md">
            <div className="flex items-center gap-2.5">
              <span className="grid h-7 w-7 place-items-center rounded-lg bg-[#E9F8EF] text-[#07A858]"><Users size={14} /></span>
              共同群聊 · 4 个
            </div>
            <span className="flex items-center gap-0.5 text-[#9a9a9a]">查看 <ChevronRight size={13} /></span>
          </button>
        </div>
      </section>
    </div>
  );
}

/* ================= 收藏 ================= */
const favTypeStyles: Record<string, { bg: string; gradient: string; text: string; icon: React.ReactNode }> = {
  链接: { bg: "#EDF7F0", gradient: "linear-gradient(135deg,#DFF2E5,#C4E7CF)", text: "#07A55B", icon: <Link2 size={14} /> },
  图片: { bg: "#EEF5FF", gradient: "linear-gradient(135deg,#DCE9FB,#C2D6F5)", text: "#4C7CCB", icon: <ImageIcon size={14} /> },
  笔记: { bg: "#FFF6E6", gradient: "linear-gradient(135deg,#FFEDCB,#F7DCA8)", text: "#B5842A", icon: <FileText size={14} /> },
  语音: { bg: "#F1F0FF", gradient: "linear-gradient(135deg,#DDD9FB,#C6C0F2)", text: "#6F68C8", icon: <Mic size={14} /> },
  文件: { bg: "#EDF7F0", gradient: "linear-gradient(135deg,#DFF2E5,#C4E7CF)", text: "#07A55B", icon: <Folder size={14} /> },
  视频: { bg: "#FFF1EE", gradient: "linear-gradient(135deg,#FBD9D1,#F5BBAE)", text: "#DE5F46", icon: <Video size={14} /> },
};

export function FavoritesPage({ notify }: { notify: (s: string) => void }) {
  const [active, setActive] = useState(favorites[0]);
  const [tab, setTab] = useState("全部");
  const list = tab === "全部" ? favorites : favorites.filter((f) => f.type === tab);
  return (
    <div className="screen-enter-soft flex min-h-0 flex-1">
      <aside className="flex w-[264px] shrink-0 flex-col border-r border-black/[0.06] bg-[#F7F7F7]">
        <PageHeader title="收藏" count={`${favorites.length} 条`} search="" onSearch={() => notify("搜索收藏")} searchPlaceholder="搜索收藏" right={<Plus size={16} />} />
        <div className="flex gap-1.5 overflow-x-auto px-4 py-2.5 no-scrollbar">
          {["全部", "图片", "笔记", "链接", "文件"].map((t) => (
            <button key={t} onClick={() => setTab(t)} className={cn("shrink-0 rounded-full px-3 py-1.5 text-[10px] transition", tab === t ? "bg-[#1f1f1f] font-medium text-white" : "bg-[#EBEBEB] text-[#777] hover:bg-[#E2E2E2]")}>{t}</button>
          ))}
        </div>
        <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto pb-2">
          {list.map((item) => {
            const type = favTypeStyles[item.type] || favTypeStyles["链接"];
            const isActive = active.id === item.id;
            return (
              <button
                key={item.id}
                onClick={() => setActive(item)}
                className={cn("relative flex w-full gap-3 px-4 py-3 text-left transition", isActive ? "bg-[#E2E2E2]" : "hover:bg-[#ECECEC]")}
              >
                {isActive && <span className="absolute left-0 top-1/2 h-8 w-[3px] -translate-y-1/2 rounded-r-full bg-[#07C160]" />}
                <span className="relative grid h-10 w-10 shrink-0 place-items-center overflow-hidden rounded-[9px] shadow-sm" style={{ background: type.gradient, color: type.text }}>
                  <span className="absolute inset-x-0 top-0 h-1/2 bg-gradient-to-b from-white/30 to-transparent" />
                  <span className="relative">{type.icon}</span>
                </span>
                <span className="min-w-0 flex-1">
                  <span className="line-clamp-2 block text-[11.5px] leading-[1.5] text-[#2f2f2f]">{item.title}</span>
                  <span className="mt-1.5 flex items-center justify-between text-[9px] tabular-nums text-[#a6a6a6]"><span className="truncate">{item.source}</span><span className="ml-2 shrink-0">{item.date}</span></span>
                </span>
                {item.image && <img src={item.image} alt="" className="h-10 w-10 shrink-0 rounded-[7px] object-cover shadow-sm" />}
              </button>
            );
          })}
          {list.length === 0 && <div className="py-12 text-center text-[10.5px] text-[#aaa]">暂无该类型的收藏</div>}
        </div>
        <div className="border-t border-black/[0.05] bg-[#F2F2F2] px-4 py-2 text-center text-[9px] tabular-nums text-[#a6a6a6]">共 {favorites.length} 条 · 已全部同步到云端</div>
      </aside>
      <section className="scrollbar-thin min-w-0 flex-1 overflow-y-auto bg-gradient-to-b from-[#F7F7F7] to-[#F1F1EC] px-8 py-6">
        <div className="mx-auto max-w-[620px]">
          <div className="mb-4 flex items-center justify-between">
            <div>
              <div className="flex items-center gap-2 text-[9px] font-semibold tracking-[0.16em] text-[#aaa]">
                <span className="rounded bg-[#E9EDE6] px-1.5 py-0.5 text-[#6b7a6d]">{active.type}</span>
                {active.date}
              </div>
              <h2 className="mt-1.5 text-[15px] font-semibold text-[#222]">{active.source}</h2>
            </div>
            <div className="flex gap-2">
              <button onClick={() => notify("已复制收藏内容")} className="rounded-lg border border-black/[0.07] bg-white px-3 py-1.5 text-[10.5px] text-[#666] shadow-sm transition hover:bg-[#FAFAFA]">复制</button>
              <button onClick={() => notify("已分享至聊天")} className="rounded-lg bg-gradient-to-b from-[#0BD566] to-[#06B058] px-3 py-1.5 text-[10.5px] font-medium text-white shadow-[0_4px_12px_rgba(7,193,96,0.3)]">分享</button>
            </div>
          </div>
          <article className="overflow-hidden rounded-2xl border border-black/[0.05] bg-white shadow-[0_12px_44px_rgba(0,0,0,0.06)]">
            {active.image && <img src={active.image} alt={active.title} className="max-h-[250px] w-full object-cover" loading="lazy" />}
            <div className="p-7">
              <h1 className="text-[21px] font-semibold leading-8 tracking-[-0.01em] text-[#1e1e1e]">{active.title}</h1>
              {active.type === "笔记" ? (
                <div className="mt-5 space-y-3.5 text-[12.5px] leading-[1.9] text-[#555]">
                  <p>微信的界面并不需要被装饰。它更像一张干净的纸，让内容、关系与情绪自然地落在上面。</p>
                  <p>克制的灰阶、单一的绿色强调、恰到好处的圆角，构成了它在视觉上始终保持的温度。</p>
                  <p className="rounded-r-lg border-l-[3px] border-[#07C160] bg-[#F6FBF7] py-2 pl-3 pr-3 text-[#666]">好的设计，是让人感觉不到设计。</p>
                </div>
              ) : (
                <p className="mt-4 text-[12.5px] leading-[1.9] text-[#777]">在内容与留白之间，找到让人愿意停留的节奏。这里有我们在春日设计小组里反复讨论的细节：层级、触感，以及一次恰到好处的出现。</p>
              )}
              <div className="mt-6 flex items-center gap-3 border-t border-[#F1F1F1] pt-4">
                <Avatar src={me.avatar} name={me.name} className="h-7 w-7 rounded-[6px]" />
                <div className="text-[10px] tabular-nums text-[#888]">收藏于我的微信 <span className="mx-1 text-[#D0D0D0]">·</span> {active.date}</div>
                <button onClick={() => notify("收藏已标记星标")} className="ml-auto grid h-7 w-7 place-items-center rounded-full bg-[#F5F5F5] text-[#B9B9B9] transition hover:text-[#F5B640]"><Star size={14} /></button>
              </div>
            </div>
          </article>
        </div>
      </section>
    </div>
  );
}

/* ================= 文件 ================= */
const fileAccent: Record<string, string> = {
  pdf: "linear-gradient(135deg,#FF8A8A,#F06060)",
  fig: "linear-gradient(135deg,#B79BF0,#8B64DC)",
  ppt: "linear-gradient(135deg,#F5AE68,#E88A3C)",
  img: "linear-gradient(135deg,#6FB3EE,#3F8BD8)",
  zip: "linear-gradient(135deg,#CDBF9E,#A8946E)",
  txt: "linear-gradient(135deg,#9AA8B5,#6E7F8F)",
};

export function FilesPage({ notify }: { notify: (s: string) => void }) {
  const [selected, setSelected] = useState(files[0]);
  const [tab, setTab] = useState("全部文件");
  return (
    <div className="screen-enter-soft flex min-h-0 flex-1">
      <aside className="flex w-[264px] shrink-0 flex-col border-r border-black/[0.06] bg-[#F7F7F7]">
        <PageHeader title="文件" count="1.29 GB" search="" onSearch={() => notify("搜索文件")} searchPlaceholder="搜索文件" right={<MoreHorizontal size={16} />} />
        <div className="space-y-0.5 px-2.5 py-2.5">
          {["全部文件", "最近使用", "图片", "文档"].map((t) => (
            <button key={t} onClick={() => setTab(t)} className={cn("flex w-full items-center gap-2.5 rounded-[9px] px-2.5 py-2 text-[11.5px] transition", tab === t ? "bg-[#E2E2E2] font-medium text-[#1f1f1f]" : "text-[#666] hover:bg-[#EBEBEB]")}>
              {t === "全部文件" ? <Folder size={14} className="text-[#07A858]" /> : <File size={14} className="text-[#A0A0A0]" />}
              {t}
              {t === "全部文件" && <span className="ml-auto rounded-full bg-[#E8E8E8] px-1.5 py-0.5 text-[8.5px] tabular-nums text-[#888]">{files.length}</span>}
            </button>
          ))}
        </div>
        <div className="mx-4 border-t border-black/[0.05]" />
        <button onClick={() => notify("文件传输助手 · 已连接")} className="flex items-center gap-2.5 px-4 py-3 text-left transition hover:bg-[#EBEBEB]">
          <span className="relative grid h-9 w-9 place-items-center overflow-hidden rounded-[9px] bg-gradient-to-br from-[#0BD566] to-[#06A854] shadow-sm">
            <span className="absolute inset-x-0 top-0 h-1/2 bg-gradient-to-b from-white/25 to-transparent" />
            <Folder size={15} className="relative text-white" />
          </span>
          <div><div className="text-[11.5px] font-medium text-[#333]">文件传输助手</div><div className="mt-0.5 flex items-center gap-1 text-[9px] text-[#07A858]"><span className="h-1 w-1 rounded-full bg-[#07C160]" /> 手机与电脑互传</div></div>
        </button>
        <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto border-t border-black/[0.05]">
          <div className="px-4 py-2 text-[9px] font-semibold tracking-[0.1em] text-[#9a9a9a]">最近文件</div>
          {files.map((f) => (
            <button key={f.id} onClick={() => setSelected(f)} className={cn("flex w-full items-center gap-2.5 px-4 py-2.5 text-left transition", selected.id === f.id ? "bg-[#E2E2E2]" : "hover:bg-[#EBEBEB]")}>
              <span className="grid h-8 w-8 shrink-0 place-items-center rounded-[7px] text-[8px] font-bold uppercase text-white shadow-sm" style={{ background: fileAccent[f.icon] }}>{f.icon}</span>
              <span className="min-w-0"><span className="block truncate text-[10.5px] text-[#333]">{f.name}</span><span className="mt-0.5 block text-[8.5px] tabular-nums text-[#a6a6a6]">{f.meta}</span></span>
            </button>
          ))}
        </div>
      </aside>
      <section className="scrollbar-thin min-w-0 flex-1 overflow-y-auto bg-gradient-to-b from-[#F7F7F7] to-[#F1F1EC]">
        <div className="sticky top-0 z-10 flex h-[58px] items-center justify-between border-b border-black/[0.05] bg-[#F7F7F7]/92 px-7 backdrop-blur">
          <div><h2 className="text-[14px] font-semibold text-[#222]">{tab}</h2><p className="mt-0.5 text-[9.5px] tabular-nums text-[#a6a6a6]">共 {files.length} 个文件 · 1.29 GB</p></div>
          <button onClick={() => notify("已打开文件选择器")} className="pressable flex items-center gap-1.5 rounded-[9px] bg-gradient-to-b from-[#0BD566] to-[#06B058] px-3.5 py-2 text-[10.5px] font-medium text-white shadow-[0_4px_12px_rgba(7,193,96,0.3)]"><Plus size={13} /> 上传文件</button>
        </div>
        <div className="p-7">
          <div className="rounded-2xl border border-black/[0.05] bg-white p-5 shadow-[0_8px_28px_rgba(0,0,0,0.04)]">
            <div className="flex items-center gap-5">
              <div className="relative h-[72px] w-[72px] shrink-0">
                <svg viewBox="0 0 72 72" className="h-full w-full -rotate-90">
                  <circle cx="36" cy="36" r="30" fill="none" stroke="#F0F0F0" strokeWidth="8" />
                  <circle cx="36" cy="36" r="30" fill="none" stroke="#07C160" strokeWidth="8" strokeLinecap="round" strokeDasharray="188" strokeDashoffset="164" />
                </svg>
                <div className="absolute inset-0 flex flex-col items-center justify-center">
                  <span className="text-[13px] font-bold tabular-nums text-[#222]">13%</span>
                </div>
              </div>
              <div className="min-w-0 flex-1">
                <div className="text-[10px] text-[#999]">存储空间</div>
                <div className="mt-1 text-[20px] font-semibold tracking-tight tabular-nums text-[#222]">1.29 <span className="text-[11px] font-normal text-[#999]">GB / 10 GB</span></div>
                <div className="mt-2.5 flex h-1.5 overflow-hidden rounded-full bg-[#F0F0F0]">
                  <span className="w-[38%] bg-gradient-to-r from-[#07C160] to-[#0BD566]" /><span className="w-[24%] bg-[#7ECB9D]" /><span className="w-[14%] bg-[#F5C26B]" />
                </div>
                <div className="mt-2 flex gap-3 text-[8.5px] tabular-nums text-[#999]"><span><i className="mr-1 inline-block h-1.5 w-1.5 rounded-full bg-[#07C160]" />聊天文件 490M</span><span><i className="mr-1 inline-block h-1.5 w-1.5 rounded-full bg-[#7ECB9D]" />图片 310M</span><span><i className="mr-1 inline-block h-1.5 w-1.5 rounded-full bg-[#F5C26B]" />其他 180M</span></div>
              </div>
              <button onClick={() => notify("正在扫描可清理的文件")} className="shrink-0 rounded-lg border border-black/[0.07] px-3 py-1.5 text-[10px] text-[#666] transition hover:bg-[#F7F7F7]">管理存储</button>
            </div>
          </div>

          <div className="mt-6 flex items-center justify-between">
            <h3 className="text-[12.5px] font-semibold text-[#333]">最近文件</h3>
            <div className="flex gap-1 rounded-full bg-[#ECECEC] p-0.5 text-[9.5px] text-[#888]">
              {["时间", "大小"].map((t, i) => <button key={t} onClick={() => notify(`按${t}排序`)} className={cn("rounded-full px-2.5 py-1", i === 0 && "bg-white font-medium text-[#333] shadow-sm")}>{t}</button>)}
            </div>
          </div>
          <div className="mt-3 grid grid-cols-2 gap-3">
            {files.map((file) => (
              <button
                key={file.id}
                onClick={() => setSelected(file)}
                className={cn(
                  "group flex items-center gap-3 rounded-xl border bg-white p-3.5 text-left transition duration-200 hover:-translate-y-0.5 hover:shadow-[0_10px_26px_rgba(0,0,0,0.08)]",
                  selected.id === file.id ? "border-[#07C160]/50 shadow-[0_0_0_3px_rgba(7,193,96,0.08)]" : "border-black/[0.05]",
                )}
              >
                <span className="relative grid h-12 w-10 shrink-0 place-items-center overflow-hidden rounded-[6px] text-[9px] font-bold uppercase text-white shadow-sm" style={{ background: fileAccent[file.icon] }}>
                  <span className="absolute inset-x-0 top-0 h-1/2 bg-gradient-to-b from-white/25 to-transparent" />
                  <span className="relative">{file.icon}</span>
                </span>
                <span className="min-w-0 flex-1"><span className="block truncate text-[11.5px] font-medium text-[#333]">{file.name}</span><span className="mt-1 block text-[9px] tabular-nums text-[#a6a6a6]">{file.meta}</span></span>
                <span onClick={(e) => { e.stopPropagation(); notify(`正在下载 ${file.name}`); }} className="grid h-7 w-7 shrink-0 place-items-center rounded-lg text-[#a6a6a6] transition hover:bg-[#EFFBF3] hover:text-[#07A858]"><Download size={14} /></span>
              </button>
            ))}
          </div>

          <div className="group mt-6 cursor-pointer rounded-2xl border-[1.5px] border-dashed border-[#D5D5D5] bg-[#FBFBF8] p-7 text-center transition hover:border-[#07C160]/50 hover:bg-[#F6FBF7]" onClick={() => notify("已打开文件选择器")}>
            <div className="mx-auto grid h-10 w-10 place-items-center rounded-full bg-white text-[#999] shadow-sm ring-1 ring-black/[0.04] transition group-hover:scale-110 group-hover:text-[#07A858]"><Plus size={18} /></div>
            <div className="mt-2.5 text-[11.5px] font-medium text-[#666]">拖拽文件到这里，快速发送到「文件传输助手」</div>
            <div className="mt-1 text-[9px] tabular-nums text-[#b2b2b2]">支持图片、视频、文档等格式 · 单文件最大 1 GB</div>
          </div>
        </div>
      </section>
    </div>
  );
}
