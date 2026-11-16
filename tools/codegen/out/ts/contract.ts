// 由 tools/codegen/generate.py 生成，**不要手改**。
// schema: contract-v1  摘要: 470793a94f99707f  contract: 1.0
// 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。


/* eslint-disable */

export const SCHEMA_VERSION = "contract-v1" as const;
export const CONTRACT_VERSION = "1.0" as const;
export const SCHEMA_DIGEST = "470793a94f99707f" as const;

export type ActionId = string;
export type EntryId = string;
export type Cursor = string;

export interface Money {
  minor: string;
  currency: string;
}

export interface DomainError {
  code: "invalid_argument" | "unauthorized" | "forbidden" | "not_found" | "conflict" | "precondition" | "rate_limited" | "timeout" | "cancelled" | "unavailable" | "internal";
  message: string;
  field?: string;
  retry_after_ms?: number;
}

export interface ActionRequest {
  action_id: string;
  entry_id?: string;
  params: Record<string, unknown>;
  idempotency_key?: string;
  deadline?: string;
  trace_id: string;
  contract: string;
}

export interface ActionResult {
  action_id: string;
  status: "ok" | "empty" | "failed";
  data?: Record<string, unknown>;
  error?: Record<string, unknown>;
  cursor?: string;
  completed_at_ms: number;
}

export interface EventEnvelope {
  event_id: string;
  seq: string;
  conv?: string;
  kind: string;
  payload: Record<string, unknown>;
  emitted_at_ms: number;
  contract: string;
}

/** 已登记的命令（与 crates/cmd 的目录同源）。 */
export const ACTIONS = [
  { id: "channel.roles", capability: "read" },
  { id: "community.threads", capability: "read" },
  { id: "conv.list", capability: "read" },
  { id: "conv.unread", capability: "read" },
  { id: "conv.history", capability: "read" },
  { id: "conv.sync", capability: "read" },
  { id: "conv.ack", capability: "write" },
  { id: "friend.list", capability: "read" },
  { id: "friend.requests.incoming", capability: "read" },
  { id: "friend.requests.outgoing", capability: "read" },
  { id: "friend.request.create", capability: "write" },
  { id: "friend.request.accept", capability: "write" },
  { id: "friend.request.decline", capability: "write" },
  { id: "friend.remark.set", capability: "write" },
  { id: "friend.remove", capability: "destructive" },
  { id: "friend.block.list", capability: "read" },
  { id: "friend.block.add", capability: "destructive" },
  { id: "friend.block.remove", capability: "write" },
  { id: "friend.privacy.get", capability: "read" },
  { id: "friend.privacy.set", capability: "write" },
  { id: "group.members", capability: "read" },
  { id: "group.info", capability: "read" },
  { id: "group.announcement.get", capability: "read" },
  { id: "group.mute_all.get", capability: "read" },
  { id: "group.create", capability: "write" },
  { id: "group.add_member", capability: "write" },
  { id: "group.announcement.set", capability: "write" },
  { id: "group.mute_all.set", capability: "write" },
  { id: "group.kick", capability: "destructive" },
  { id: "me.info", capability: "read" },
  { id: "me.sessions", capability: "read" },
  { id: "media.files.list", capability: "read" },
  { id: "media.upload.get", capability: "read" },
  { id: "media.upload.start", capability: "write" },
  { id: "media.upload.complete", capability: "write" },
  { id: "media.upload.cancel", capability: "write" },
  { id: "media.object.access", capability: "write" },
  { id: "media.object.revoke", capability: "write" },
  { id: "msg.list", capability: "read" },
  { id: "msg.search", capability: "read" },
  { id: "msg.get", capability: "read" },
  { id: "msg.send", capability: "write" },
  { id: "msg.forward", capability: "write" },
  { id: "msg.recall", capability: "destructive" },
  { id: "video.select", capability: "read" },
  { id: "video.stats", capability: "read" },
  { id: "video.job.get", capability: "read" },
  { id: "video.job.create", capability: "write" },
  { id: "video.job.retry", capability: "write" },
  { id: "video.view.report", capability: "write" },
] as const;

/** 已知 action id 集合。 */
export const KNOWN_ACTIONS: ReadonlySet<string> = new Set(ACTIONS.map((a) => a.id));

