export const px = (id: number, w = 240, h = 240) =>
  `https://images.pexels.com/photos/${id}/pexels-photo-${id}.jpeg?auto=compress&cs=tinysrgb&fit=crop&w=${w}&h=${h}`;

export const assets = {
  cover: px(36328262, 1200, 700),
  lake: px(4084851, 1000, 700),
  dawn: px(28593673, 1200, 800),
  rocks: px(27413508, 1000, 700),
  coffee: px(1403396, 1200, 900),
  brunch: px(9000104, 900, 900),
  croissant: px(29324131, 1000, 750),
  dinner: px(37653794, 1000, 800),
  cat: px(29748152, 800, 1100),
  catBed: px(220876, 1000, 700),
  gallery: px(35436850, 1200, 800),
  whiteWall: px(2090082, 1000, 700),
  posters: px(6373781, 1000, 700),
  tulips: px(35983785, 800, 1000),
  pinkTulips: px(29558368, 800, 1000),
  tulipGlass: px(7235326, 800, 1000),
  cameras: px(36933577, 1000, 700),
  desk: px(2842457, 1000, 700),
  night: px(28593673, 800, 1100),
  street: px(977418, 1000, 700),
};

export type Person = {
  id: string;
  name: string;
  avatar: string;
  initial: string;
  note?: string;
  region?: string;
  online?: boolean;
  star?: boolean;
};

export const me: Person = {
  id: "me",
  name: "苏 禾",
  avatar: px(38785088, 240, 240),
  initial: "S",
  note: "把日常过成喜欢的样子。",
  region: "中国 · 杭州",
  online: true,
};

export const contacts: Person[] = [
  { id: "anna", name: "Anna Chen", avatar: px(38768990, 200, 200), initial: "A", note: "Studio · 品牌设计师", region: "上海", online: true, star: true },
  { id: "lin", name: "林小满", avatar: px(14664508, 200, 200), initial: "L", note: "一起去看展吧 🌿", region: "杭州", online: true, star: true },
  { id: "mama", name: "妈妈", avatar: px(698532, 200, 200), initial: "M", note: "记得按时吃饭", region: "苏州", online: false, star: true },
  { id: "zhixia", name: "陈知夏", avatar: px(20894157, 200, 200), initial: "Z", note: "独立摄影师", region: "成都", online: true },
  { id: "yuran", name: "周予然", avatar: px(27688436, 200, 200), initial: "Z", note: "在路上的第 143 天", region: "大理", online: false },
  { id: "jiang", name: "江 屿", avatar: px(9854302, 200, 200), initial: "J", note: "产品 · 咖啡 · 电影", region: "北京", online: true },
  { id: "zhang", name: "张野", avatar: px(20885850, 200, 200), initial: "Z", note: "摄影工作室主理人", region: "杭州", online: false },
  { id: "ge", name: "葛一鸣", avatar: px(977418, 200, 200), initial: "G", note: "户外爱好者", region: "厦门", online: true },
  { id: "lu", name: "陆青", avatar: px(34081654, 200, 200), initial: "L", note: "建筑设计师", region: "昆明", online: false },
  { id: "hao", name: "郝然", avatar: px(977374, 200, 200), initial: "H", note: "今天也要骑远一点", region: "南京", online: true },
  { id: "qi", name: "齐北", avatar: px(14811388, 200, 200), initial: "Q", note: "制片人", region: "广州", online: false },
  { id: "tang", name: "唐一诺", avatar: px(654708, 200, 200), initial: "T", note: "插画师 · 接稿中", region: "杭州", online: true },
];

export type MessageKind = "text" | "image" | "voice" | "file" | "link" | "transfer" | "sticker" | "redpacket" | "system" | "location" | "video";

export type Message = {
  id: string;
  side: "me" | "them" | "system";
  kind: MessageKind;
  sender?: string;
  text?: string;
  image?: string;
  time?: string;
  duration?: number;
  title?: string;
  description?: string;
  size?: string;
  amount?: string;
};

export type Chat = {
  id: string;
  name: string;
  avatar: string;
  avatars?: string[];
  type: "direct" | "group" | "service" | "file";
  last: string;
  time: string;
  unread?: number;
  pinned?: boolean;
  muted?: boolean;
  icon?: "file" | "wechat";
  online?: boolean;
  draft?: string;
};

