// 由 tools/codegen/generate.py 生成，**不要手改**。
// schema: contract-v1  摘要: ad2d2be657637cf7  contract: 1.0
// 改了契约就重跑生成器；`--check` 会在 CI 上盯着这一步。


/* eslint-disable */

export const SCHEMA_VERSION = "contract-v1" as const;
export const CONTRACT_VERSION = "1.0" as const;
export const SCHEMA_DIGEST = "ad2d2be657637cf7" as const;

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
  { id: "conv.list", capability: "read" },
  { id: "conv.unread", capability: "read" },
  { id: "conv.history", capability: "read" },
  { id: "conv.sync", capability: "read" },
  { id: "msg.list", capability: "read" },
  { id: "msg.search", capability: "read" },
  { id: "msg.get", capability: "read" },
  { id: "group.members", capability: "read" },
  { id: "group.info", capability: "read" },
  { id: "community.threads", capability: "read" },
  { id: "channel.roles", capability: "read" },
  { id: "me.info", capability: "read" },
  { id: "me.sessions", capability: "read" },
  { id: "msg.send", capability: "write" },
  { id: "conv.ack", capability: "write" },
  { id: "msg.forward", capability: "write" },
  { id: "group.create", capability: "write" },
  { id: "group.add_member", capability: "write" },
  { id: "video.select", capability: "read" },
  { id: "video.stats", capability: "read" },
  { id: "video.job.get", capability: "read" },
  { id: "video.job.create", capability: "write" },
  { id: "video.job.retry", capability: "write" },
  { id: "video.view.report", capability: "write" },
  { id: "group.kick", capability: "destructive" },
  { id: "msg.recall", capability: "destructive" },
] as const;

/** 已知 action id 集合。 */
export const KNOWN_ACTIONS: ReadonlySet<string> = new Set(ACTIONS.map((a) => a.id));

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

