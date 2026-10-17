/**
 * Rendering: views subscribe to the atoms they read; interactions dispatch
 * through `api.ts`. HuLa-inspired bits from the task book's checklist live
 * here: swipeable session rows, expanding search, distinct bubbles, image
 * grid, chat transition, and a full dark-mode theme (see styles.css).
 */

import { appStore, toast, type ChatMsg, type Session } from "./state";
import {
  balanceAtom,
  chatsAtom,
  connectedAtom,
  friendsAtom,
  meAtom,
  requestsAtom,
  searchOpenAtom,
  sessionsAtom,
  viewAtom,
} from "./state";
import * as api from "./api";

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
// main shell (sessions / friends / me)
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
  for (const [key, label] of [["chats", "聊天"], ["friends", "通讯录"], ["me", "我"]] as const) {
    const b = h("button", `tab ${tab === key ? "active" : ""}`, label);
    b.dataset.tab = key;
    on(b, "click", () => appStore.set(viewAtom, { screen: "main", tab: key }));
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
    else drawMe(list);
  };

  for (const atom of [sessionsAtom, friendsAtom, requestsAtom, viewAtom, balanceAtom, meAtom, connectedAtom]) {
    appStore.sub(atom, () => {
      drawList();
      syncTabHighlight();
    });
  }
  on(searchInput, "input", drawList);
  on(themeBtn, "click", () => api.toggleDark());

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

function drawMe(list: El): void {
  const me = appStore.get(meAtom);
  const balance = appStore.get(balanceAtom);
  if (!me) return;
  const card = h("div", "me-card");
  card.append(h("div", "face big", me.name.slice(0, 1).toUpperCase()));
  card.append(h("div", "me-name", me.name));
  card.append(h("div", "me-id", me.user_id));
  card.append(h("div", "me-sign", me.signature || "这个人很懒，什么都没写"));
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
  list.append(card, wallet, sig, save);
}

// ---------------------------------------------------------------------------
// chat view
// ---------------------------------------------------------------------------

function renderChat(root: El, convId: string, title: string, isGroup: boolean): void {
  clear(root);
  const back = h("button", "icon-btn", "←");
  on(back, "click", () => appStore.set(viewAtom, { screen: "main", tab: "chats" }));
  const head = h("div", "chat-head");
  head.append(back, h("div", "chat-title", title));

  const packets = h("div", "chat-body");
  const footer = h("div", "chat-footer");
  const img = h("button", "icon-btn", "🖼");
  const imgInput = h("input") as HTMLInputElement;
  imgInput.type = "file";
  imgInput.accept = "image/*";
  imgInput.hidden = true;
  const red = h("button", "icon-btn", "🧧");
  const input = h("input", "chat-input") as HTMLInputElement;
  input.placeholder = "输入消息，回车发送";
  const send = h("button", "btn primary", "发送");
  footer.append(img, red, input, send, imgInput);

  root.append(head, packets, footer);

  const me = appStore.get(meAtom);
  const to: string[] = [];
  if (!isGroup && me) {
    // direct chat: recipient id is the other half of the conv id
    const parts = convId.slice(2).split("~");
    to.push(parts.find((p) => p !== me.user_id) ?? "");
  }

  // 图片九宫格: consecutive image messages lay out in a grid
  const draw = (): void => {
    const messages = appStore.get(chatsAtom)[convId] ?? [];
    clear(packets);
    let grid: El | null = null;
    for (const m of messages) {
      if (m.kind === "image") {
        if (grid === null) {
          grid = h("div", m.mine ? "img-grid mine" : "img-grid");
          packets.append(bubbleWrap(grid, m));
        }
        grid.append(imageCell(m));
        continue;
      }
      grid = null;
      packets.append(renderMsg(m));
    }
    packets.scrollTop = packets.scrollHeight;
  };
  appStore.sub(chatsAtom, draw);
  draw();

  const doSend = (): void => {
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    void api.sendText(convId, to, text).catch((e) => toast(String(e).slice(0, 120)));
  };
  on(send, "click", doSend);
  on(input, "keydown", (e) => {
    if (e.key === "Enter") doSend();
  });
  on(img, "click", () => imgInput.click());
  on(imgInput, "change", () => {
    const file = imgInput.files?.[0];
    if (file) void api.uploadAndSendImage(convId, to, file).catch((e) => toast(String(e).slice(0, 120)));
    imgInput.value = "";
  });
  on(red, "click", () => void redpacketDialog(convId, to));
}

function bubbleWrap(inner: El, m: ChatMsg): El {
  const row = h("div", `msg-row ${m.mine ? "mine" : "theirs"}`);
  const bubble = h("div", `bubble ${m.mine ? "out" : "in"}`);
  bubble.append(inner);
  row.append(bubble);
  return row;
}

function renderMsg(m: ChatMsg): El {
  if (m.kind === "system") return h("div", "sys-msg", m.text ?? "");
  let inner: El;
  switch (m.kind) {
    case "text":
      inner = h("span", "msg-text", m.text ?? "");
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
    default:
      inner = h("span", "msg-text", "[不支持的消息]");
  }
  return bubbleWrap(inner, m);
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
    }
  };
  appStore.sub(viewAtom, draw);
  draw();
}
