/**
 * Rendering: views subscribe to the atoms they read; interactions dispatch
 * through `api.ts`. HuLa-inspired bits from the task book's checklist live
 * here: swipeable session rows, expanding search, distinct bubbles, image
 * grid, chat transition, and a full dark-mode theme (see styles.css).
 *
 * P1 additions: paged history ("加载更早"), typing indicator, read-receipt
 * marks on own bubbles, the bubble action menu (收藏/转发/复制/撤回/多选),
 * merge-forward selection, in-chat search, secret-chat toggle, favorites tab.
 */

import { appStore, toast, type ChatMsg, type Session } from "./state";
import {
  balanceAtom,
  chatsAtom,
  connectedAtom,
  favoritesAtom,
  friendsAtom,
  historyCursorAtom,
  meAtom,
  requestsAtom,
  searchHitsAtom,
  selectModeAtom,
  searchOpenAtom,
  sessionsAtom,
  typingAtom,
  viewAtom,
} from "./state";
import * as api from "./api";
import { renderCallPage, renderVerifyPage, stopCallPolling } from "./callpage";
import ListenerSetter from "./vendor/tweb/listenerSetter";

type El = HTMLElement;

function h(tag: string, cls?: string, text?: string): El {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text !== undefined) el.textContent = text;
  return el;
}

function clear(el: El): void {
  while (el.firstChild) el.removeChild(el.firstChild);
}

function on<K extends keyof HTMLElementEventMap>(
  el: El,
  kind: K,
  fn: (e: HTMLElementEventMap[K]) => void,
): void {
  el.addEventListener(kind, fn as EventListener);
}

/**
 * 菜单"点外面关闭"的监听器集中管理（借用 tweb `ListenerSetter`）。
 * 之前的写法只在"下一次点击"时自清；菜单被切视图/重渲染关掉时监听器会挂着。
 */
const menuListeners = new ListenerSetter();

/** Conversations where the secret-chat toggle is on (P1 prototype). */
const secretConvs = new Set<string>();

// ---------------------------------------------------------------------------
// auth screen
// ---------------------------------------------------------------------------