export const chats: Chat[] = [
  { id: "lin", name: "林小满", avatar: px(14664508, 200, 200), type: "direct", last: "那周六下午三点，美术馆见 🌿", time: "14:32", unread: 2, pinned: true, online: true },
  { id: "spring", name: "春日设计小组 (4)", avatar: px(35436850, 200, 200), avatars: [px(38768990, 100, 100), px(14664508, 100, 100), px(9854302, 100, 100), px(20894157, 100, 100)], type: "group", last: "Anna：[图片] 新的一版视觉稿，大家看看", time: "13:48", unread: 5, pinned: true },
  { id: "file", name: "文件传输助手", avatar: "", type: "file", last: "[文件] 微信设计规范 v2.4.pdf", time: "12:10", icon: "file" },
  { id: "mama", name: "妈妈", avatar: px(698532, 200, 200), type: "direct", last: "晚上回来吃饭吗？给你炖了汤", time: "11:26", unread: 1 },
  { id: "anna", name: "Anna Chen", avatar: px(38768990, 200, 200), type: "direct", last: "Fine, see you on Friday ✦", time: "10:03", online: true },
  { id: "weekend", name: "周末去哪儿", avatar: px(36328262, 200, 200), avatars: [px(14664508, 100, 100), px(977418, 100, 100), px(27688436, 100, 100), px(977374, 100, 100)], type: "group", last: "葛一鸣：天气预报说周六晴天，冲！", time: "昨天", muted: true },
  { id: "wechat", name: "微信团队", avatar: "", type: "service", icon: "wechat", last: "欢迎使用微信，这是一段新的旅程", time: "昨天" },
  { id: "zhixia", name: "陈知夏", avatar: px(20894157, 200, 200), type: "direct", last: "照片洗出来了，超好看", time: "周二" },
  { id: "jiang", name: "江 屿", avatar: px(9854302, 200, 200), type: "direct", last: "[语音] 8″", time: "周一" },
  { id: "studio", name: "Studio 2026", avatar: px(36933577, 200, 200), avatars: [px(9854302, 100, 100), px(38768990, 100, 100), px(14811388, 100, 100)], type: "group", last: "齐北：下周三提案，各位辛苦", time: "周一", muted: true },
  { id: "tang", name: "唐一诺", avatar: px(654708, 200, 200), type: "direct", last: "[红包] 恭喜发财，大吉大利", time: "周一", online: true },
  { id: "yuran", name: "周予然", avatar: px(27688436, 200, 200), type: "direct", last: "[图片] 大理的云", time: "7/18" },
];

