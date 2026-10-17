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
  friendsAtom,
  meAtom,
  requestsAtom,
  sessionsAtom,
  toast,
  viewAtom,
  type FriendRequest,
  type ChatMsg,
  type Friend,
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
// inbound events from the hub pump
// ---------------------------------------------------------------------------

function upsertSession(convId: string, title: string, isGroup: boolean, lastText: string, at: number, unreadDelta: number): void {
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
  list.push(msg);
  chats[convId] = list;
  appStore.set(chatsAtom, chats);
}

export function onInbound(payload: unknown): void {
  const ev = payload as InboundEvent;
  const me = appStore.get(meAtom);
  if (!me) return;
  const mine = ev.sender === me.user_id;
  const msg: ChatMsg = {
    seq: ev.seq,
    sender: ev.sender,
    kind: ev.kind,
    text: ev.text,
    mediaId: ev.media_id,
    redpacket:
      ev.kind === "redpacket"
        ? {
            redpacket_id: ev.payload?.redpacket_id ?? "",
            amount: ev.payload?.amount ?? 0,
            count: ev.payload?.count ?? 0,
            memo: ev.payload?.memo ?? "",
          }
        : undefined,
    sentAtMs: ev.sent_at_ms,
    mine,
  };
  appendChat(ev.conv_id, msg);
  const current = appStore.get(viewAtom);
  const isCurrent = current.screen === "chat" && current.convId === ev.conv_id;
  const preview =
    ev.kind === "image" ? "[图片]" : ev.kind === "redpacket" ? "[红包]" : (ev.text ?? "");
  const friend = appStore.get(friendsAtom).find((f) => f.friend_id === ev.sender);
  const title = ev.conv_id.startsWith("g-") ? ev.conv_id : (friend?.remark || friend?.friend_id || ev.sender);
  upsertSession(ev.conv_id, title, ev.conv_id.startsWith("g-"), preview, ev.sent_at_ms, isCurrent || mine ? 0 : 1);
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

/** Marks a conversation read when opened. */
export function openSession(session: Session): void {
  const sessions = appStore.get(sessionsAtom).map((s) =>
    s.convId === session.convId ? { ...s, unread: 0 } : s,
  );
  appStore.set(sessionsAtom, sessions);
  go({ screen: "chat", convId: session.convId, title: session.title, isGroup: session.isGroup });
}