function renderAuth(root: El): void {
  clear(root);
  const card = h("div", "auth-card");
  const title = h("h1", "auth-title", "wxlike");
  const sub = h("p", "auth-sub", "登录或注册一个账号开始聊天");

  const server = h("input", "input") as HTMLInputElement;
  server.value = localStorage.getItem("wxlike-server") ?? "http://127.0.0.1:8099";
  server.placeholder = "服务器地址";

  const name = h("input", "input") as HTMLInputElement;
  name.placeholder = "用户名";
  const pass = h("input", "input") as HTMLInputElement;
  pass.type = "password";
  pass.placeholder = "密码";

  const loginBtn = h("button", "btn primary", "登录");
  const regBtn = h("button", "btn", "注册");
  const err = h("p", "auth-error");

  async function go(create: boolean): Promise<void> {
    err.textContent = "";
    try {
      localStorage.setItem("wxlike-server", server.value.trim());
      await api.setServer(server.value.trim());
      const reply = create
        ? await api.register(name.value.trim(), pass.value)
        : await api.login(name.value.trim(), pass.value);
      const me = await api.fetchMe();
      appStore.set(meAtom, { ...me, user_id: me.user_id || reply.user_id });
      await api.connectHub(localStorage.getItem("wxlike-hub") ?? "127.0.0.1:9443");
      await Promise.all([api.loadFriends(), api.loadRequests(), api.loadBalance()]);
      appStore.set(viewAtom, { screen: "main", tab: "chats" });
    } catch (e) {
      err.textContent = String(e).replace(/^.*error"?[: ]*/i, "").slice(0, 160);
    }
  }

  on(loginBtn, "click", () => void go(false));
  on(regBtn, "click", () => void go(true));
  on(pass, "keydown", (e) => {
    if (e.key === "Enter") void go(false);
  });

  const dark = h("button", "btn ghost corner", "🌙");
  on(dark, "click", () => api.toggleDark());

  card.append(title, sub, server, name, pass, err, loginBtn, regBtn, dark);
  root.append(card);
}

// ---------------------------------------------------------------------------
// main shell (sessions / friends / favorites / me)
// ---------------------------------------------------------------------------

function renderMain(root: El): void {
  clear(root);
  const sidebar = h("aside", "sidebar");
  const content = h("main", "content");
  content.append(h("div", "empty", "选择一个会话开始聊天"));
  root.append(sidebar, content);

  const view = appStore.get(viewAtom);
  const tab = view.screen === "main" ? view.tab : "chats";

  // -- sidebar header: brand + expanding search + theme
  const header = h("div", "side-head");
  const brand = h("div", "brand", "wxlike");
  const searchWrap = h("div", "search-wrap");
  const searchBtn = h("button", "icon-btn", "🔍");
  const searchInput = h("input", "search-input") as HTMLInputElement;
  searchInput.placeholder = "搜索";
  searchWrap.append(searchBtn, searchInput);
  const themeBtn = h("button", "icon-btn", "🌗");
  header.append(brand, searchWrap, themeBtn);

  // expanding search (复刻清单: 搜索展开动画)
  if (appStore.get(searchOpenAtom)) searchWrap.classList.add("open");
  on(searchBtn, "click", () => {
    searchWrap.classList.toggle("open");
    appStore.set(searchOpenAtom, searchWrap.classList.contains("open"));
    if (searchWrap.classList.contains("open")) searchInput.focus();
  });

  // -- tabs
  const tabs = h("div", "tabs");
  const tabBtns: Record<string, El> = {};
  const tabDefs: [string, string][] = [
    ["chats", "聊天"],
    ["friends", "通讯录"],
    ["favorites", "收藏"],
    ["me", "我"],
  ];
  for (const [key, label] of tabDefs) {
    const b = h("button", `tab ${tab === key ? "active" : ""}`, label);
    b.dataset.tab = key;
    on(b, "click", () => appStore.set(viewAtom, { screen: "main", tab: key as never }));
    tabBtns[key] = b;
    tabs.append(b);
  }

  const conn = h("div", `conn ${appStore.get(connectedAtom) ? "ok" : "bad"}`);
  on(conn, "click", () => void reconnect());
  conn.title = "点击重连 hub";

  const list = h("div", "side-list");
  sidebar.append(header, tabs, list, conn, themeBtn);

  const syncTabHighlight = (): void => {
    const v = appStore.get(viewAtom);
    const active = v.screen === "main" ? v.tab : "chats";
    for (const [key, b] of Object.entries(tabBtns)) b.classList.toggle("active", key === active);
  };
  syncTabHighlight();

  const drawList = (): void => {
    const v = appStore.get(viewAtom);
    const active = v.screen === "main" ? v.tab : "chats";
    clear(list);
    if (active === "chats") drawSessions(list, searchInput.value);
    else if (active === "friends") drawFriends(list);
    else if (active === "favorites") drawFavorites(list);
    else drawMe(list);
  };

  for (const atom of [
    sessionsAtom,
    friendsAtom,
    requestsAtom,
    viewAtom,
    balanceAtom,
    meAtom,
    connectedAtom,
    favoritesAtom,
  ]) {
    appStore.sub(atom, () => {
      drawList();
      syncTabHighlight();
    });
  }
  on(searchInput, "input", drawList);
  on(themeBtn, "click", () => api.toggleDark());

  if (tab === "favorites") void api.favoritesLoad().catch(() => undefined);
  drawList();
}

async function reconnect(): Promise<void> {
  try {
    await api.connectHub(localStorage.getItem("wxlike-hub") ?? "127.0.0.1:9443");
    toast("已重连");
  } catch (e) {
    toast(`重连失败: ${String(e).slice(0, 80)}`);
  }
}

function drawSessions(list: El, filter: string): void {
  const sessions = appStore.get(sessionsAtom).filter((s) =>
    s.title.toLowerCase().includes(filter.toLowerCase()),
  );
  if (sessions.length === 0) {
    list.append(h("div", "empty", "还没有会话 — 去通讯录发起聊天吧"));
    return;
  }
  for (const s of sessions) list.append(sessionRow(s));
}

/** Session row with swipe-to-reveal actions (复刻清单: 会话列表滑动操作). */
function sessionRow(s: Session): El {
  const row = h("div", "session-row");
  const face = h("div", "face", s.isGroup ? "👥" : (s.title.slice(0, 1) || "?").toUpperCase());
  const main = h("div", "session-main");
  const top = h("div", "session-top");
  top.append(h("span", "session-title", s.title));
  if (s.lastAtMs > 0) top.append(h("span", "session-time", timeShort(s.lastAtMs)));
  const bottom = h("div", "session-bottom");
  bottom.append(h("span", "session-preview", s.lastText || " "));
  if (s.unread > 0) bottom.append(h("span", "badge", String(s.unread)));
  main.append(top, bottom);
  row.append(face, main);

  const actions = h("div", "row-actions");
  const del = h("button", "row-action danger", "删除");
  on(del, "click", (e) => {
    e.stopPropagation();
    const sessions = appStore.get(sessionsAtom).filter((x) => x.convId !== s.convId);
    appStore.set(sessionsAtom, sessions);
  });
  actions.append(del);
  row.append(actions);

  // pointer-driven swipe: drag left reveals the action strip
  let startX = 0;
  let dragging = false;
  on(row, "pointerdown", (e) => {
    startX = e.clientX;
    dragging = true;
  });
  on(row, "pointermove", (e) => {
    if (!dragging) return;
    const dx = Math.min(0, e.clientX - startX);
    row.style.transform = `translateX(${Math.max(dx, -72)}px)`;
  });
  on(row, "pointerup", (e) => {
    dragging = false;
    const dx = e.clientX - startX;
    row.style.transform = dx < -36 ? "translateX(-72px)" : "";
  });
  on(row, "pointerleave", () => {
    dragging = false;
    row.style.transform = "";
  });
  on(row, "click", () => void openChat(s));
  return row;
}

async function openChat(s: Session): Promise<void> {
  // group chats read member titles from the group cache
  api.openSession(s);
}

function drawFriends(list: El): void {
  const friends = appStore.get(friendsAtom);
  const requests = appStore.get(requestsAtom);

  if (requests.length > 0) {
    const head = h("div", "list-head", "好友申请");
    list.append(head);
    for (const r of requests) {
      const row = h("div", "friend-row");
      row.append(h("div", "face", "?"), h("div", "friend-name", r.from_uid));
      const actions = h("div", "friend-actions");
      const yes = h("button", "btn tiny primary", "同意");
      const no = h("button", "btn tiny", "拒绝");
      on(yes, "click", () => void api.acceptRequest(r.id));
      on(no, "click", () => void api.declineRequest(r.id));
      actions.append(yes, no);
      row.append(actions);
      list.append(row);
    }
  }

  const head = h("div", "list-head", `通讯录 (${friends.length})`);
  const add = h("button", "btn tiny primary", "+ 加好友");
  on(add, "click", () => void addFriendDialog());
  head.append(add);
  list.append(head);

  if (friends.length === 0) list.append(h("div", "empty", "还没有好友"));
  for (const f of friends) {
    const row = h("div", "friend-row");
    row.append(h("div", "face", (f.remark || f.friend_id).slice(0, 1).toUpperCase()));
    row.append(h("div", "friend-name", f.remark || f.friend_id));
    const chat = h("button", "btn tiny primary", "发消息");
    on(chat, "click", () => {
      const me = appStore.get(meAtom);
      if (!me) return;
      const convId = `d-${[me.user_id, f.friend_id].sort().join("~")}`;
      api.openSession({ convId, title: f.remark || f.friend_id, isGroup: false, lastText: "", lastAtMs: 0, unread: 0 });
    });
    row.append(chat);
    list.append(row);
  }

  const groupHead = h("div", "list-head", "群聊");
  const create = h("button", "btn tiny primary", "+ 建群");
  on(create, "click", () => void createGroupDialog());
  groupHead.append(create);
  list.append(groupHead);
}

async function addFriendDialog(): Promise<void> {
  const to = window.prompt("对方用户 ID:");
  if (!to) return;
  const message = window.prompt("验证消息（可留空）:") ?? "";
  try {
    await api.sendFriendRequest(to.trim(), message);
    toast("申请已发送");
  } catch (e) {
    toast(String(e).slice(0, 120));
  }
}

async function createGroupDialog(): Promise<void> {
  const friends = appStore.get(friendsAtom);
  if (friends.length === 0) {
    toast("先加好友才能建群");
    return;
  }
  const picked = friends
    .map((f, i) => `${i + 1}. ${f.remark || f.friend_id} (${f.friend_id})`)
    .join("\n");
  const sel = window.prompt(`输入要拉进群的成员序号（逗号分隔）:\n${picked}`);
  if (!sel) return;
  const idxs = sel.split(",").map((x) => parseInt(x.trim(), 10) - 1).filter((i) => i >= 0 && i < friends.length);
  if (idxs.length === 0) return;
  const name = window.prompt("群名称:", "新群聊") ?? "新群聊";
  try {
    const groupId = await api.createGroup(name, idxs.map((i) => friends[i].friend_id));
    toast(`群 ${name} 已创建`);
    api.openSession({ convId: groupId, title: name, isGroup: true, lastText: "", lastAtMs: 0, unread: 0 });
  } catch (e) {
    toast(String(e).slice(0, 120));
  }
}

function drawFavorites(list: El): void {
  const rows = appStore.get(favoritesAtom);
  const head = h("div", "list-head", `收藏 (${rows.length})`);
  list.append(head);
  if (rows.length === 0) {
    list.append(h("div", "empty", "长按/右键消息可以收藏"));
    return;
  }
  for (const f of rows) {
    const row = h("div", "fav-row");
    const main = h("div", "fav-main");
    main.append(h("div", "fav-text", f.text || `[${f.kind}]`));
    main.append(h("div", "fav-meta", `${f.sender.slice(0, 8)} · ${timeShort(f.sent_at_ms)}`));
    const del = h("button", "btn tiny", "删除");
    on(del, "click", () => void api.favoriteRemove(f.id).catch((e) => toast(String(e).slice(0, 100))));
    row.append(main, del);
    list.append(row);
  }
}

function drawMe(list: El): void {
  const me = appStore.get(meAtom);
  const balance = appStore.get(balanceAtom);
  if (!me) return;
  const card = h("div", "me-card");
  const face = h("div", "face big", me.name.slice(0, 1).toUpperCase());
  if (me.avatar) {
    // P2 头像接媒体: the avatar field is a media id since the wiring landed
    void api
      .fetchMediaDataUrl(me.avatar, true)
      .then((url) => {
        const img = document.createElement("img");
        img.src = url;
        img.style.width = "100%";
        img.style.height = "100%";
        img.style.borderRadius = "16px";
        img.style.objectFit = "cover";
        face.replaceChildren(img);
      })
      .catch(() => undefined);
  }
  card.append(face);
  card.append(h("div", "me-name", me.name));
  card.append(h("div", "me-id", me.user_id));
  card.append(h("div", "me-sign", me.signature || "这个人很懒，什么都没写"));
  const avatarInput = h("input") as HTMLInputElement;
  avatarInput.type = "file";
  avatarInput.accept = "image/*";
  avatarInput.hidden = true;
  const avatarBtn = h("button", "btn", "设置头像");
  on(avatarBtn, "click", () => avatarInput.click());
  on(avatarInput, "change", () => {
    const file = avatarInput.files?.[0];
    if (file) {
      void api
        .uploadAvatar(file)
        .then(() => api.fetchMe())
        .then(() => {
          toast("头像已更新");
          appStore.set(viewAtom, { screen: "main", tab: "me" });
        })
        .catch((e) => toast(String(e).slice(0, 120)));
    }
    avatarInput.value = "";
  });
  const wallet = h("div", "wallet");
  wallet.append(h("span", "wallet-label", "钱包余额"));
  wallet.append(h("span", "wallet-amount", `${(balance / 100).toFixed(2)} 元`));
  const sig = h("input", "input") as HTMLInputElement;
  sig.placeholder = "写一句签名…";
  sig.value = me.signature;
  const save = h("button", "btn primary", "保存签名");
  on(save, "click", () => {
    void api.invokeUpdateProfile(sig.value).catch((e) => toast(String(e).slice(0, 120)));
  });
  const secret = h("button", "btn", "启用秘密聊天 (E2EE)");
  on(secret, "click", () => {
    void api.secretEnable().catch((e) => toast(String(e).slice(0, 120)));
  });

  // P4 设备管理：登录会话 + 推送令牌，可远程下线（服务端立即失效）
  const devicesBtn = h("button", "btn", "设备管理");
  const devicesPanel = h("div", "devices");
  devicesPanel.hidden = true;
  on(devicesBtn, "click", () => {
    devicesPanel.hidden = !devicesPanel.hidden;
    if (!devicesPanel.hidden) void drawDevices(devicesPanel);
  });

  // 网络诊断：DNS → TCP → HTTP → QUIC，卡在哪一跳一目了然
  const diagBtn = h("button", "btn", "网络诊断");
  const diagPanel = h("div", "devices");
  diagPanel.hidden = true;
  on(diagBtn, "click", () => {
    diagPanel.hidden = !diagPanel.hidden;
    if (!diagPanel.hidden) void drawDiag(diagPanel);
  });

  list.append(
    card,
    avatarBtn,
    avatarInput,
    wallet,
    sig,
    save,
    secret,
    devicesBtn,
    devicesPanel,
    diagBtn,
    diagPanel,
  );
}

const DIAG_LABELS: Record<string, string> = {
  dns: "域名解析",
  tcp: "TCP 连接",
  http: "HTTP 探活",
  quic: "QUIC 握手",
};

const DIAG_STATUS: Record<string, string> = {
  ok: "通过",
  failed: "失败",
  skipped: "跳过",
};

/** 网络诊断面板。诊断比普通请求慢（每步上限 5 秒），所以先渲染"检测中"。 */
async function drawDiag(host: El): Promise<void> {
  clear(host);
  host.append(h("div", "devices-title", "网络诊断"));
  host.append(h("div", "devices-empty", "检测中…"));
  let report: api.DiagReport;
  try {
    report = await api.netDiag();
  } catch (e) {
    clear(host);
    host.append(h("div", "devices-title", "网络诊断"));
    host.append(h("div", "devices-empty", String(e).slice(0, 160)));
    return;
  }
  clear(host);
  host.append(h("div", "devices-title", "网络诊断"));
  host.append(h("div", "devices-empty", report.summary));
  for (const s of report.steps) {
    const row = h("div", "device-row");
    const left = h("div", "device-info");
    const label = DIAG_LABELS[s.step] ?? s.step;
    const state = DIAG_STATUS[s.status.status] ?? s.status.status;
    left.append(h("div", "device-name", `${label} · ${state}`));
    left.append(
      h("div", "device-meta", s.elapsed_ms > 0 ? `${s.elapsed_ms}ms · ${s.status.detail}` : s.status.detail),
    );
    row.append(left);
    host.append(row);
  }
}

/** 相对时间：设备页只需要"多久没活跃"，不需要绝对时间。 */
function since(ms: number): string {
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  return `${Math.floor(diff / 86_400_000)} 天前`;
}

async function drawDevices(host: El): Promise<void> {
  clear(host);
  host.append(h("div", "devices-title", "登录设备"));
  let reply: api.SessionsReply;
  try {
    reply = await api.sessionsList();
  } catch (e) {
    host.append(h("div", "devices-empty", String(e).slice(0, 120)));
    return;
  }
  if (reply.sessions.length === 0) {
    host.append(h("div", "devices-empty", "没有其他登录记录"));
    return;
  }
  for (const s of reply.sessions) {
    const row = h("div", "device-row");
    const label = s.device_name || s.device_id || "未知设备";
    const title = h("div", "device-name", s.current ? `${label}（本机）` : label);
    const meta = h(
      "div",
      "device-meta",
      [s.ip || "未知 IP", `活跃 ${since(s.last_seen)}`].join(" · "),
    );
    const left = h("div", "device-info");
    left.append(title, meta);
    row.append(left);
    if (!s.current) {
      const kick = h("button", "btn small", "下线") as HTMLButtonElement;
      on(kick, "click", () => {
        kick.disabled = true;
        void api
          .sessionRevoke(s.sid)
          .then(() => drawDevices(host))
          .catch((e) => {
            kick.disabled = false;
            toast(String(e).slice(0, 120));
          });
      });
      row.append(kick);
    }
    host.append(row);
  }
  if (reply.push_devices.length > 0) {
    const push = h("div", "devices-title", "推送令牌");
    host.append(push);
    for (const d of reply.push_devices) {
      host.append(h("div", "device-meta", `${d.platform} · ${d.device_token.slice(0, 12)}…`));
    }
  }
  const others = h("button", "btn danger", "退出其他所有设备") as HTMLButtonElement;
  on(others, "click", () => {
    others.disabled = true;
    void api
      .sessionsRevokeOthers()
      .then((n) => {
        toast(n > 0 ? `已下线 ${n} 台设备` : "没有其他设备");
        return drawDevices(host);
      })
      .catch((e) => {
        others.disabled = false;
        toast(String(e).slice(0, 120));
      });
  });
  host.append(others);
}

// ---------------------------------------------------------------------------
// chat view
// ---------------------------------------------------------------------------

/** 直聊会话 id 形如 `d-<a>~<b>`；返回"对方"的 id（群聊返回 null）。 */
function directPeer(convId: string): string | null {
  if (!convId.startsWith("d-")) return null;
  const [a, b] = convId.slice(2).split("~");
  const me = appStore.get(meAtom)?.user_id;
  if (!a || !b || !me) return null;
  return a === me ? b : a;
}

function renderChat(root: El, convId: string, title: string, isGroup: boolean): void {
  clear(root);
  const back = h("button", "icon-btn", "←");
  on(back, "click", () => {
    api.stopSelect();
    appStore.set(viewAtom, { screen: "main", tab: "chats" });
  });
  const head = h("div", "chat-head");
  const headMain = h("div", "chat-head-main");
  headMain.append(h("div", "chat-title", title));
  const typingLine = h("div", "typing-line");
  headMain.append(typingLine);
  const searchBtn = h("button", "icon-btn", "🔍");
  searchBtn.title = "在聊天中搜索";
  on(searchBtn, "click", () => void chatSearchDialog(convId));
  const callBtn = h("button", "icon-btn", "📞");
  callBtn.title = "发起通话";
  on(callBtn, "click", () =>
    appStore.set(viewAtom, { screen: "call", convId, title, isGroup }),
  );
  head.append(back, headMain, callBtn, searchBtn);
  // 直聊才谈得上"对端设备"：群聊没有单一对端身份可验证
  if (!isGroup) {
    const peer = directPeer(convId);
    if (peer) {
      const verifyBtn = h("button", "icon-btn", "🛡");
      verifyBtn.title = "验证对端设备";
      on(verifyBtn, "click", () =>
        appStore.set(viewAtom, { screen: "verify", peerId: peer, title }),
      );
      head.append(verifyBtn);
    }
  }

  const packets = h("div", "chat-body");
  const footer = h("div", "chat-footer");
  const img = h("button", "icon-btn", "🖼");
  const imgInput = h("input") as HTMLInputElement;
  imgInput.type = "file";
  imgInput.accept = "image/*";
  imgInput.hidden = true;
  const red = h("button", "icon-btn", "🧧");
  const mic = h("button", "icon-btn", "🎤");
  mic.title = "按住回车停止：录一段语音条";
  const clip = h("button", "icon-btn", "📎");
  clip.title = "发送文件";
  const fileInput = h("input") as HTMLInputElement;
  fileInput.type = "file";
  fileInput.hidden = true;
  const film = h("button", "icon-btn", "🎞");
  film.title = "发送视频（云端转码 HLS）";
  const videoInput = h("input") as HTMLInputElement;
  videoInput.type = "file";
  videoInput.accept = "video/*";
  videoInput.hidden = true;
  const sticker = h("button", "icon-btn", "😀");
  sticker.title = "贴纸";
  const input = h("input", "chat-input") as HTMLInputElement;
  input.placeholder = "输入消息，回车发送";
  const send = h("button", "btn primary", "发送");
  const lock = h("button", "icon-btn", isGroup ? "" : secretConvs.has(convId) ? "🔒" : "🔓");
  if (!isGroup) {
    lock.title = "秘密聊天开关（端到端加密）";
    on(lock, "click", () => {
      if (secretConvs.has(convId)) {
        secretConvs.delete(convId);
        lock.textContent = "🔓";
        toast("已切换为普通消息");
      } else {
        secretConvs.add(convId);
        lock.textContent = "🔒";
        toast("秘密聊天已开启：本会话消息将加密发送");
      }
    });
  }
  footer.append(img, red, mic, clip, film, sticker, input, send, lock, imgInput, fileInput, videoInput);

  // selection bar (merge forward): replaces nothing, sits above the footer
  const selectBar = h("div", "select-bar");
  selectBar.hidden = true;

  root.append(head, packets, selectBar, footer);

  const me = appStore.get(meAtom);
  const to: string[] = [];
  if (!isGroup && me) {
    // direct chat: recipient id is the other half of the conv id
    const parts = convId.slice(2).split("~");
    to.push(parts.find((p) => p !== me.user_id) ?? "");
  }

  // -- typing indicator (P1): peer names fade out after 5s
  const syncTyping = (): void => {
    const who = appStore.get(typingAtom)[convId];
    typingLine.textContent = who ? "对方正在输入…" : "";
  };
  appStore.sub(typingAtom, syncTyping);
  syncTyping();

  // -- selection mode (merge forward)
  const syncSelectBar = (): void => {
    const sel = appStore.get(selectModeAtom);
    const active = sel !== null && sel.convId === convId;
    selectBar.hidden = !active;
    if (!active) return;
    clear(selectBar);
    const count = h("span", "sel-count", `已选 ${sel.seqs.length} 条`);
    const merge = h("button", "btn tiny primary", "合并转发");
    on(merge, "click", () => {
      const msgs = (appStore.get(chatsAtom)[convId] ?? []).filter((m) => sel.seqs.includes(m.seq));
      if (msgs.length === 0) {
        toast("先勾选要转发的消息");
        return;
      }
      void mergeForwardDialog(convId, msgs);
    });
    const cancel = h("button", "btn tiny", "取消");
    on(cancel, "click", () => api.stopSelect());
    selectBar.append(count, merge, cancel);
  };
  appStore.sub(selectModeAtom, syncSelectBar);
  syncSelectBar();

  // -- message list (windowed render: at most RENDER_WINDOW bubbles in the
  // DOM; older ones load on demand — the "virtualization" contract item)
  let renderWindow = 300;
  const draw = (): void => {
    const sel = appStore.get(selectModeAtom);
    const selecting = sel !== null && sel.convId === convId;
    const messages = appStore.get(chatsAtom)[convId] ?? [];
    clear(packets);
    const start = Math.max(0, messages.length - renderWindow);
    const visible = selecting ? messages : messages.slice(start);
    if (start > 0 && !selecting) {
      const older = h("button", "history-more", `显示更早的 ${start} 条消息`);
      on(older, "click", () => {
        renderWindow += 300;
        draw();
      });
      packets.append(older);
    }
    // P1: paged history — offer the next page while the cursor is set
    const cursor = appStore.get(historyCursorAtom)[convId];
    if (cursor !== null && cursor !== undefined) {
      const more = h("button", "history-more", "加载更早的消息");
      on(more, "click", () => {
        const meNow = appStore.get(meAtom);
        if (!meNow) return;
        void api.loadHistory(convId, meNow.user_id).catch((e) => toast(String(e).slice(0, 100)));
      });
      packets.append(more);
    }
    let grid: El | null = null;
    for (const m of visible) {
      if (selecting) {
        packets.append(selectableRow(m, sel.seqs.includes(m.seq), convId));
        grid = null;
        continue;
      }
      if (m.kind === "image") {
        if (grid === null) {
          grid = h("div", m.mine ? "img-grid mine" : "img-grid");
          packets.append(bubbleWrap(grid, m));
        }
        grid.append(imageCell(m));
        continue;
      }
      grid = null;
      packets.append(renderMsg(m, convId, to, isGroup));
    }
    packets.scrollTop = packets.scrollHeight;
  };
  appStore.sub(chatsAtom, draw);
  appStore.sub(historyCursorAtom, draw);
  draw();

  const doSend = (): void => {
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    const secret = !isGroup && secretConvs.has(convId);
    const p = secret
      ? api.sendSecretText(convId, to, text)
      : api.sendText(convId, to, text);
    p.catch((e) => toast(String(e).slice(0, 120)));
  };
  on(send, "click", doSend);
  on(input, "keydown", (e) => {
    if (e.key === "Enter") doSend();
  });
  on(input, "input", () => {
    if (input.value.trim()) api.typingThrottled(convId, to);
  });
  on(img, "click", () => imgInput.click());
  on(imgInput, "change", () => {
    const file = imgInput.files?.[0];
    if (file) void api.uploadAndSendImage(convId, to, file).catch((e) => toast(String(e).slice(0, 120)));
    imgInput.value = "";
  });
  on(red, "click", () => void redpacketDialog(convId, to));
  on(clip, "click", () => fileInput.click());
  on(fileInput, "change", () => {
    const file = fileInput.files?.[0];
    if (file) {
      toast(`正在发送文件 ${file.name}…`);
      void api.uploadAndSendFile(convId, to, file).catch((e) => toast(String(e).slice(0, 120)));
    }
    fileInput.value = "";
  });
  on(film, "click", () => videoInput.click());
  on(videoInput, "change", () => {
    const file = videoInput.files?.[0];
    if (file) {
      toast("视频上传转码中，稍候片刻…");
      void api
        .uploadVideo(file)
        .then((ingest) => api.sendVideo(convId, to, ingest))
        .then(() => toast("视频已发送"))
        .catch((e) => toast(String(e).slice(0, 140)));
    }
    videoInput.value = "";
  });
  on(sticker, "click", () => stickerDialog(convId, to));
  on(mic, "click", () => void voiceRecordDialog(convId, to, mic));
}

function selectableRow(m: ChatMsg, checked: boolean, convId: string): El {
  const row = h("div", `msg-row select ${m.mine ? "mine" : "theirs"}`);
  const box = h("input") as HTMLInputElement;
  box.type = "checkbox";
  box.checked = checked;
  on(box, "change", () => {
    api.toggleSelect(convId, m.seq);
  });
  const bubble = h("div", `bubble ${m.mine ? "out" : "in"}`);
  bubble.append(h("span", "msg-text", m.text || `[${m.kind}]`));
  row.append(box, bubble);
  return row;
}

function bubbleWrap(inner: El, m: ChatMsg): El {
  const row = h("div", `msg-row ${m.mine ? "mine" : "theirs"}`);
  const bubble = h("div", `bubble ${m.mine ? "out" : "in"}`);
  bubble.append(inner);
  row.append(bubble);
  return row;
}

function renderMsg(m: ChatMsg, convId: string, _to: string[], isGroup: boolean): El {
  if (m.kind === "system") return h("div", "sys-msg", m.text ?? "");
  let inner: El;
  switch (m.kind) {
    case "text":
      inner = h("span", "msg-text", m.text ?? "");
      break;
    case "e2ee":
      inner = h("span", `msg-text ${m.decryptFailed ? "e2ee-bad" : "e2ee-ok"}`);
      inner.textContent = (m.encrypted ? "🔐 " : "") + (m.text ?? "");
      break;
    case "redpacket":
      inner = h("div", "redpacket");
      inner.append(h("div", "rp-head", "🧧 红包"));
      inner.append(h("div", "rp-memo", m.redpacket?.memo || "恭喜发财，大吉大利"));
      inner.append(h("div", "rp-amount", `${((m.redpacket?.amount ?? 0) / 100).toFixed(2)} 元 · ${m.redpacket?.count ?? 0} 个`));
      if (!m.mine) {
        const grab = h("button", "btn tiny primary", "拆红包");
        on(grab, "click", () => {
          void api
            .claimRedpacket(m.redpacket?.redpacket_id ?? "")
            .then((amount) => {
              grab.textContent = `已领 ${(amount / 100).toFixed(2)} 元`;
              grab.classList.add("done");
              void api.loadBalance();
            })
            .catch((e) => toast(String(e).slice(0, 120)));
        });
        inner.append(grab);
      }
      break;
    case "msg.forward": {
      inner = h("div", "fwd-card");
      inner.append(h("div", "fwd-head", "↪️ 转发消息"));
      inner.append(h("div", "fwd-text", m.text ?? String(m.payload?.text ?? "")));
      break;
    }
    case "msg.merge": {
      const items = (m.payload?.items as ChatMsg[] | undefined) ?? [];
      inner = h("div", "fwd-card");
      inner.append(h("div", "fwd-head", `🗂 ${m.payload?.title ?? "聊天记录"}`));
      for (const it of items.slice(0, 4)) {
        inner.append(h("div", "fwd-item", `${String(it.sender).slice(0, 8)}: ${it.text ?? ""}`));
      }
      if (items.length > 4) inner.append(h("div", "fwd-item", `…共 ${items.length} 条`));
      break;
    }
    case "msg.voice": {
      inner = h("div", "voice-card");
      const bars = (m.payload?.waveform as number[] | undefined) ?? new Array(40).fill(10);
      const duration = Number(m.payload?.duration_ms ?? 0);
      const wave = h("div", "voice-wave");
      for (const b of bars) {
        const bar = h("span", "voice-bar");
        bar.style.height = `${Math.max(2, Math.min(100, b))}%`;
        wave.append(bar);
      }
      const label = h("span", "voice-time", `${(duration / 1000).toFixed(1)}″`);
      const asr = m.payload?.asr as string | undefined;
      const play = h("button", "voice-play", "▶");
      on(play, "click", () => {
        if (play.dataset.playing === "1") return;
        play.dataset.playing = "1";
        play.textContent = "…";
        void api
          .fetchMediaDataUrl(m.mediaId ?? "", false)
          .then((url) => {
            const audio = new Audio(url);
            void audio.play();
            audio.onended = () => {
              play.textContent = "▶";
              play.dataset.playing = "";
            };
            // progress sweep across the bars
            audio.ontimeupdate = () => {
              const idx = Math.floor(
                (audio.currentTime * 1000 / Math.max(duration, 1)) * bars.length,
              );
              wave.querySelectorAll(".voice-bar").forEach((el, i) => {
                el.classList.toggle("played", i <= idx);
              });
            };
          })
          .catch(() => {
            play.textContent = "▶";
            play.dataset.playing = "";
            toast("语音加载失败");
          });
      });
      inner.append(play, wave, label);
      inner.append(h("div", "voice-asr", asr ?? "转文字暂不可用（P4 接入）"));
      break;
    }
    case "msg.file": {
      const name = String(m.payload?.name ?? "文件");
      const size = Number(m.payload?.size ?? 0);
      inner = h("div", "file-card");
      inner.append(h("div", "file-icon", "📄"));
      const main = h("div", "file-main");
      main.append(h("div", "file-name", name));
      main.append(h("div", "file-size", formatBytes(size)));
      inner.append(main);
      const dl = h("button", "btn tiny", "下载");
      on(dl, "click", () => {
        void api
          .fetchMediaDataUrl(m.mediaId ?? "", false)
          .then((url) => {
            const a = document.createElement("a");
            a.href = url;
            a.download = name;
            a.click();
          })
          .catch((e) => toast(String(e).slice(0, 100)));
      });
      inner.append(dl);
      break;
    }
    case "msg.sticker": {
      inner = h("div", "sticker-card");
      const cell = h("div", "sticker-img", "…");
      void api
        .fetchMediaDataUrl(m.mediaId ?? "", false)
        .then((url) => {
          const imgEl = document.createElement("img");
          imgEl.src = url;
          imgEl.width = 96;
          imgEl.height = 96;
          cell.replaceChildren(imgEl);
        })
        .catch(() => {
          cell.textContent = "加载失败";
        });
      inner.append(cell);
      break;
    }
    case "msg.video": {
      const duration = Number(m.payload?.duration_ms ?? 0);
      const playUrl = String(m.payload?.play_url ?? "");
      inner = h("div", "video-card");
      const screen = h("div", "video-thumb", "▶ 播放");
      on(screen, "click", () => {
        screen.textContent = "加载中…";
        void api
          .fetchVideoDataUrl(playUrl || `/api/video/${m.payload?.video_id}/play.mp4`)
          .then((url) => {
            const vid = document.createElement("video");
            vid.src = url;
            vid.controls = true;
            vid.width = 280;
            vid.playbackRate = Number(screen.dataset.speed ?? "1");
            screen.replaceChildren(vid);
          })
          .catch(() => {
            screen.textContent = "▶ 播放";
            toast("视频加载失败");
          });
      });
      inner.append(screen);
      const bar = h("div", "video-bar");
      bar.append(h("span", "video-dur", `${Math.round(duration / 1000)}s`));
      const speed = h("select", "video-speed") as HTMLSelectElement;
      for (const s of [0.5, 1.0, 1.5, 2.0]) {
        const opt = document.createElement("option");
        opt.value = String(s);
        opt.textContent = `${s}x`;
        if (s === 1.0) opt.selected = true;
        speed.append(opt);
      }
      on(speed, "change", () => {
        screen.dataset.speed = speed.value;
        const vid = screen.querySelector("video");
        if (vid) vid.playbackRate = Number(speed.value);
      });
      bar.append(speed);
      inner.append(bar);
      break;
    }
    default:
      inner = h("span", "msg-text", `[不支持的消息: ${m.kind}]`);
  }
  const wrap = bubbleWrap(inner, m);
  // own messages carry the read-receipt mark (P1): ✓✓ once read, ✓ sent
  if (m.mine && (m.kind === "text" || m.kind === "e2ee")) {
    const mark = h("span", `read-mark ${m.read ? "read" : ""}`, m.read ? "✓✓" : "✓");
    wrap.append(mark);
  }
  if (!isGroup) {
    on(wrap, "contextmenu", (e) => {
      e.preventDefault();
      showMsgMenu(e as MouseEvent, m, convId);
    });
  }
  return wrap;
}

/** The P1 bubble action menu (right-click / long-press). */
function showMsgMenu(ev: MouseEvent, m: ChatMsg, convId: string): void {
  hideMsgMenu();
  const menu = h("div", "msg-menu");
  const me = appStore.get(meAtom);
  const addItem = (label: string, fn: () => void): void => {
    const it = h("button", "msg-menu-item", label);
    on(it, "click", () => {
      hideMsgMenu();
      fn();
    });
    menu.append(it);
  };
  addItem("复制", () => {
    void navigator.clipboard?.writeText(m.text ?? "").then(
      () => toast("已复制"),
      () => toast("复制失败"),
    );
  });
  if (m.kind === "text" || m.kind === "e2ee" || m.kind === "msg.forward") {
    addItem("收藏", () => {
      void api.favoriteAdd(m, convId).catch((e) => toast(String(e).slice(0, 100)));
    });
    addItem("转发", () => void forwardDialog(m, convId));
  }
  if (m.kind === "image" && m.mediaId) {
    addItem("存为贴纸", () => {
      const pack = JSON.parse(localStorage.getItem("wxlike-stickers") ?? "[]") as {
        media_id: string; pack: string; id: string;
      }[];
      pack.push({ media_id: m.mediaId ?? "", pack: "my", id: `s-${pack.length + 1}` });
      localStorage.setItem("wxlike-stickers", JSON.stringify(pack.slice(-24)));
      toast("已存为贴纸");
    });
  }
  if (m.mine && me && m.msgId) {
    addItem("撤回", () => {
      void api.recallMsg(convId, m.msgId ?? "").catch((e) => toast(String(e).slice(0, 100)));
    });
  }
  addItem("多选", () => api.startSelect(convId));
  menu.style.left = `${Math.min(ev.clientX, window.innerWidth - 150)}px`;
  menu.style.top = `${Math.min(ev.clientY, window.innerHeight - 200)}px`;
  document.body.append(menu);
  // click-away closes it（监听器登记在 menuListeners，hideMsgMenu 统一清理）
  window.setTimeout(() => {
    menuListeners.add(document)("pointerdown", () => hideMsgMenu(), { once: true });
  }, 0);
}

function hideMsgMenu(): void {
  menuListeners.removeAll();
  for (const el of document.querySelectorAll(".msg-menu")) el.remove();
}

async function forwardDialog(m: ChatMsg, fromConv: string): Promise<void> {
  const target = await pickConversation("转发到会话");
  if (!target) return;
  const snapshot = api.snapshotOf(m, fromConv);
  try {
    await api.forwardMessage(target.convId, target.to, snapshot);
    toast(`已转发到 ${target.title}`);
  } catch (e) {
    toast(String(e).slice(0, 100));
  }
}

async function mergeForwardDialog(fromConv: string, msgs: ChatMsg[]): Promise<void> {
  const target = await pickConversation("合并转发到会话");
  if (!target) return;
  const items = msgs.map((m) => api.snapshotOf(m, fromConv));
  try {
    await api.mergeForward(target.convId, target.to, items);
    api.stopSelect();
    toast(`已合并转发 ${items.length} 条到 ${target.title}`);
  } catch (e) {
    toast(String(e).slice(0, 100));
  }
}

interface ConvTarget {
  convId: string;
  title: string;
  to: string[];
}

/** Conversation picker built on prompt() — same style as the other dialogs. */
async function pickConversation(promptTitle: string): Promise<ConvTarget | null> {
  const sessions = appStore.get(sessionsAtom);
  const friends = appStore.get(friendsAtom);
  const me = appStore.get(meAtom);
  if (!me) return null;
  const lines = [
    ...sessions.map((s, i) => `${i + 1}. ${s.title} (${s.convId.slice(0, 12)}…)`),
    ...friends.map((f, i) => `${sessions.length + i + 1}. 好友 ${f.remark || f.friend_id}`),
  ];
  const sel = window.prompt(`${promptTitle} — 输入序号:\n${lines.join("\n")}`);
  if (!sel) return null;
  const idx = parseInt(sel.trim(), 10) - 1;
  if (Number.isNaN(idx) || idx < 0) return null;
  if (idx < sessions.length) {
    const s = sessions[idx];
    const to = s.isGroup ? [] : [s.convId.slice(2).split("~").find((p) => p !== me.user_id) ?? ""];
    return { convId: s.convId, title: s.title, to };
  }
  const f = friends[idx - sessions.length];
  if (!f) return null;
  const convId = `d-${[me.user_id, f.friend_id].sort().join("~")}`;
  return { convId, title: f.remark || f.friend_id, to: [f.friend_id] };
}

/** Cloud search across my conversations (per-conv scoping arrives with the
 * P3 search-service; the endpoint already authorizes per user). */
async function chatSearchDialog(_convId: string): Promise<void> {
  const q = window.prompt("搜索聊天记录（云端）:");
  if (!q) return;
  try {
    await api.searchMessages(q.trim());
    const hits = appStore.get(searchHitsAtom) ?? [];
    if (hits.length === 0) {
      toast("没有匹配的消息");
      return;
    }
    const lines = hits.map(
      (r, i) => `${i + 1}. ${String(r.sender).slice(0, 8)}: ${r.text ?? ""} (${timeShort(r.sent_at_ms)})`,
    );
    toast(`找到 ${hits.length} 条（前 3 条）:\n` + lines.slice(0, 3).join("\n"));
  } catch (e) {
    toast(String(e).slice(0, 100));
  }
}

function imageCell(m: ChatMsg): El {
  const cell = h("button", "img-cell");
  cell.textContent = "…";
  void api
    .fetchMediaDataUrl(m.mediaId ?? "", true)
    .then((url) => {
      const img = document.createElement("img");
      img.src = url;
      img.loading = "lazy";
      on(img, "click", () => void api.fetchMediaDataUrl(m.mediaId ?? "", false).then((full) => window.open(full, "_blank")));
      cell.replaceChildren(img);
    })
    .catch(() => {
      cell.textContent = "加载失败";
    });
  return cell;
}

/** P2 voice note: the 🎤 button starts, clicking again stops; the waveform
 * is derived from the decoded PCM and travels inside the message payload. */
async function voiceRecordDialog(convId: string, to: string[], micBtn: El): Promise<void> {
  if (micBtn.dataset.recording === "1") {
    micBtn.dataset.stopRequested = "1";
    return;
  }
  let recording: Awaited<ReturnType<typeof api.startVoiceRecording>>;
  try {
    recording = await api.startVoiceRecording();
  } catch (e) {
    toast(String(e).slice(0, 120));
    return;
  }
  micBtn.dataset.recording = "1";
  micBtn.textContent = "⏹";
  micBtn.title = "再次点击结束录音（最长 60 秒）";
  toast("录音中…再次点击 ⏹ 结束");
  const watch = window.setInterval(() => {
    if (micBtn.dataset.stopRequested === "1") {
      window.clearInterval(watch);
      micBtn.dataset.stopRequested = "";
    }
  }, 100);
  // the 60 s hard cap inside startVoiceRecording resolves stop() on its own
  window.setTimeout(() => {
    micBtn.dataset.stopRequested = "1";
  }, 60_500);
  try {
    const { blob, durationMs, waveform } = await recording.stop();
    window.clearInterval(watch);
    const mediaId = await api.uploadVoiceBlob(blob);
    await api.sendVoice(convId, to, mediaId, durationMs, waveform);
    toast("语音已发送");
  } catch (e) {
    toast(String(e).slice(0, 120));
  } finally {
    window.clearInterval(watch);
    micBtn.dataset.recording = "";
    micBtn.textContent = "🎤";
    micBtn.title = "录一段语音条";
  }
}

/** P2 sticker panel: the local "my" pack (images saved from chats) plus a
 * starter set of built-in emoji-art stickers. */
function stickerDialog(convId: string, to: string[]): void {
  hideMsgMenu();
  const menu = h("div", "msg-menu sticker-panel");
  const head = h("div", "fwd-head", "贴纸（点图片消息右键可存为贴纸）");
  menu.append(head);
  const grid = h("div", "sticker-grid");
  const builtins = ["(◕‿◕)", "(｡◕‿◕｡)", "٩(◕‿◕)۶", "ʕ•ᴥ•ʔ", "(☞ﾟヮﾟ)☞", "(╯°□°）╯︵ ┻━┻"];
  for (const face of builtins) {
    const b = h("button", "sticker-item", face);
    on(b, "click", () => {
      hideMsgMenu();
      void api
        .sendSticker(convId, to, "", "builtin", encodeURIComponent(face))
        .catch((e) => toast(String(e).slice(0, 100)));
    });
    grid.append(b);
  }
  // user-saved media stickers
  const saved = JSON.parse(localStorage.getItem("wxlike-stickers") ?? "[]") as {
    media_id: string; pack: string; id: string;
  }[];
  for (const s of saved.slice(-12)) {
    const b = h("button", "sticker-item", "…");
    void api
      .fetchMediaDataUrl(s.media_id, true)
      .then((url) => {
        const img = document.createElement("img");
        img.src = url;
        img.width = 48;
        img.height = 48;
        b.replaceChildren(img);
      })
      .catch(() => b.textContent = "?");
    on(b, "click", () => {
      hideMsgMenu();
      void api.sendSticker(convId, to, s.media_id, s.pack, s.id).catch((e) => toast(String(e).slice(0, 100)));
    });
    grid.append(b);
  }
  menu.append(grid);
  menu.style.left = "70px";
  menu.style.bottom = "70px";
  document.body.append(menu);
  window.setTimeout(() => {
    menuListeners.add(document)("pointerdown", () => {
      menuListeners.removeAll();
      if (menu.isConnected) menu.remove();
    }, { once: true });
  }, 0);
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

async function redpacketDialog(convId: string, to: string[]): Promise<void> {
  const amountYuan = window.prompt("红包总金额（元）:", "1.00");
  if (!amountYuan) return;
  const count = window.prompt("红包个数:", "1");
  if (!count) return;
  const amount = Math.round(parseFloat(amountYuan) * 100);
  const n = parseInt(count, 10);
  if (!Number.isFinite(amount) || amount <= 0 || !Number.isInteger(n) || n <= 0) {
    toast("金额或个数不合法");
    return;
  }
  try {
    await api.createRedpacket(convId, to, amount, n, "恭喜发财，大吉大利");
    void api.loadBalance();
  } catch (e) {
    toast(String(e).slice(0, 120));
  }
}

function timeShort(ms: number): string {
  const d = new Date(ms);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  return sameDay
    ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleDateString([], { month: "numeric", day: "numeric" });
}

// ---------------------------------------------------------------------------
// root renderer: swaps between screens on viewAtom changes
// ---------------------------------------------------------------------------

export function renderRoot(root: El): void {
  api.applyTheme();
  const draw = (): void => {
    api.applyTheme();
    const v = appStore.get(viewAtom);
    clear(root);
    hideMsgMenu();
    // 通话页的轮询必须在视图切换时停掉（否则会对已卸载的 DOM 继续刷新）
    stopCallPolling();
    switch (v.screen) {
      case "auth":
        renderAuth(root);
        break;
      case "main":
        renderMain(root);
        break;
      case "chat":
        renderChat(root, v.convId, v.title, v.isGroup);
        break;
      case "call":
        renderCallPage(root, v.convId, v.title, v.isGroup);
        break;
      case "verify":
        renderVerifyPage(root, v.peerId, v.title);
        break;
    }
  };
  appStore.sub(viewAtom, draw);
  draw();
}
