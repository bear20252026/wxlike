/**
 * Command surface: typed invoke wrappers + the actions views call.
 * Every server interaction lands here on its way to Rust.
 */

import { invoke } from "./tauri";
import {
  appStore,
  balanceAtom,
  chatsAtom,
  connectedAtom,
  darkAtom,
  favoritesAtom,
  friendsAtom,
  groupCacheAtom,
  historyCursorAtom,
  meAtom,
  requestsAtom,
  searchHitsAtom,
  selectModeAtom,
  sessionsAtom,
  toast,
  typingAtom,
  viewAtom,
  type ChatMsg,
  type FavoriteRow,
  type Friend,
  type FriendRequest,
  type InboundEvent,
  type Me,
  type Session,
  type View,
} from "./state";

export interface AuthReply {
  user_id: string;
  token: string;
}

export async function setServer(base: string): Promise<void> {
  await invoke("set_server", { base_url: base });
}

export async function register(name: string, password: string): Promise<AuthReply> {
  return invoke<AuthReply>("register", { name, password });
}

export async function login(name: string, password: string): Promise<AuthReply> {
  return invoke<AuthReply>("login", { name, password });
}

export async function fetchMe(): Promise<Me> {
  const me = await invoke<Me>("me");
  appStore.set(meAtom, me);
  return me;
}

export async function connectHub(addr: string): Promise<void> {
  await invoke("connect_hub", { addr });
  appStore.set(connectedAtom, true);
  await invoke("start_recv_pump");
}

export async function invokeUpdateProfile(signature: string): Promise<void> {
  await invoke("update_profile", { name: null, avatar: null, signature });
  await fetchMe();
}

export async function loadFriends(): Promise<void> {
  const rows = await invoke<Friend[]>("friends");
  appStore.set(friendsAtom, rows);
}

export async function loadRequests(): Promise<void> {
  const rows = await invoke<FriendRequest[]>("friend_requests_incoming");
  appStore.set(requestsAtom, rows);
}

export async function loadBalance(): Promise<void> {
  const balance = await invoke<number>("wallet_balance");
  appStore.set(balanceAtom, balance);
}

export async function acceptRequest(reqId: string): Promise<void> {
  await invoke("friend_accept", { req_id: reqId });
  await Promise.all([loadFriends(), loadRequests()]);
}

export async function declineRequest(reqId: string): Promise<void> {
  await invoke("friend_decline", { req_id: reqId });
  await loadRequests();
}

export async function sendFriendRequest(toUserId: string, message: string): Promise<void> {
  await invoke("friend_request", { to_user_id: toUserId, message });
}

export async function sendText(convId: string, to: string[], text: string): Promise<void> {
  await invoke("send_text", { conv_id: convId, to, text });
}

export async function uploadAndSendImage(convId: string, to: string[], file: File): Promise<void> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const uploaded = await invoke<{ media_id: string }>("upload_media", {
    bytes: Array.from(bytes),
    content_type: file.type || "application/octet-stream",
    sha256: null,
  });
  await invoke("send_image", { conv_id: convId, to, media_id: uploaded.media_id });
}

export async function fetchMediaDataUrl(mediaId: string, thumb: boolean): Promise<string> {
  const blob = await invoke<{ base64: string; content_type: string }>("download_media", {
    media_id: mediaId,
    thumb,
  });
  return `data:${blob.content_type};base64,${blob.base64}`;
}

export async function createRedpacket(
  convId: string,
  to: string[],
  amount: number,
  count: number,
  memo: string,
): Promise<void> {
  const created = await invoke<{ redpacket_id: string }>("wallet_redpacket_cmd", {
    amount, count, memo,
  });
  await invoke("send_redpacket_msg", {
    conv_id: convId, to, redpacket_id: created.redpacket_id, amount, count, memo,
  });
}

export async function claimRedpacket(redpacketId: string): Promise<number> {
  return invoke<number>("wallet_claim", { redpacket_id: redpacketId });
}

export async function createGroup(name: string, memberIds: string[]): Promise<string> {
  return invoke<string>("groups_create", { name, member_ids: memberIds });
}

export function view(): View {
  return appStore.get(viewAtom);
}

export function go(view: View): void {
  appStore.set(viewAtom, view);
}

// ---------------------------------------------------------------------------
// P1: message operations (typing / forward / merge / favorites / search)
// ---------------------------------------------------------------------------

let lastTypingSentAt = 0;

