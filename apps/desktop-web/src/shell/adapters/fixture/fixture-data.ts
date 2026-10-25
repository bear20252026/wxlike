import type {
  ChatMessage,
  Contact,
  Conversation,
  FriendRequest,
  GroupMember,
  Preferences,
} from "../../models";

export interface FixtureAccount {
  user_id: string;
  name: string;
  avatar: string;
  password: string;
  signature: string;
  contacts: Contact[];
  requests: FriendRequest[];
  groups: { group_id: string; name: string; avatar: string; my_role: "owner" | "admin" | "member"; members: GroupMember[] }[];
  conversations: Conversation[];
  messages: Record<string, ChatMessage[]>;
  preferences: Preferences;
  drafts: Record<string, string>;
}

const now = Date.now();
const at = (minutesAgo: number) => new Date(now - minutesAgo * 60_000).toISOString();
const seq = (n: number) => String(n);

export const FIXTURE_PREFERENCES: Preferences = {
  theme: "light",
  compact: false,
  notification_preview: true,
  enter_to_send: true,
  sync_range_days: 7,
  auto_login: false,
};

function contact(
  user_id: string,
  name: string,
  region: string,
  note: string,
  extra: Partial<Contact> = {},
): Contact {
  return {
    user_id,
    name,
    region,
    note,
    avatar: "",
    tags: extra.tags ?? [],
    star: extra.star ?? false,
    online: extra.online ?? false,
    blocked: false,
  };
}

function message(
  msg_id: string,
  conv_id: string,
  seqNo: number,
  direction: "in" | "out",
  text: string,
  minutesAgo: number,
  sender: { user_id: string; name: string },
): ChatMessage {
  return {
    msg_id,
    conv_id,
    seq: seq(seqNo),
    sender,
    direction,
    body: { kind: "text", text },
    state: "DEVICE_STORED",
    created_at_ms: at(minutesAgo),
  };
}

function member(user_id: string, name: string, role: "owner" | "admin" | "member", days: number): GroupMember {
  return { user_id, name, avatar: "", role, joined_at_ms: at(days * 1440) };
}

export function createAlphaAccount(): FixtureAccount {
  const lin: Contact = contact("user-lin", "林小满", "杭州", "一起去看展吧", {
    star: true,
    online: true,
    tags: ["设计", "朋友"],
  });
  const anna = contact("user-anna", "Anna Chen", "上海", "Studio · 品牌设计师", {
    star: true,
    online: true,
    tags: ["设计"],
  });
  const zhixia = contact("user-zhixia", "陈知夏", "成都", "独立摄影师", { online: true });
  const mama = contact("user-mama", "妈妈", "苏州", "记得按时吃饭", { star: true });

  const springMembers: GroupMember[] = [
    member("user-alpha", "苏禾", "owner", 90),
    member("user-anna", "Anna Chen", "admin", 80),
    member("user-zhixia", "陈知夏", "member", 60),
    member("user-tang", "唐一诺", "member", 20),
  ];
  const weekendMembers: GroupMember[] = [
    member("user-hao", "郝然", "owner", 120),
    member("user-alpha", "苏禾", "member", 100),
    member("user-ge", "葛一鸣", "member", 70),
  ];

  return {
    user_id: "user-alpha",
    name: "苏禾",
    avatar: "",
    password: "demo1234",
    signature: "把日常过成喜欢的样子。",
    contacts: [lin, anna, zhixia, mama],
    requests: [
      {
        request_id: "req-alpha-1",
        from: { user_id: "user-tang", name: "唐一诺", avatar: "" },
        message: "我是唐一诺，插画师，想加你聊合作",
        status: "PENDING",
        created_at_ms: at(180),
      },
    ],
    groups: [
      {
        group_id: "group-spring",
        name: "春日设计小组",
        avatar: "",
        my_role: "owner",
        members: springMembers,
      },
      {
        group_id: "group-weekend",
        name: "周末去哪儿",
        avatar: "",
        my_role: "member",
        members: weekendMembers,
      },
    ],
    conversations: [
      {
        conv_id: "conv-lin",
        kind: "direct",
        title: "林小满",
        peer_user_id: "user-lin",
        unread: 2,
        pinned: true,
        muted: false,
        last_preview: "那周六下午三点，美术馆见",
        last_at_ms: at(8),
        draft: "",
      },
      {
        conv_id: "conv-spring",
        kind: "group",
        title: "春日设计小组 (4)",
        peer_user_id: undefined,
        unread: 5,
        pinned: true,
        muted: false,
        last_preview: "Anna：[图片] 新的一版视觉稿",
        last_at_ms: at(44),
        draft: "",
      },
      {
        conv_id: "conv-file",
        kind: "file",
        title: "文件传输助手",
        unread: 0,
        pinned: false,
        muted: false,
        last_preview: "[文件] 设计规范.pdf",
        last_at_ms: at(120),
        draft: "",
      },
      {
        conv_id: "conv-mama",
        kind: "direct",
        title: "妈妈",
        peer_user_id: "user-mama",
        unread: 1,
        pinned: false,
        muted: false,
        last_preview: "晚上回来吃饭吗？",
        last_at_ms: at(150),
        draft: "",
      },
      {
        conv_id: "conv-anna",
        kind: "direct",
        title: "Anna Chen",
        peer_user_id: "user-anna",
        unread: 0,
        pinned: false,
        muted: false,
        last_preview: "Fine, see you on Friday",
        last_at_ms: at(210),
        draft: "",
      },
    ],
    messages: {
      "conv-lin": [
        message("m-lin-1", "conv-lin", 1, "in", "刚看完「春日回声」的布展，给你留了两张票", 300, {
          user_id: "user-lin",
          name: "林小满",
        }),
        message("m-lin-2", "conv-lin", 2, "out", "真的吗！我正想约你周末去", 290, {
          user_id: "user-alpha",
          name: "苏禾",
        }),
        message("m-lin-3", "conv-lin", 3, "in", "这周末一起去吧，下午三点的场次。", 280, {
          user_id: "user-lin",
          name: "林小满",
        }),
        message("m-lin-4", "conv-lin", 4, "out", "好呀，那我在美术馆门口等你", 270, {
          user_id: "user-alpha",
          name: "苏禾",
        }),
        message("m-lin-5", "conv-lin", 5, "in", "那周六下午三点，美术馆见", 8, {
          user_id: "user-lin",
          name: "林小满",
        }),
      ],
      "conv-spring": [
        message("m-spring-1", "conv-spring", 1, "in", "新的一版视觉稿，大家看看这个绿色饱和度？", 60, {
          user_id: "user-anna",
          name: "Anna Chen",
        }),
        message("m-spring-2", "conv-spring", 2, "out", "赞同，移动端的按钮再收一点圆角。", 50, {
          user_id: "user-alpha",
          name: "苏禾",
        }),
        message("m-spring-3", "conv-spring", 3, "in", "收到！我今晚把第二稿整理到 Figma", 44, {
          user_id: "user-zhixia",
          name: "陈知夏",
        }),
      ],
      "conv-file": [
        message("m-file-1", "conv-file", 1, "out", "WeChat redesign / 组件规范 / 2026.07.18", 120, {
          user_id: "user-alpha",
          name: "苏禾",
        }),
      ],
      "conv-mama": [
        message("m-mama-1", "conv-mama", 1, "in", "晚上回来吃饭吗？给你炖了汤", 150, {
          user_id: "user-mama",
          name: "妈妈",
        }),
        message("m-mama-2", "conv-mama", 2, "out", "回的，七点左右到家", 145, {
          user_id: "user-alpha",
          name: "苏禾",
        }),
      ],
      "conv-anna": [
        message("m-anna-1", "conv-anna", 1, "in", "The type spec looks great.", 220, {
          user_id: "user-anna",
          name: "Anna Chen",
        }),
        message("m-anna-2", "conv-anna", 2, "in", "Fine, see you on Friday", 210, {
          user_id: "user-anna",
          name: "Anna Chen",
        }),
      ],
    },
    preferences: { ...FIXTURE_PREFERENCES },
    drafts: {},
  };
}

