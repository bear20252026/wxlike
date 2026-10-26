/**
 * Command surface: typed invoke wrappers + the actions views call.
 * Every server interaction lands here on its way to Rust.
 */

import { MessageIndex } from "./msgIndex";
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
    // 归属会话：附件只有会话成员能下载（issue #14）
    conv_id: convId,
    public: false,
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

const lastTypingSentAt: Record<string, number> = {};

/**
 * Fires at most one typing ping every 2.5 s while the user keeps typing —
 * **per conversation**: a shared timestamp would let two open chats starve
 * each other's indicator (every ping from chat A resets the clock for B).
 */
export function typingThrottled(convId: string, to: string[]): void {
  const now = Date.now();
  if (now - (lastTypingSentAt[convId] ?? 0) < 2500) return;
  lastTypingSentAt[convId] = now;
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

/** P4 设备管理：登录会话（可远程下线）+ 推送令牌。 */
export interface SessionRow {
  sid: string;
  device_id: string;
  device_name: string;
  ip: string;
  created_at: number;
  last_seen: number;
  current: boolean;
}

export interface PushDeviceRow {
  platform: string;
  device_token: string;
}

export interface SessionsReply {
  sessions: SessionRow[];
  push_devices: PushDeviceRow[];
}

export async function sessionsList(): Promise<SessionsReply> {
  const reply = await invoke<SessionsReply>("sessions_list");
  return { sessions: reply.sessions ?? [], push_devices: reply.push_devices ?? [] };
}

export async function sessionRevoke(sid: string): Promise<void> {
  await invoke("session_revoke", { sid });
}

export async function sessionsRevokeOthers(): Promise<number> {
  const reply = await invoke<{ revoked: number }>("sessions_revoke_others");
  return reply.revoked ?? 0;
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
// P2: rich messages (voice / file / sticker / video)
// ---------------------------------------------------------------------------

export async function sendVoice(
  convId: string,
  to: string[],
  mediaId: string,
  durationMs: number,
  waveform: number[],
): Promise<void> {
  await invoke("send_voice", { conv_id: convId, to, media_id: mediaId, duration_ms: durationMs, waveform });
}

export async function uploadAndSendFile(convId: string, to: string[], file: File): Promise<void> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const up = await invoke<{ media_id: string }>("upload_media", {
    bytes: Array.from(bytes),
    content_type: file.type || "application/octet-stream",
    sha256: null,
    conv_id: convId,
    public: false,
  });
  await invoke("send_file", {
    conv_id: convId, to, media_id: up.media_id,
    name: file.name, size: bytes.length, sha256: "",
  });
}

export async function sendSticker(
  convId: string,
  to: string[],
  mediaId: string,
  pack: string,
  id: string,
): Promise<void> {
  await invoke("send_sticker", { conv_id: convId, to, media_id: mediaId, pack, id });
}

/** Uploads to video-service and returns the ingest reply (ids + urls). */
export async function uploadVideo(file: File): Promise<{
  video_id: string; playlist_url: string; play_url: string; thumb_url: string;
  duration_ms: number; width: number; height: number;
}> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  return invoke("upload_video", { bytes: Array.from(bytes) });
}

export async function sendVideo(
  convId: string,
  to: string[],
  ingest: Awaited<ReturnType<typeof uploadVideo>>,
): Promise<void> {
  await invoke("send_video", {
    conv_id: convId, to,
    payload: {
      video_id: ingest.video_id,
      playlist_url: ingest.playlist_url,
      play_url: ingest.play_url,
      thumb_media_id: "", // P2 thumb lives in video-service; media-id thumb arrives with P5
      duration_ms: ingest.duration_ms,
      speeds: [0.5, 1.0, 1.5, 2.0],
    },
  });
}

export async function fetchVideoDataUrl(path: string): Promise<string> {
  const blob = await invoke<{ base64: string; content_type: string }>("download_video", { path });
  return `data:${blob.content_type};base64,${blob.base64}`;
}

/** Starts a voice recording; `stop()` finishes it and derives duration +
 * display waveform from the decoded PCM. Mic permission refused / API missing
 * rejects with a user-readable message; the hard cap is 60 s. */