/** Fires at most one typing ping every 2.5 s while the user keeps typing. */
export function typingThrottled(convId: string, to: string[]): void {
  const now = Date.now();
  if (now - lastTypingSentAt < 2500) return;
  lastTypingSentAt = now;
  void invoke("send_typing", { conv_id: convId, to }).catch(() => {
    /* transient signal: failures are ignorable by design */
  });
}

export async function forwardMessage(
  toConv: string,
  to: string[],
  item: Record<string, unknown>,
): Promise<void> {
  await invoke("forward_message", { to_conv: toConv, to, item });
}

/** Recalls one of my own messages (hub enforces authorship). */
export async function recallMsg(convId: string, msgId: string): Promise<void> {
  await invoke("recall_msg", { conv_id: convId, target: msgId });
}

/** Read receipt for everything up to `msgId` (P1): the author's UI flips
 * the bubble to ✓✓. Fire-and-forget — a lost receipt is cosmetic. */
export function sendReadReceipt(convId: string, msgId: string): void {
  if (!msgId) return;
  void invoke("send_read_receipt", { conv_id: convId, up_to: msgId }).catch(() => undefined);
}

export async function mergeForward(
  toConv: string,
  to: string[],
  items: Record<string, unknown>[],
): Promise<void> {
  await invoke("merge_forward", { to_conv: toConv, to, items });
}

/** The snapshot shape both favorites and forwards carry. */
export function snapshotOf(m: ChatMsg, convId: string): Record<string, unknown> {
  return {
    origin_conv: convId,
    origin_msg_id: m.msgId ?? "",
    kind: m.kind === "e2ee" ? "text" : m.kind,
    text: m.text ?? "",
    media_id: m.mediaId ?? "",
    sender: m.sender,
    sent_at_ms: m.sentAtMs,
  };
}

export async function favoriteAdd(m: ChatMsg, convId: string): Promise<void> {
  await invoke("favorite_add", { item: snapshotOf(m, convId) });
  toast("已收藏");
}

export async function favoritesLoad(): Promise<void> {
  const reply = await invoke<{ favorites: FavoriteRow[] }>("favorites_list", { limit: 50 });
  appStore.set(favoritesAtom, reply.favorites ?? []);
}

export async function favoriteRemove(id: string): Promise<void> {
  await invoke("favorite_remove", { id });
  await favoritesLoad();
}

export async function searchMessages(q: string): Promise<void> {
  const reply = await invoke<{ results: Record<string, unknown>[] }>("search_messages", {
    q,
    limit: 30,
  });
  appStore.set(searchHitsAtom, (reply.results ?? []) as never);
}

export function closeSearch(): void {
  appStore.set(searchHitsAtom, null);
}

// ---------------------------------------------------------------------------
// P1: server history (cloud sync)
// ---------------------------------------------------------------------------

interface HistoryRow {
  conv_id: string;
  seq: number;
  sender: string;
  kind: string;
  text?: string;
  media_id?: string;
  msg_id?: string;
  quote_of?: string | null;
  sent_at_ms: number;
  withdrawn?: boolean;
  payload?: Record<string, unknown>;
}

function rowToMsg(row: HistoryRow, meId: string): ChatMsg {
  return {
    seq: row.seq,
    sender: row.sender,
    kind: row.withdrawn ? "system" : row.kind,
    text: row.withdrawn ? "消息已撤回" : row.text,
    mediaId: row.media_id,
    payload: row.payload,
    msgId: row.msg_id,
    quoteOf: row.quote_of ?? null,
    sentAtMs: row.sent_at_ms,
    mine: row.sender === meId,
  };
}

function mergeMessages(existing: ChatMsg[], incoming: ChatMsg[]): ChatMsg[] {
  const byKey = new Map<string, ChatMsg>();
  for (const m of existing) byKey.set(m.msgId || `s:${m.seq}`, m);
  for (const m of incoming) byKey.set(m.msgId || `s:${m.seq}`, m);
  return [...byKey.values()].sort((a, b) => a.seq - b.seq);
}

/** Loads one page of history and prepends it; returns false when exhausted. */
export async function loadHistory(convId: string, meId: string, limit = 30): Promise<boolean> {
  const cursors = appStore.get(historyCursorAtom);
  if (convId in cursors && cursors[convId] === null) return false;
  const before = cursors[convId] ?? 0;
  const reply = await invoke<{ messages: HistoryRow[]; next_before_seq: number | null }>(
    "load_history",
    { conv_id: convId, before_seq: before, limit },
  );
  const mapped = (reply.messages ?? []).map((r) => rowToMsg(r, meId));
  const chats = { ...appStore.get(chatsAtom) };
  chats[convId] = mergeMessages(chats[convId] ?? [], mapped);
  appStore.set(chatsAtom, chats);
  appStore.set(historyCursorAtom, { ...cursors, [convId]: reply.next_before_seq ?? null });
  return reply.next_before_seq != null;
}