/** 逐命令的参数类型（与 crates/cmd 的 Param 声明同源）。 */
export interface ActionParams {
  "channel.roles": { conv: string };
  "community.threads": { community: string; limit?: number };
  "conv.list": Record<string, never>;
  "conv.unread": { conv: string };
  "conv.history": { conv: string; before_seq?: number; limit?: number };
  "conv.sync": { conv: string; version: number };
  "conv.ack": { conv: string; seq?: number };
  "friend.list": Record<string, never>;
  "friend.requests.incoming": Record<string, never>;
  "friend.requests.outgoing": Record<string, never>;
  "friend.request.create": { to_user_id: string; message?: string };
  "friend.request.accept": { req_id: string };
  "friend.request.decline": { req_id: string };
  "friend.remark.set": { user_id: string; remark: string };
  "friend.remove": { user_id: string; confirmation_token?: string };
  "friend.block.list": Record<string, never>;
  "friend.block.add": { user_id: string; confirmation_token?: string };
  "friend.block.remove": { user_id: string };
  "friend.privacy.get": Record<string, never>;
  "friend.privacy.set": { allow: boolean };
  "group.members": { conv: string; after_user?: string; limit?: number };
  "group.info": { conv: string };
  "group.announcement.get": { conv: string };
  "group.mute_all.get": { conv: string };
  "group.create": { name: string; member_ids?: string[]; idempotency_key: string };
  "group.add_member": { conv: string; user_ids: string[] };
  "group.announcement.set": { conv: string; text: string; media_id?: string };
  "group.mute_all.set": { conv: string; on: boolean };
  "group.kick": { conv: string; user_id: string; confirmation_token?: string };
  "me.info": Record<string, never>;
  "me.sessions": Record<string, never>;
  "media.files.list": { category?: string; folder?: string; sort?: string; cursor?: string; limit?: number };
  "media.upload.get": { upload_id: string };
  "media.upload.start": { purpose: string; name: string; size: number; mime: string; digest?: string };
  "media.upload.complete": { upload_id: string; digest?: string };
  "media.upload.cancel": { upload_id: string };
  "media.object.access": { object_id: string };
  "media.object.revoke": { object_id: string };
  "msg.list": { conv: string; from_seq: number; to_seq?: number };
  "msg.search": { q: string; limit?: number };
  "msg.get": { conv: string; msg_id: string };
  "msg.send": { conv: string; text: string; reply_to?: string; idempotency_key: string };
  "msg.forward": { from_conv: string; to_conv: string; msg_id: string; idempotency_key: string };
  "msg.recall": { conv: string; msg_id: string; confirmation_token?: string };
  "video.select": { video_id: string; kbps: number };
  "video.stats": { video_id: string };
  "video.job.get": { job_id: string };
  "video.job.create": { path: string; callback_url?: string };
  "video.job.retry": { job_id: string };
  "video.view.report": { video_id: string; viewer: string; watched_ms: number; position_ms?: number; completed?: boolean; session_id?: string };
}

/**
 * 解码一条命令结果。
 *
 * 未知 `status` 返回 `null`：服务端先上线新状态、旧客户端后升级是常态，
 * 这时候应当"跳过并记一笔"，不是抛异常把整条渲染链打断。
 */
export function decodeActionResult(raw: unknown): ActionResult | null {
  if (raw === null || typeof raw !== "object") return null;
  const value = raw as Record<string, unknown>;
  if (typeof value.action_id !== "string") return null;
  if (value.status !== "ok" && value.status !== "empty" && value.status !== "failed") return null;
  return value as unknown as ActionResult;
}

/** 金额：字符串 → BigInt（超过 2^53 也不能丢精度）。 */
export function moneyToMinor(money: Money): bigint {
  return BigInt(money.minor);
}

/**
 * FFI 事件帧（S2）：CoreHandle.poll / subscribe 推出的每条消息。
 * result 帧把规范 ActionResult 整个 flatten，故 decodeActionResult 可直接解。
 * 与 crates/ffi 的类型化帧（ResultFrame/ErrorFrame/FatalFrame）同源。
 */
export type CoreFrame =
  | ({ kind: "result"; request_id: string } & ActionResult)
  | { kind: "error"; request_id: string; error: DomainError; retryable: boolean }
  | { kind: "fatal"; message: string };

/** 解码一条 FFI 帧；未知 kind 返回 null（跳过，不抛）。 */
export function decodeCoreFrame(raw: unknown): CoreFrame | null {
  if (raw === null || typeof raw !== "object") return null;
  const value = raw as Record<string, unknown>;
  if (value.kind === "result") {
    const result = decodeActionResult(value);
    if (result === null || typeof value.request_id !== "string") return null;
    return { kind: "result", request_id: value.request_id, ...result };
  }
  if (value.kind === "error") {
    if (typeof value.request_id !== "string" || value.error === null || typeof value.error !== "object") return null;
    return { kind: "error", request_id: value.request_id, error: value.error as DomainError, retryable: value.retryable === true };
  }
  if (value.kind === "fatal") {
    if (typeof value.message !== "string") return null;
    return { kind: "fatal", message: value.message };
  }
  return null;
}