export async function startVoiceRecording(): Promise<{
  stop: () => Promise<{ blob: Blob; durationMs: number; waveform: number[] }>;
}> {
  if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
    throw new Error("此环境不支持录音");
  }
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const chunks: Blob[] = [];
  const rec = new MediaRecorder(stream);
  rec.ondataavailable = (e) => chunks.push(e.data);
  rec.start();
  const started = Date.now();
  let stopped = false;

  const finish = async (): Promise<{ blob: Blob; durationMs: number; waveform: number[] }> => {
    if (!stopped) {
      stopped = true;
      if (rec.state !== "inactive") rec.stop();
      await new Promise<void>((resolve) => {
        rec.onstop = () => resolve();
      });
      for (const t of stream.getTracks()) t.stop();
    }
    const blob = new Blob(chunks, { type: rec.mimeType || "audio/webm" });
    const durationMs = Math.max(200, Date.now() - started);
    let waveform: number[];
    try {
      const ctx = new AudioContext();
      const audio = await ctx.decodeAudioData(await blob.arrayBuffer());
      const data = audio.getChannelData(0);
      const bars = 40;
      const step = Math.max(1, Math.floor(data.length / bars));
      const peaks: number[] = [];
      for (let b = 0; b < bars; b++) {
        let peak = 0;
        for (let i = b * step; i < Math.min((b + 1) * step, data.length); i++) {
          const v = Math.abs(data[i]);
          if (v > peak) peak = v;
        }
        peaks.push(peak);
      }
      const max = Math.max(...peaks, 0.01);
      waveform = peaks.map((p) => Math.max(1, Math.round((p / max) * 100)));
      void ctx.close();
    } catch {
      // decode unsupported in this webview: flat-but-valid bars, duration is real
      waveform = new Array(40).fill(8);
    }
    return { blob, durationMs, waveform };
  };

  // hard cap: 60 s, then it stops itself
  window.setTimeout(() => {
    if (!stopped) void finish();
  }, 60_000);

  return { stop: finish };
}

export async function uploadVoiceBlob(convId: string, blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  const up = await invoke<{ media_id: string }>("upload_media", {
    bytes: Array.from(bytes),
    content_type: blob.type || "audio/webm",
    sha256: null,
    conv_id: convId,
    public: false,
  });
  return up.media_id;
}

// ---------------------------------------------------------------------------
// P2: local message library
// ---------------------------------------------------------------------------

export async function openLocalDb(path: string): Promise<void> {
  await invoke("local_open", { path });
}

export async function localReady(): Promise<boolean> {
  return invoke<boolean>("local_ready");
}

/** Loads one page from the on-disk library into the chat store. */
export async function loadLocalHistory(convId: string, meId: string, limit = 50): Promise<boolean> {
  const reply = await invoke<{ messages: HistoryRow[]; next_before_seq: number | null }>(
    "local_history",
    { conv_id: convId, before_seq: 0, limit },
  );
  const mapped = (reply.messages ?? []).map((r) => rowToMsg(r, meId));
  if (mapped.length > 0) {
    const chats = { ...appStore.get(chatsAtom) };
    chats[convId] = mergeMessages(convId, chats[convId] ?? [], mapped);
    appStore.set(chatsAtom, chats);
  }
  return reply.next_before_seq != null;
}

// ---------------------------------------------------------------------------
// P2: avatar
// ---------------------------------------------------------------------------

