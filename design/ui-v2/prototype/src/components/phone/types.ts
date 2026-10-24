export type MobileScreen =
  | "chats"
  | "contacts"
  | "discover"
  | "me"
  | "chat"
  | "moments"
  | "profile"
  | "channels"
  | "mini"
  | "wallet"
  | "settings"
  | "favorites"
  | "stickers"
  | "cards"
  | "search"
  | "scan"
  | "pay"
  | "redpacket"
  | "call"
  | "reading"
  | "newfriends"
  | "login"
  | "status"
  | "transfer"
  | "location"
  | "official"
  | "mycode";

export type MobilePageMeta = {
  id: MobileScreen;
  title: string;
  english: string;
  caption: string;
  group: "核心" | "关系" | "发现" | "我" | "服务";
};

export const MOBILE_PAGES: MobilePageMeta[] = [
  { id: "chats", title: "聊天列表", english: "Chats", caption: "置顶会话、未读与消息预览", group: "核心" },
  { id: "chat", title: "聊天详情", english: "Conversation", caption: "图文消息、语音与输入工具栏", group: "核心" },
  { id: "contacts", title: "通讯录", english: "Contacts", caption: "好友分组与字母索引", group: "关系" },
  { id: "profile", title: "好友资料", english: "Profile", caption: "备注、共同群聊与朋友圈", group: "关系" },
  { id: "newfriends", title: "新的朋友", english: "New Friends", caption: "好友申请与添加来源", group: "关系" },
  { id: "official", title: "公众号", english: "Official", caption: "订阅号消息与文章列表", group: "关系" },
  { id: "discover", title: "发现", english: "Discover", caption: "朋友圈、视频号与小程序的入口", group: "发现" },
  { id: "moments", title: "朋友圈", english: "Moments", caption: "封面、动态、点赞与评论", group: "发现" },
  { id: "channels", title: "视频号", english: "Channels", caption: "沉浸式短视频与直播", group: "发现" },
  { id: "mini", title: "小程序", english: "Mini Programs", caption: "最近使用与分类推荐", group: "发现" },
  { id: "search", title: "搜一搜", english: "Search", caption: "全局搜索与热搜内容", group: "发现" },
  { id: "scan", title: "扫一扫", english: "Scan", caption: "取景框、识别与闪光灯", group: "发现" },
  { id: "reading", title: "公众号文章", english: "Article", caption: "图文阅读与内容互动", group: "发现" },
  { id: "location", title: "位置共享", english: "Location", caption: "实时位置与附近地点", group: "发现" },
  { id: "me", title: "我", english: "Me", caption: "个人资料与服务入口", group: "我" },
  { id: "status", title: "我的状态", english: "Status", caption: "心情状态与朋友动态", group: "我" },
  { id: "mycode", title: "我的二维码", english: "My QR", caption: "名片二维码与换样式", group: "我" },
  { id: "favorites", title: "收藏", english: "Favorites", caption: "笔记、图片、链接与文件", group: "我" },
  { id: "cards", title: "卡包", english: "Cards", caption: "会员卡、优惠券与票证", group: "我" },
  { id: "stickers", title: "表情", english: "Stickers", caption: "收藏表情与表情包", group: "我" },
  { id: "settings", title: "设置", english: "Settings", caption: "账号安全、通用与隐私", group: "我" },
  { id: "login", title: "登录", english: "Sign In", caption: "手机号登录与欢迎页", group: "我" },
  { id: "wallet", title: "服务", english: "Services", caption: "钱包、交易与生活服务", group: "服务" },
  { id: "pay", title: "收付款", english: "Pay", caption: "二维码收款与付款方式", group: "服务" },
  { id: "transfer", title: "转账", english: "Transfer", caption: "金额输入与留言备注", group: "服务" },
  { id: "redpacket", title: "微信红包", english: "Red Packet", caption: "红包封面与开红包动效", group: "服务" },
  { id: "call", title: "视频通话", english: "Call", caption: "全屏视频通话与通话控制", group: "服务" },
];

export const DESKTOP_PAGES = [
  { id: "chats", title: "聊天", english: "Chats", caption: "会话列表与即时对话" },
  { id: "contacts", title: "通讯录", english: "Contacts", caption: "好友资料与关系管理" },
  { id: "favorites", title: "收藏", english: "Favorites", caption: "多类型内容收藏与阅读" },
  { id: "files", title: "文件", english: "Files", caption: "聊天文件、传输与存储" },
  { id: "moments", title: "朋友圈", english: "Moments", caption: "朋友动态与互动" },
  { id: "channels", title: "视频号", english: "Channels", caption: "短视频、直播与创作者" },
  { id: "mini", title: "小程序", english: "Mini Programs", caption: "发现与最近使用" },
  { id: "wallet", title: "支付", english: "Pay", caption: "钱包、交易与收付款" },
  { id: "login", title: "扫码登录", english: "Sign In", caption: "二维码登录与设备管理" },
  { id: "settings", title: "设置", english: "Settings", caption: "偏好、隐私与存储" },
];

export const TOTAL_SCREENS = MOBILE_PAGES.length + DESKTOP_PAGES.length;
