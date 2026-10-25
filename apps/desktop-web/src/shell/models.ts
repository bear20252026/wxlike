import type { DomainError } from "./core-contract";

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
  has_more: boolean;
  snapshot_version?: string;
}

export type MessageState =
  | "LOCAL_QUEUED"
  | "SENDING"
  | "SERVER_ACCEPTED"
  | "DEVICE_STORED"
  | "FAILED_RETRYABLE"
  | "FAILED_FINAL"
  | "RECALLED";

export type BodyKind = "text" | "image" | "voice" | "file" | "sticker" | "link" | "system";

export interface MessageBody {
  kind: BodyKind;
  text?: string;
  media_id?: string;
  name?: string;
  size?: number;
  mime?: string;
  duration_ms?: number;
  width?: number;
  height?: number;
  quote_msg_id?: string;
  sticker?: string;
}

export interface ChatMessage {
  msg_id: string;
  conv_id: string;
  seq: string;
  sender: { user_id: string; name: string; avatar?: string };
  direction: "in" | "out";
  body: MessageBody;
  state: MessageState;
  created_at_ms: string;
  client_msg_id?: string;
  recalled?: boolean;
  error?: { code: string; message: string; retryable: boolean };
}

export type ConversationKind = "direct" | "group" | "file";

export interface Conversation {
  conv_id: string;
  kind: ConversationKind;
  title: string;
  avatar?: string;
  peer_user_id?: string;
  unread: number;
  pinned: boolean;
  muted: boolean;
  last_preview: string;
  last_at_ms: string;
  draft: string;
}

export interface Contact {
  user_id: string;
  name: string;
  avatar?: string;
  note?: string;
  tags: string[];
  star: boolean;
  online: boolean;
  region?: string;
  blocked: boolean;
}

export interface FriendRequest {
  request_id: string;
  from: { user_id: string; name: string; avatar?: string };
  message: string;
  status: "PENDING" | "ACCEPTED" | "DECLINED";
  created_at_ms: string;
}

export type GroupRole = "owner" | "admin" | "member";

export interface GroupMember {
  user_id: string;
  name: string;
  avatar?: string;
  role: GroupRole;
  joined_at_ms: string;
}

export interface GroupSummary {
  group_id: string;
  name: string;
  avatar?: string;
  member_count: number;
  my_role: GroupRole;
}

export interface RelationProfile {
  user_id: string;
  name: string;
  avatar?: string;
  note?: string;
  region?: string;
  signature?: string;
  is_friend: boolean;
  blocked: boolean;
  common_groups: { group_id: string; name: string }[];
}

export type SearchCategory = "all" | "chats" | "contacts" | "groups" | "content";

export interface SearchHit {
  id: string;
  kind: "chat" | "contact" | "group" | "content";
  title: string;
  subtitle: string;
  conv_id?: string;
  user_id?: string;
  group_id?: string;
  snippet?: string;
}

export interface SearchResults {
  category: SearchCategory;
  hits: SearchHit[];
  unavailable?: { category: SearchCategory; reason: string }[];
}

export type ThemePreference = "light" | "dark" | "system";

export interface Preferences {
  theme: ThemePreference;
  compact: boolean;
  notification_preview: boolean;
  enter_to_send: boolean;
  sync_range_days: 7 | 30 | 90 | 0;
  auto_login: boolean;
}

export type SessionPhase = "loading" | "signed_out" | "locked" | "restoring" | "authenticated" | "error";

export interface SessionAccount {
  user_id: string;
  name: string;
  avatar?: string;
  device_id: string;
  session_id: string;
}

export interface SessionProjection {
  phase: SessionPhase;
  account?: SessionAccount;
  error?: DomainError;
  lock_reason?: "manual" | "resume" | "timeout";
}

export type QrState =
  | "CREATED"
  | "SCANNED"
  | "WAITING_APPROVAL"
  | "APPROVED"
  | "EXPIRED"
  | "REJECTED";

export interface QrChallenge {
  challenge_id: string;
  state: QrState;
  qr_svg?: string;
  payload?: string;
  expires_at_ms: string;
  account_hint?: string;
}

export interface AppInfo {
  name: string;
  version: string;
  commit: string;
  built_at: string;
  licenses: { name: string; license: string; url?: string }[];
}

export interface StorageStatus {
  cache_bytes: number;
  message_bytes: number;
  media_bytes: number;
}

export interface BackupInfo {
  file_name: string;
  size: number;
  digest: string;
  created_at_ms: string;
}