export async function uploadAvatar(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  return invoke<string>("upload_avatar", {
    bytes: Array.from(bytes),
    content_type: file.type || "image/png",
  });
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

/**
 * 按会话缓存的 `MessageIndex`（tweb `SlicedArray` 支撑）。
 *
 * 手写版是 `Map + sort`（每页 O(n log n)）；现在分页合并交给 tweb 的切片
 * 插入，同 seq 覆盖、跨页去重都由它完成。索引长度与原子数组对不上时重建；
 * 长度一致但**内容漂移**（已读/撤回等局部变异替换了数组里的对象）时用
 * `refresh` 按对象身份修正，防止下一次 merge 复活变异前的旧对象。
 */
const msgIndexes = new Map<string, MessageIndex>();

function mergeMessages(convId: string, existing: ChatMsg[], incoming: ChatMsg[]): ChatMsg[] {
  let index = msgIndexes.get(convId);
  if (!index || index.size !== existing.length) {
    index = MessageIndex.from(existing);
    msgIndexes.set(convId, index);
  } else {
    index.refresh(existing);
  }
  return index.merge(incoming);
}

/** 退出登录/切换账号时清空索引缓存。 */
export function resetMessageIndexes(): void {
  msgIndexes.clear();
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
  chats[convId] = mergeMessages(convId, chats[convId] ?? [], mapped);
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

/** 会话级秘密聊天开关 + 消失消息 TTL（服务端是唯一真相）。 */
export interface SecretState {
  conv_id: string;
  enabled: boolean;
  ttl_ms: number;
}

export async function secretConfigure(
  convId: string,
  enabled: boolean,
  ttlMs = 0,
): Promise<SecretState> {
  return invoke<SecretState>("secret_configure", {
    conv_id: convId,
    enabled,
    ttl_ms: ttlMs,
  });
}

export async function secretState(convId: string): Promise<SecretState> {
  return invoke<SecretState>("secret_state", { conv_id: convId });
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
  // 本地增量只负责"立刻有反馈"；真正的未读数以服务端信箱为准。
  // 校正改为**每会话 2 s 尾随去抖**：离线期间补一批消息时，不再一条消息
  // 一个 HTTP 请求，而是等流停了拉一次最终值（离线收到的、别的设备读过
  // 的都能对上）。
  scheduleUnreadCorrection(convId);
}

/** 每会话一个尾随定时器：窗口内的新消息只触发最后一次校正。 */
const unreadCorrectionTimers = new Map<string, number>();

function scheduleUnreadCorrection(convId: string): void {
  const prev = unreadCorrectionTimers.get(convId);
  if (prev !== undefined) window.clearTimeout(prev);
  unreadCorrectionTimers.set(
    convId,
    window.setTimeout(() => {
      unreadCorrectionTimers.delete(convId);
      fetchConvUnread(convId)
        .then((u) => {
          const cur = appStore.get(sessionsAtom);
          appStore.set(
            sessionsAtom,
            cur.map((s) => (s.convId === convId ? { ...s, unread: u.unread } : s)),
          );
        })
        .catch(() => undefined);
    }, 2000),
  );
}

/** 服务端未读（信箱是唯一真相；本地增量只用于即时反馈）。 */
export async function fetchConvUnread(
  convId: string,
): Promise<{ unread: number; head_seq: number; read_seq: number }> {
  return invoke("conv_unread", { conv: convId });
}

/** 打开会话即 ack：把游标推到最新（区别于本地 markRead(conv,msgId)）。 */
export async function ackConv(convId: string): Promise<void> {
  await invoke("conv_mark_read", { conv: convId });
}

function appendChat(convId: string, msg: ChatMsg): void {
  const chats = { ...appStore.get(chatsAtom) };
  const existing = chats[convId] ?? [];
  const key = msg.msgId || `s:${msg.seq}`;
  if (existing.some((m) => (m.msgId || `s:${m.seq}`) === key)) return; // dedupe live echo
  // 新消息也走索引：tweb 的 insertSlice 会把更大的 seq 插到最新的那一块
  chats[convId] = mergeMessages(convId, existing, [msg]);
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

/**
 * Marks my outgoing message read when a read receipt names it (P1).
 * **Up-to 语义**：回执指到哪条，那条**及之前**的自己的消息都算已读
 * （服务端 `send_read_receipt` 发的就是 `up_to` 游标）；只勾命中的那一条
 * 会让前面的气泡永远停在单个 ✓。
 */
function markRead(convId: string, targetMsgId: string): void {
  if (!targetMsgId) return;
  const chats = { ...appStore.get(chatsAtom) };
  const list = chats[convId];
  if (!list) return;
  const target = list.find((m) => m.msgId === targetMsgId);
  if (!target) return; // 目标不在本端（没加载/别的设备发的）：无从排序，不动
  let changed = false;
  chats[convId] = list.map((m) => {
    if (m.mine && !m.read && m.seq <= target.seq) {
      changed = true;
      return { ...m, read: true };
    }
    return m;
  });
  if (changed) appStore.set(chatsAtom, chats);
}

/**
 * 收到 hub 事件的入口。整段包在 try/catch + 最小形状校验里：泵是
 * 一条长循环，一个畸形事件（缺 conv_id / seq 不是数字）抛出去会**静默
 * 杀掉整条泵**，后面的正常消息也收不到了——宁可丢这一条并 console.error。
 */
export function onInbound(payload: unknown): void {
  try {
    const ev = payload as InboundEvent;
    if (
      ev === null ||
      typeof ev !== "object" ||
      typeof ev.conv_id !== "string" ||
      typeof ev.sender !== "string" ||
      typeof ev.seq !== "number"
    ) {
      console.error("onInbound: malformed event dropped", payload);
      return;
    }
    handleInbound(ev);
  } catch (e) {
    console.error("onInbound: handler failed (event dropped)", e);
  }
}

function handleInbound(ev: InboundEvent): void {
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
    if (!mine && ev.msg_id) {
      const current = appStore.get(viewAtom);
      const isCurrent = current.screen === "chat" && current.convId === ev.conv_id;
      if (isCurrent) sendReadReceipt(ev.conv_id, ev.msg_id);
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
    case "msg.voice":
      return "[语音]";
    case "msg.file":
      return "[文件]";
    case "msg.sticker":
      return "[贴纸]";
    case "msg.video":
      return "[视频]";
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

/** 网络诊断的一步结论。 */
export interface DiagStep {
  step: "dns" | "tcp" | "http" | "quic";
  elapsed_ms: number;
  status: { status: "ok" | "failed" | "skipped"; detail: string };
}

export interface DiagReport {
  host: string;
  http_port: number;
  quic_port: number;
  steps: DiagStep[];
  summary: string;
}

/** 跑一次网络诊断：DNS → TCP → HTTP → QUIC，四步顺序短路。 */
export async function netDiag(): Promise<DiagReport> {
  return invoke<DiagReport>("net_diag");
}

/** 发送队列：待发条数与彻底失败的消息。 */
export interface OutboxStatus {
  pending: number;
  failed: Array<{
    task_id: string;
    conv_id: string;
    attempts: number;
    error: string | null;
    payload: unknown;
  }>;
}

export async function fetchOutbox(): Promise<OutboxStatus> {
  return invoke<OutboxStatus>("outbox_status");
}

/** 用户点"重发"：清零计数，后台泵会再试。 */
export async function retryOutbox(taskId: string): Promise<void> {
  await invoke("outbox_retry", { task_id: taskId });
}

/** 用户点"删除"：放弃这条。 */
export async function dropOutbox(taskId: string): Promise<void> {
  await invoke("outbox_drop", { task_id: taskId });
}

/** 后台重发泵的进度事件：补发成功 / 彻底失败都要让用户知道。 */
export function onOutbox(payload: unknown): void {
  const ev = payload as { sent?: number; retrying?: number; failed?: number; pending?: number };
  if ((ev.failed ?? 0) > 0) {
    toast(`${ev.failed} 条消息发送失败，可在会话里重发`);
    return;
  }
  if ((ev.sent ?? 0) > 0) {
    toast(`已补发 ${ev.sent} 条消息`);
  }
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
  // 本地清零只是 UI；服务端游标也要推（否则重装/换设备后未读会回来）
  void ackConv(session.convId).catch(() => undefined);
  const me = appStore.get(meAtom);
  if (me) {
    // P2 本地消息库即时开页：磁盘历史先画（零网络 RTT，会话"秒开"），
    // 服务端页随后对账盖上来——同 seq 合并取后到者，服务端是唯一真相。
    // local_autopen 幂等（boot 时已开过一次），失败不阻塞服务端路径。
    void (async () => {
      try {
        await invoke("local_autopen");
        if (await localReady()) await loadLocalHistory(session.convId, me.user_id);
      } catch {
        /* 本地库关闭/缺库都不影响服务端历史 */
      }
    })();
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

/**
 * 退出登录（P3 修复：以前 UI 里没有任何入口能走到，缓存跨账号残留）。
 * 服务端会话先失效（host `logout`），然后**本地**清空：消息索引、原子、
 * 去抖/typing 定时器。秘密会话渲染缓存在 ui.ts 侧随按钮一起清。
 */
export async function logout(): Promise<void> {
  try {
    await invoke("logout");
  } catch (e) {
    // host 侧失败也要清本地——不能让上一个账号的聊天记录留在内存里
    console.error("logout: host command failed, clearing local state anyway", e);
  }
  resetMessageIndexes();
  for (const t of unreadCorrectionTimers.values()) window.clearTimeout(t);
  unreadCorrectionTimers.clear();
  for (const t of Object.values(typingTimers)) window.clearTimeout(t);
  for (const k of Object.keys(typingTimers)) delete typingTimers[k];
  appStore.set(meAtom, null);
  appStore.set(sessionsAtom, []);
  appStore.set(chatsAtom, {});
  appStore.set(friendsAtom, []);
  appStore.set(requestsAtom, []);
  appStore.set(groupCacheAtom, {});
  appStore.set(favoritesAtom, []);
  appStore.set(searchHitsAtom, null);
  appStore.set(selectModeAtom, null);
  appStore.set(historyCursorAtom, {});
  appStore.set(typingAtom, {});
  appStore.set(balanceAtom, 0);
  appStore.set(connectedAtom, false);
  appStore.set(viewAtom, { screen: "auth" });
}

// ---------------------------------------------------------------------------
// P4/P5: 通话房间控制面 + 设备验证（全部经 Tauri 命令，前端不碰 HTTP）
// ---------------------------------------------------------------------------

export interface CallParticipant {
  user: string;
  role: string;
  audio_muted: boolean;
  video_muted: boolean;
  hand_raised: boolean;
  joined_at: number;
}

export interface CallState {
  conv_id: string;
  live: boolean;
  host?: string | null;
  locked?: boolean;
  recording?: boolean;
  participants: CallParticipant[];
}

export type CallAction =
  | "start"
  | "join"
  | "leave"
  | "mute"
  | "mute-all"
  | "kick"
  | "lock"
  | "record"
  | "hand"
  | "transfer"
  | "egress";

export async function callState(convId: string): Promise<CallState> {
  return invoke<CallState>("call_state", { conv: convId });
}

export async function callAction(
  convId: string,
  action: CallAction,
  opts: { target?: string; track?: "audio" | "video"; value?: boolean; capacity?: number } = {},
): Promise<Record<string, unknown>> {
  return invoke<Record<string, unknown>>("call_action", {
    conv: convId,
    action,
    target: opts.target ?? null,
    track: opts.track ?? null,
    value: opts.value ?? null,
    capacity: opts.capacity ?? null,
  });
}

/** 通话信令：内容结构由 `wxlike-rtc` 的 `SignalContent` 定义（invite/accept/
 * reject/cancel/hangup/candidate），前端只透传，不解释 SDP。 */
/** 本地通话引擎状态（`call://rtc` 事件的负载）。 */
export interface CallRtcStatus {
  active: boolean;
  conv: string;
  call_id: string;
  ringing: boolean;
  connected: boolean;
  phase: string;
  audio_frames: number;
  /** 本端麦克风编码后送出的帧数（未编译 `audio` feature 时恒为 0）。 */
  mic_frames: number;
  /** 对端总数（1 = 直聊，>1 = 多方 mesh）。 */
  peers: number;
  /** 其中已连通的数量。 */
  connected_peers: number;
}

/**
 * 发起语音直连：对每个对端开一条媒体会话（mesh）。
 * 房间控制面另行 `callAction`；返回每条会话的本地 call_id。
 */
export async function callRtcStart(
  convId: string,
  peers: string[],
  video = false,
): Promise<string[]> {
  return invoke<string[]>("call_rtc_start", { conv: convId, peers, video });
}

export async function callRtcAccept(): Promise<void> {
  return invoke<void>("call_rtc_accept");
}

export async function callRtcHangup(): Promise<void> {
  return invoke<void>("call_rtc_hangup");
}

export async function callRtcStatus(): Promise<CallRtcStatus> {
  return invoke<CallRtcStatus>("call_rtc_status");
}

export async function callSignalPost(
  convId: string,
  signal: Record<string, unknown>,
): Promise<{ dropped: number }> {
  return invoke<{ dropped: number }>("call_signal_post", { conv: convId, signal });
}

export async function callSignalTake(
  convId: string,
): Promise<{ signals: Record<string, unknown>[] }> {
  return invoke<{ signals: Record<string, unknown>[] }>("call_signal_take", { conv: convId });
}

export interface SafetyInfo {
  safety_number: string;
  peer_identity: string;
  verified_at: number | null;
}

export async function safetyNumber(peer: string): Promise<SafetyInfo> {
  return invoke<SafetyInfo>("safety_number", { peer });
}

export async function verifyPeer(peer: string, peerIdentity: string): Promise<void> {
  await invoke("verify_peer", { peer, peer_identity: peerIdentity });
}