export function createBetaAccount(): FixtureAccount {
  const alpha = contact("user-alpha", "苏禾", "杭州", "把日常过成喜欢的样子", {
    star: true,
    online: true,
  });
  const ge = contact("user-ge", "葛一鸣", "厦门", "户外爱好者", { online: true });

  return {
    user_id: "user-beta",
    name: "林小满",
    avatar: "",
    password: "demo1234",
    signature: "把一整个春天，装进一间白房子。",
    contacts: [alpha, ge],
    requests: [
      {
        request_id: "req-beta-1",
        from: { user_id: "user-yuran", name: "周予然", avatar: "" },
        message: "我是周予然，在大理拍照",
        status: "PENDING",
        created_at_ms: at(90),
      },
    ],
    groups: [
      {
        group_id: "group-spring",
        name: "春日设计小组",
        avatar: "",
        my_role: "admin",
        members: [
          member("user-alpha", "苏禾", "owner", 90),
          member("user-beta", "林小满", "admin", 90),
          member("user-anna", "Anna Chen", "member", 80),
        ],
      },
    ],
    conversations: [
      {
        conv_id: "conv-alpha",
        kind: "direct",
        title: "苏禾",
        peer_user_id: "user-alpha",
        unread: 0,
        pinned: true,
        muted: false,
        last_preview: "好呀，那我在美术馆门口等你",
        last_at_ms: at(270),
        draft: "",
      },
    ],
    messages: {
      "conv-alpha": [
        message("b-1", "conv-alpha", 1, "out", "刚看完「春日回声」的布展，给你留了两张票", 300, {
          user_id: "user-beta",
          name: "林小满",
        }),
        message("b-2", "conv-alpha", 2, "in", "真的吗！我正想约你周末去", 290, {
          user_id: "user-alpha",
          name: "苏禾",
        }),
        message("b-3", "conv-alpha", 3, "out", "这周末一起去吧，下午三点的场次。", 280, {
          user_id: "user-beta",
          name: "林小满",
        }),
      ],
    },
    preferences: {
      ...FIXTURE_PREFERENCES,
      theme: "light",
      enter_to_send: true,
    },
    drafts: {},
  };
}

export const FIXTURE_DIRECTORY = [
  { user_id: "user-yuran", name: "周予然", region: "大理", note: "在路上的第 143 天" },
  { user_id: "user-tang", name: "唐一诺", region: "杭州", note: "插画师 · 接稿中" },
  { user_id: "user-jiang", name: "江 屿", region: "北京", note: "产品 · 咖啡 · 电影" },
  { user_id: "user-ge", name: "葛一鸣", region: "厦门", note: "户外爱好者" },
];
