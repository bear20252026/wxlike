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
  | { screen: "main"; tab: "chats" | "friends" | "me" }
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

export type MsgKind = "text" | "image" | "redpacket" | "system" | "recall" | "other";

export interface ChatMsg {
  seq: number;
  sender: string;
  kind: MsgKind;
  text?: string;
  mediaId?: string;
  redpacket?: { redpacket_id: string; amount: number; count: number; memo: string };
  sentAtMs: number;
  mine: boolean;
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

export interface InboundEvent {
  sender: string;
  conv_id: string;
  seq: number;
  sent_at_ms: number;
  kind: MsgKind;
  text?: string;
  media_id?: string;
  payload?: {
    redpacket_id?: string;
    amount?: number;
    count?: number;
    memo?: string;
  };
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

export function toast(msg: string): void {
  appStore.set(toastAtom, msg);
  window.setTimeout(() => {
    if (appStore.get(toastAtom) === msg) appStore.set(toastAtom, null);
  }, 2600);
}