export const conversations: Record<string, Message[]> = {
  lin: [
    { id: "l1", side: "system", kind: "system", text: "今天 14:26" },
    { id: "l2", side: "them", kind: "text", text: "刚看完「春日回声」的布展，给你留了两张票 🌿" },
    { id: "l3", side: "me", kind: "text", text: "真的吗！我正想约你周末去" },
    { id: "l4", side: "them", kind: "image", image: assets.gallery },
    { id: "l5", side: "them", kind: "text", text: "这周末一起去吧，下午三点的场次。光线最好的时候。" },
    { id: "l6", side: "me", kind: "text", text: "好呀，那我在美术馆门口等你" },
    { id: "l7", side: "them", kind: "sticker", text: "🌷" },
    { id: "l8", side: "them", kind: "text", text: "那周六下午三点，美术馆见 🌿" },
  ],
  spring: [
    { id: "s1", side: "system", kind: "system", text: "今天 13:42" },
    { id: "s1b", side: "system", kind: "system", text: "“陈知夏”邀请“唐一诺”加入了群聊" },
    { id: "s2", side: "them", kind: "text", sender: "Anna", text: "新的一版视觉稿，大家看看这个绿色饱和度会不会更舒服？" },
    { id: "s3", side: "them", kind: "image", sender: "Anna", image: assets.tulips },
    { id: "s4", side: "them", kind: "text", sender: "江屿", text: "我觉得可以，比上一版干净很多。桌面端的层级也顺了。" },
    { id: "s5", side: "me", kind: "text", text: "赞同，移动端的按钮再收一点圆角会更接近原生。" },
    { id: "s6", side: "them", kind: "text", sender: "陈知夏", text: "收到！我今晚把第二稿整理到 Figma ✦" },
  ],
  file: [
    { id: "f1", side: "system", kind: "system", text: "今天 12:08" },
    { id: "f2", side: "me", kind: "file", title: "微信设计规范 v2.4.pdf", size: "18.6 MB" },
    { id: "f3", side: "me", kind: "image", image: assets.desk },
    { id: "f4", side: "me", kind: "text", text: "WeChat redesign / 组件规范 / 2026.07.18" },
  ],
  mama: [
    { id: "m1", side: "system", kind: "system", text: "今天 11:24" },
    { id: "m2", side: "them", kind: "voice", duration: 12, text: "" },
    { id: "m3", side: "them", kind: "text", text: "晚上回来吃饭吗？给你炖了汤" },
    { id: "m4", side: "me", kind: "text", text: "回的，七点左右到家～想吃糖醋排骨" },
    { id: "m5", side: "them", kind: "sticker", text: "🥰" },
  ],
  anna: [
    { id: "a1", side: "system", kind: "system", text: "今天 10:01" },
    { id: "a2", side: "them", kind: "text", text: "The type spec looks great. Could we try one more version of the chat bubble?" },
    { id: "a3", side: "me", kind: "text", text: "Sure — I'll keep the tail subtle and soften the shadow." },
    { id: "a4", side: "them", kind: "sticker", text: "✦" },
    { id: "a5", side: "them", kind: "text", text: "Fine, see you on Friday ✦" },
  ],
  weekend: [
    { id: "w1", side: "system", kind: "system", text: "昨天 21:16" },
    { id: "w2", side: "them", kind: "text", sender: "葛一鸣", text: "天气预报说周六晴天，冲！" },
    { id: "w3", side: "them", kind: "text", sender: "周予然", text: "又是这张图，你已经在群里发了三次了哈哈" },
    { id: "w4", side: "them", kind: "image", sender: "葛一鸣", image: assets.rocks },
    { id: "w5", side: "me", kind: "text", text: "那就定周六，八点出发，我负责咖啡。" },
  ],
  wechat: [
    { id: "we1", side: "system", kind: "system", text: "昨天 09:00" },
    { id: "we2", side: "them", kind: "text", text: "欢迎使用微信，这是一段新的旅程。" },
    { id: "we3", side: "them", kind: "link", title: "微信使用小贴士", description: "了解聊天、朋友圈与支付的更多用法", image: assets.dawn },
  ],
  zhixia: [
    { id: "z1", side: "system", kind: "system", text: "周二 16:45" },
    { id: "z2", side: "them", kind: "image", image: assets.tulipGlass },
    { id: "z3", side: "them", kind: "text", text: "照片洗出来了，超好看" },
    { id: "z4", side: "me", kind: "text", text: "好喜欢这张，下次教我怎么拍" },
  ],
  jiang: [
    { id: "j1", side: "system", kind: "system", text: "周一 19:20" },
    { id: "j2", side: "them", kind: "voice", duration: 8, text: "" },
    { id: "j3", side: "me", kind: "text", text: "哈哈听到了，周五见" },
  ],
  tang: [
    { id: "t1", side: "system", kind: "system", text: "周一 15:02" },
    { id: "t2", side: "them", kind: "text", text: "画稿第一版出来啦，先给你看看～" },
    { id: "t3", side: "them", kind: "image", image: assets.posters },
    { id: "t4", side: "them", kind: "redpacket", text: "辛苦啦，请喝咖啡 ☕" },
  ],
  studio: [
    { id: "st1", side: "system", kind: "system", text: "周一 11:02" },
    { id: "st2", side: "them", kind: "text", sender: "齐北", text: "下周三提案，各位辛苦" },
    { id: "st3", side: "them", kind: "image", sender: "Anna", image: assets.posters },
  ],
  yuran: [
    { id: "y1", side: "system", kind: "system", text: "7月18日 17:40" },
    { id: "y2", side: "them", kind: "image", image: assets.dawn },
    { id: "y3", side: "them", kind: "text", text: "大理今天的云，像一张被揉开的纸。" },
    { id: "y4", side: "me", kind: "text", text: "想去。替我在洱海边多走一会儿。" },
  ],
};