// ---------------------------------------------------------------------------
// P1: E2EE secret chat
// ---------------------------------------------------------------------------

export async function secretEnable(): Promise<string> {
  const fingerprint = await invoke<string>("secret_enable");
  toast("秘密聊天已启用（本会话）");
  return fingerprint;
}

export async function secretEnabled(): Promise<boolean> {
  return invoke<boolean>("secret_enabled");
}

export async function sendSecretText(
  convId: string,
  to: string[],
  text: string,
): Promise<void> {
  await invoke("send_secret_text", { conv_id: convId, to, text });
}

// ---------------------------------------------------------------------------
// selection mode (merge forward)
// ---------------------------------------------------------------------------

export function toggleSelect(convId: string, seq: number): void {
  const cur = appStore.get(selectModeAtom);
  if (!cur || cur.convId !== convId) return;
  const seqs = cur.seqs.includes(seq)
    ? cur.seqs.filter((s) => s !== seq)
    : [...cur.seqs, seq];
  appStore.set(selectModeAtom, { convId, seqs });
}

export function startSelect(convId: string): void {
  appStore.set(selectModeAtom, { convId, seqs: [] });
}

export function stopSelect(): void {
  appStore.set(selectModeAtom, null);
}

// ---------------------------------------------------------------------------
// inbound events from the hub pump
// ---------------------------------------------------------------------------

function upsertSession(
  convId: string,
  title: string,
  isGroup: boolean,
  lastText: string,
  at: number,
  unreadDelta: number,
): void {
  const sessions = appStore.get(sessionsAtom).slice();
  const existing = sessions.findIndex((s) => s.convId === convId);
  if (existing >= 0) {
    const s = sessions[existing];
    sessions.splice(existing, 1);
    sessions.unshift({ ...s, lastText, lastAtMs: at, unread: s.unread + unreadDelta });
  } else {
    sessions.unshift({ convId, title, isGroup, lastText, lastAtMs: at, unread: unreadDelta });
  }
  appStore.set(sessionsAtom, sessions);
}

function appendChat(convId: string, msg: ChatMsg): void {
  const chats = { ...appStore.get(chatsAtom) };
  const list = (chats[convId] ?? []).slice();
  const key = msg.msgId || `s:${msg.seq}`;
  if (list.some((m) => (m.msgId || `s:${m.seq}`) === key)) return; // dedupe live echo
  list.push(msg);
  chats[convId] = list;
  appStore.set(chatsAtom, chats);
}

const typingTimers: Record<string, number> = {};

function noteTyping(convId: string, who: string): void {
  const cur = { ...appStore.get(typingAtom), [convId]: who };
  appStore.set(typingAtom, cur);
  window.clearTimeout(typingTimers[convId]);
  typingTimers[convId] = window.setTimeout(() => {
    const next = { ...appStore.get(typingAtom) };
    delete next[convId];
    appStore.set(typingAtom, next);
  }, 5000);
}

/** Marks my outgoing message read when a read receipt names it (P1). */
function markRead(convId: string, targetMsgId: string): void {
  if (!targetMsgId) return;
  const chats = { ...appStore.get(chatsAtom) };
  const list = chats[convId];
  if (!list) return;
  let changed = false;
  chats[convId] = list.map((m) => {
    if (m.msgId === targetMsgId && !m.read) {
      changed = true;
      return { ...m, read: true };
    }
    // a read receipt covers everything up to that message in practice; mark
    // earlier own messages read too (WeChat shows one marker per message)
    return m;
  });
  if (changed) appStore.set(chatsAtom, chats);
}

