/**
 * Application state as jotai atoms (vanilla API — no React).
 *
 * One store, one `set`/`sub` surface: views re-render the slices they read,
 * events from the hub (`im-in`) and command results flow in through
 * `actions` so no view mutates another view's DOM directly.
 */

import { atom, createStore } from "jotai/vanilla";

export type View =
  | { screen: "auth" }
  | { screen: "main"; tab: "chats" | "friends" | "me" | "favorites" }
  | { screen: "chat"; convId: string; title: string; isGroup: boolean };

export interface Me {
  user_id: string;
  name: string;
  avatar: string;
  signature: string;
}

/** Server shape: FriendRow (friend_id is the peer's user id; remark is
 * the local nickname). */
export interface Friend {
  friend_id: string;
  remark: string;
  source: string;
  created_at: number;
}

export interface GroupInfo {
  group_id: string;
  name: string;
  owner_uid: string;
  members: { user_id: string; name: string; role: string }[];
}

/** Kinds the hub event payload carries. Custom feature kinds arrive as their
 * own `kind` string ("msg.forward", "wallet.redpacket", …). */
export type MsgKind =
  | "text"
  | "image"
  | "redpacket"
  | "system"
  | "recall"
  | "e2ee"
  | "typing"
  | "receipt"
  | "msg.forward"
  | "msg.merge"
  | (string & {});

export interface ChatMsg {
  seq: number;
  sender: string;
  kind: MsgKind;
  text?: string;
  mediaId?: string;
  redpacket?: { redpacket_id: string; amount: number; count: number; memo: string };
  /** forward/merge-forward payload (items, origin info) */
  payload?: Record<string, unknown>;
  msgId?: string;
  quoteOf?: string | null;
  sentAtMs: number;
  mine: boolean;
  /** E2EE path produced this text */
  encrypted?: boolean;
  /** Decrypt failed on this device (placeholder shown instead) */
  decryptFailed?: boolean;
  /** Read receipt on my own outgoing message (P1). */
  read?: boolean;
}

export interface Session {
  convId: string;
  title: string;
  isGroup: boolean;
  lastText: string;
  lastAtMs: number;
  unread: number;
}

export interface FriendRequest {
  id: string;
  from_uid: string;
  message: string;
}

export interface FavoriteRow {
  id: string;
  user_id: string;
  source_conv: string;
  source_msg_id: string;
  kind: string;
  text: string;
  media_id: string;
  sender: string;
  sent_at_ms: number;
  created_at: number;
}

export interface SearchHit {
  conv_id: string;
  seq: number;
  sender: string;
  kind: MsgKind;
  text?: string;
  sent_at_ms: number;
  msg_id?: string;
}

export interface InboundEvent {
  sender: string;
  conv_id: string;
  seq: number;
  sent_at_ms: number;
  kind: MsgKind;
  text?: string;
  media_id?: string;
  msg_id?: string;
  quote_of?: string | null;
  encrypted?: boolean;
  decrypt_failed?: boolean;
  /** recall target (hex msg id), top-level on recall events */
  target?: string;
  receipt?: { state: string; target: string };
  payload?: Record<string, unknown>;
}

export const appStore = createStore();

export const meAtom = atom<Me | null>(null);
export const viewAtom = atom<View>({ screen: "auth" });
export const sessionsAtom = atom<Session[]>([]);
export const chatsAtom = atom<Record<string, ChatMsg[]>>({});
export const friendsAtom = atom<Friend[]>([]);
export const requestsAtom = atom<FriendRequest[]>([]);
export const groupCacheAtom = atom<Record<string, GroupInfo>>({});
export const balanceAtom = atom(0);
export const connectedAtom = atom(false);
export const darkAtom = atom(localStorage.getItem("wxlike-dark") === "1");
export const searchOpenAtom = atom(false);
export const toastAtom = atom<string | null>(null);

/** P1: typing indicator per conversation — who is typing right now. */
export const typingAtom = atom<Record<string, string>>({});
/** P1: server history paging cursor per conversation (null = fully loaded). */
export const historyCursorAtom = atom<Record<string, number | null>>({});
/** P1: favorites list (Me tab → 收藏). */
export const favoritesAtom = atom<FavoriteRow[]>([]);
/** P1: in-chat message search results. */
export const searchHitsAtom = atom<SearchHit[] | null>(null);
/** P1: message-selection mode in a chat (merge forward needs many). */
export const selectModeAtom = atom<{ convId: string; seqs: number[] } | null>(null);

export function toast(msg: string): void {
  appStore.set(toastAtom, msg);
  window.setTimeout(() => {
    if (appStore.get(toastAtom) === msg) appStore.set(toastAtom, null);
  }, 2600);
}