export type Moment = {
  id: string;
  name: string;
  avatar: string;
  text: string;
  images?: string[];
  location?: string;
  time: string;
  likes: string[];
  comments: { name: string; text: string }[];
};

export const moments: Moment[] = [
  {
    id: "mo1",
    name: "林小满",
    avatar: px(14664508, 120, 120),
    text: "「春日回声」布置完成。把一整个春天，装进一间白房子。周末见 🌿",
    images: [assets.gallery, assets.tulips, assets.brunch],
    location: "天目里美术馆",
    time: "12 分钟前",
    likes: ["苏禾", "Anna Chen", "陈知夏", "周予然"],
    comments: [
      { name: "陈知夏", text: "已经在路上了！" },
      { name: "苏禾", text: "周六见 ✦" },
    ],
  },
  {
    id: "mo2",
    name: "周予然",
    avatar: px(27688436, 120, 120),
    text: "在路上的第 143 天。风把湖面吹皱的时候，刚好有人按下快门。",
    images: [assets.dawn],
    location: "大理 · 洱海",
    time: "2 小时前",
    likes: ["苏禾", "葛一鸣"],
    comments: [{ name: "葛一鸣", text: "下次带我。" }],
  },
  {
    id: "mo3",
    name: "陈知夏",
    avatar: px(20894157, 120, 120),
    text: "本周的静物练习，自然光是最好的滤镜。",
    images: [assets.pinkTulips, assets.croissant],
    time: "昨天",
    likes: ["苏禾", "林小满", "Anna Chen"],
    comments: [{ name: "Anna Chen", text: "The light ✦" }],
  },
  {
    id: "mo4",
    name: "江 屿",
    avatar: px(9854302, 120, 120),
    text: "城市咖啡地图 +1。这家手冲的老板说，周末会换新的豆子。",
    images: [assets.coffee],
    location: "三里屯 · %Arabica",
    time: "昨天",
    likes: ["苏禾", "唐一诺"],
    comments: [{ name: "苏禾", text: "下次一起去蹲新豆！" }],
  },
  {
    id: "mo5",
    name: "Anna Chen",
    avatar: px(38768990, 120, 120),
    text: "Studio visit day. Always inspired by how light moves through white spaces.",
    images: [assets.whiteWall, assets.desk],
    location: "Shanghai · Studio",
    time: "2 天前",
    likes: ["苏禾", "陈知夏", "齐北"],
    comments: [{ name: "齐北", text: "下次提案就在这里拍吧" }],
  },
];

export const favorites = [
  { id: "fa1", type: "链接", title: "A calm guide to designing interfaces", source: "designnotes.co", date: "7月18日", image: assets.whiteWall },
  { id: "fa2", type: "图片", title: "春日回声 · 布展现场", source: "林小满", date: "7月16日", image: assets.gallery },
  { id: "fa3", type: "笔记", title: "微信 UI 的设计原则：克制、清晰、有温度。", source: "我的笔记", date: "7月12日", image: "" },
  { id: "fa4", type: "语音", title: "关于第二版界面的反馈", source: "Anna Chen", date: "7月09日", image: "" },
  { id: "fa5", type: "文件", title: "WeChat-Design-System.fig", source: "春日设计小组", date: "7月02日", image: "" },
  { id: "fa6", type: "视频", title: "洱海边的第 143 天 · 完整版", source: "周予然", date: "6月28日", image: assets.dawn },
];

export const files = [
  { id: "fi1", name: "微信设计规范 v2.4.pdf", meta: "18.6 MB · 今天 12:10", icon: "pdf" },
  { id: "fi2", name: "WeChat-Desktop.fig", meta: "64.2 MB · 昨天 18:32", icon: "fig" },
  { id: "fi3", name: "春日回声_布展方案.pptx", meta: "12.4 MB · 7月16日", icon: "ppt" },
  { id: "fi4", name: "IMG_2481.HEIC", meta: "4.8 MB · 7月15日", icon: "img" },
  { id: "fi5", name: "聊天记录备份.zip", meta: "1.2 GB · 7月10日", icon: "zip" },
  { id: "fi6", name: "关于桌面端体验的 12 条想法.txt", meta: "2 KB · 7月08日", icon: "txt" },
];