export function onInbound(payload: unknown): void {
  const ev = payload as InboundEvent;
  const me = appStore.get(meAtom);
  if (!me) return;
  const mine = ev.sender === me.user_id;

  if (ev.kind === "typing") {
    if (!mine) noteTyping(ev.conv_id, ev.sender);
    return;
  }
  if (ev.kind === "receipt" && ev.receipt) {
    if (ev.receipt.state === "read") markRead(ev.conv_id, ev.receipt.target);
    return;
  }
  if (ev.kind === "recall") {
    // the recalled bubble becomes a placeholder; the original text is gone
    const chats = { ...appStore.get(chatsAtom) };
    const list = chats[ev.conv_id];
    if (list && ev.target) {
      chats[ev.conv_id] = list.map((m) =>
        m.msgId === ev.target ? { ...m, kind: "system" as const, text: "消息已撤回" } : m,
      );
      appStore.set(chatsAtom, chats);
    }
    appendChat(ev.conv_id, {
      seq: ev.seq,
      sender: ev.sender,
      kind: "system",
      text: mine ? "你撤回了一条消息" : "对方撤回了一条消息",
      sentAtMs: ev.sent_at_ms,
      mine,
      msgId: ev.msg_id,
    });
  } else {
    const msg: ChatMsg = {
      seq: ev.seq,
      sender: ev.sender,
      kind: ev.kind,
      text: ev.text,
      mediaId: ev.media_id,
      redpacket:
        ev.kind === "wallet.redpacket"
          ? {
              redpacket_id: String(ev.payload?.redpacket_id ?? ""),
              amount: Number(ev.payload?.amount ?? 0),
              count: Number(ev.payload?.count ?? 0),
              memo: String(ev.payload?.memo ?? ""),
            }
          : undefined,
      payload: ev.payload,
      msgId: ev.msg_id,
      quoteOf: ev.quote_of ?? null,
      sentAtMs: ev.sent_at_ms,
      mine,
      encrypted: ev.encrypted,
      decryptFailed: ev.decrypt_failed,
    };
    appendChat(ev.conv_id, msg);
    // P1 read receipt: a message seen while its chat is open counts as read
    if (!mine && ev.msgId) {
      const current = appStore.get(viewAtom);
      const isCurrent = current.screen === "chat" && current.convId === ev.conv_id;
      if (isCurrent) sendReadReceipt(ev.conv_id, ev.msgId);
    }
  }

  const current = appStore.get(viewAtom);
  const isCurrent = current.screen === "chat" && current.convId === ev.conv_id;
  const preview = previewOf(ev);
  const friend = appStore.get(friendsAtom).find((f) => f.friend_id === ev.sender);
  const group = appStore.get(groupCacheAtom)[ev.conv_id];
  const isGroup = ev.conv_id in appStore.get(groupCacheAtom);
  const title = (friend?.remark || friend?.friend_id || ev.sender) as string;
  upsertSession(
    ev.conv_id,
    isGroup ? group?.name ?? ev.conv_id : title,
    isGroup,
    preview,
    ev.sent_at_ms,
    isCurrent || mine ? 0 : 1,
  );
}

function previewOf(ev: InboundEvent): string {
  switch (ev.kind) {
    case "image":
      return "[图片]";
    case "wallet.redpacket":
      return "[红包]";
    case "msg.forward":
      return "[转发] " + (ev.text ?? "");
    case "msg.merge":
      return "[合并转发]";
    case "e2ee":
      return ev.decrypt_failed ? "[加密消息]" : "[密] " + (ev.text ?? "");
    case "typing":
    case "receipt":
      return "";
    default:
      return ev.text ?? "";
  }
}

export function toggleDark(): void {
  const next = !appStore.get(darkAtom);
  appStore.set(darkAtom, next);
  localStorage.setItem("wxlike-dark", next ? "1" : "0");
}

export function applyTheme(): void {
  document.documentElement.dataset.theme = appStore.get(darkAtom) ? "dark" : "light";
}

export function notifyLost(): void {
  appStore.set(connectedAtom, false);
  toast("与服务器断开，请重连");
}

/** Marks a conversation read when opened (and requests its history page). */
export function openSession(session: Session): void {
  const sessions = appStore.get(sessionsAtom).map((s) =>
    s.convId === session.convId ? { ...s, unread: 0 } : s,
  );
  appStore.set(sessionsAtom, sessions);
  const me = appStore.get(meAtom);
  if (me) {
    void loadHistory(session.convId, me.user_id)
      .then(() => {
        // reading the open conversation also acks it (P1 receipts)
        const list = appStore.get(chatsAtom)[session.convId] ?? [];
        const last = [...list].reverse().find((m) => !m.mine && m.msgId);
        if (last?.msgId) sendReadReceipt(session.convId, last.msgId);
      })
      .catch(() => undefined);
  }
  go({ screen: "chat", convId: session.convId, title: session.title, isGroup: session.isGroup });
}