export const miniApps = [
  { id: "m1", name: "腾讯地图", emoji: "🗺️", bg: "#E8F3FF" },
  { id: "m2", name: "微信读书", emoji: "📖", bg: "#EDF7F0" },
  { id: "m3", name: "滴滴出行", emoji: "🚕", bg: "#FFF4E3" },
  { id: "m4", name: "美团外卖", emoji: "🥡", bg: "#FFF1E8" },
  { id: "m5", name: "京东购物", emoji: "📦", bg: "#FFECEC" },
  { id: "m6", name: "猫眼电影", emoji: "🎬", bg: "#F1EDFF" },
  { id: "m7", name: "Keep", emoji: "🏃", bg: "#EFF7FF" },
  { id: "m8", name: "故宫博物院", emoji: "🏮", bg: "#FFF3ED" },
  { id: "m9", name: "微信指数", emoji: "📊", bg: "#ECF7F4" },
  { id: "m10", name: "微快递", emoji: "📮", bg: "#FFF8E5" },
  { id: "m11", name: "天气", emoji: "⛅", bg: "#EAF4FF" },
  { id: "m12", name: "更多", emoji: "⋯", bg: "#F4F4F4" },
];

export const transactions = [
  { id: "t1", name: "天目里美术馆", time: "今天 14:02", amount: "-120.00", emoji: "🎨" },
  { id: "t2", name: "微信红包 · 妈妈", time: "今天 11:30", amount: "+200.00", emoji: "🧧" },
  { id: "t3", name: "%Arabica 咖啡", time: "昨天 16:24", amount: "-32.00", emoji: "☕" },
  { id: "t4", name: "林小满", time: "昨天 13:10", amount: "-88.00", emoji: "🌿" },
  { id: "t5", name: "转账 · Anna", time: "7月16日", amount: "+1,280.00", emoji: "✦" },
  { id: "t6", name: "滴滴出行", time: "7月15日", amount: "-24.50", emoji: "🚕" },
  { id: "t7", name: "微信读书会员", time: "7月12日", amount: "-19.00", emoji: "📖" },
];

/* ---------- 新增：状态、公众号、位置 ---------- */

export const statuses = [
  { id: "s1", emoji: "🌿", label: "在春天里", desc: "分享此刻的心情", count: "3 位朋友" },
  { id: "s2", emoji: "☕", label: "喝咖啡", desc: "今日份咖啡因", count: "5 位朋友" },
  { id: "s3", emoji: "📷", label: "拍照", desc: "记录光与影", count: "2 位朋友" },
  { id: "s4", emoji: "🏃", label: "运动", desc: "今日步数 8,432", count: "4 位朋友" },
  { id: "s5", emoji: "📖", label: "阅读", desc: "正在读《日常的诗意》", count: "1 位朋友" },
  { id: "s6", emoji: "💤", label: "休息中", desc: "稍后回复消息", count: "6 位朋友" },
];

export const officialAccounts = [
  { id: "o1", name: "设计手记", avatar: px(20894157, 120, 120), desc: "在风经过的地方，重新理解日常", time: "12:30", unread: 2 },
  { id: "o2", name: "微信派", avatar: "", desc: "微信使用小贴士：让沟通更高效", time: "昨天", unread: 0 },
  { id: "o3", name: "天目里美术馆", avatar: px(35436850, 120, 120), desc: "「春日回声」展览开幕 · 限时预约中", time: "昨天", unread: 1 },
  { id: "o4", name: "看理想", avatar: px(9854302, 120, 120), desc: "新节目上线：城市漫步指南", time: "周二", unread: 0 },
];

export const nearbyPlaces = [
  { id: "p1", name: "天目里美术馆", addr: "杭州市西湖区 · 距你 220m", tag: "展览" },
  { id: "p2", name: "%Arabica 咖啡", addr: "天目里 B1 · 距你 180m", tag: "咖啡" },
  { id: "p3", name: "茑屋书店", addr: "天目里 2F · 距你 260m", tag: "书店" },
  { id: "p4", name: "KEEP 运动空间", addr: "西溪路 640m · 距你 640m", tag: "健身" },
];

export const transferPresets = ["8.88", "66.66", "88.88", "128.00", "520.00"];
