/**
 * Consumer-side view of the Rust core contract (ADR-04).
 *
 * G01 owns the generated bindings. Until G01-02/G01-03 land this file is the
 * frozen shape G10 codes against: ActionRequest in, ActionResponse/EventEnvelope
 * out, no HTTP/QUIC/bincode in the webview. Field names and the action catalog
 * must be ratified by G00-03 before integration acceptance.
 */

export type ActionId = string;

export interface ActionRequest<P = unknown> {
  action_id: ActionId;
  request_id: string;
  schema_version: 1;
  idempotency_key?: string;
  params: P;
}

export type ErrorCode =
  | "UNAUTHENTICATED"
  | "FORBIDDEN"
  | "VALIDATION"
  | "CONFLICT"
  | "RATE_LIMITED"
  | "REQUIRES_CONFIRMATION"
  | "DEPENDENCY_UNAVAILABLE"
  | "NOT_IMPLEMENTED"
  | "OFFLINE"
  | "CANCELLED"
  | "INTERNAL";

export interface DomainError {
  code: ErrorCode;
  message: string;
  field_errors?: Record<string, string>;
  retryable: boolean;
  retry_after_ms?: number;
  request_id: string;
}

export type ActionResponse<T> =
  | { ok: true; data: T }
  | { ok: false; error: DomainError };

export interface EventEnvelope<T = unknown> {
  event_id: string;
  aggregate_id: string;
  aggregate_version: string;
  event_type: string;
  server_time_ms: string;
  payload: T;
}

export interface EventCursor {
  stream_id: string;
  cursor: string;
}

export type Unsubscribe = () => void;

export interface CoreCapabilities {
  contract_version: string | null;
  transport: "tauri" | "wasm" | "fixture";
  actions: ActionId[];
  qr_login: boolean;
  secrets_os_backed: boolean;
  sync_range: boolean;
  notes?: string[];
}

export interface CoreHandle {
  dispatch<T = unknown>(request: ActionRequest): Promise<ActionResponse<T>>;
  subscribe(cursor: EventCursor | null, handler: (event: EventEnvelope) => void): Unsubscribe;
  capabilities(): Promise<CoreCapabilities>;
}

/** Stable action ids used by G10 screens. Domain owners ratify in G00-03. */
export const ACTIONS = {
  sessionProjection: "identity.session.projection",
  login: "identity.session.login",
  register: "identity.session.register",
  logout: "identity.session.logout",
  lock: "security.lock",
  unlock: "security.unlock",
  restore: "identity.session.restore",
  qrCreate: "identity.qr_login.create",
  qrCancel: "identity.qr_login.cancel",
  qrSwitchAccount: "identity.qr_login.switch_account",
  syncRangeGet: "sync.range.get",
  syncRangeSet: "sync.range.set",

  conversationsList: "messaging.conversations.list",
  messagesPage: "messaging.messages.page",
  messageSend: "messaging.send",
  messageRetry: "messaging.retry",
  messageRecall: "messaging.recall",
  messageForward: "messaging.forward",
  messageRead: "messaging.read",
  conversationPin: "messaging.conversation.pin",
  conversationMute: "messaging.conversation.mute",
  conversationReadAll: "messaging.conversation.read_all",
  conversationEnsure: "messaging.conversation.ensure",
  draftGet: "messaging.draft.get",
  draftSave: "messaging.draft.save",
  draftClear: "messaging.draft.clear",

  mediaUploadBegin: "media.upload.begin",
  mediaUploadPart: "media.upload.part",
  mediaUploadComplete: "media.upload.complete",
  mediaUploadCancel: "media.upload.cancel",

  relationsList: "relations.list",
  requestsIncoming: "relations.requests.incoming",
  requestSend: "relations.request.send",
  requestAccept: "relations.request.accept",
  requestDecline: "relations.request.decline",
  relationBlock: "relations.block",
  relationNote: "relations.note.set",
  relationTag: "relations.tag.set",
  relationStar: "relations.star.set",
  relationProfile: "relations.profile",
  commonGroups: "relations.common_groups",
  groupMembers: "relations.group.members",
  groupKick: "relations.group.kick",
  groupSettings: "relations.group.settings.set",

  // 钱包域（#293 提供者 / #294 消费）。名单与 `core_bridge.rs` 的 `wallet_router`
  // 逐条对齐；`scripts/check-core-transport.mjs` 会把这里与 `lib/domain-registry.ts`
  // 对一遍，两边写歪任一处都红。`redpacketList` 服务端没有路由，桥如实回
  // NOT_IMPLEMENTED —— 登记它不是为了假装能用，而是为了让这个路径不许回落到旧 HTTP。
  walletBalanceGet: "wallet.balance.get",
  walletLedgerPage: "wallet.ledger.page",
  walletTransfer: "wallet.transfer",
  walletRedpacketCreate: "wallet.redpacket.create",
  walletRedpacketGet: "wallet.redpacket.get",
  walletRedpacketClaim: "wallet.redpacket.claim",
  walletRedpacketList: "wallet.redpacket.list",

  // 通话域（D6 逐域退役）。名单与 `core_bridge` 的 `call_router` 逐条对齐；
  // `scripts/check-core-transport.mjs` 会把这里与 `lib/core-transport.ts` 的
  // CALL_ROUTES 对一遍，两边写歪任一处都红。控制类端点服务端回 204，桥读成 {}。
  callState: "call.state",
  callStart: "call.start",
  callJoin: "call.join",
  callLeave: "call.leave",
  callMute: "call.mute",
  callMuteAll: "call.mute_all",
  callLock: "call.lock",
  callRecord: "call.record",
  callHand: "call.hand",
  callKick: "call.kick",
  callSignalPost: "call.signal.post",
  callSignalTake: "call.signal.take",

  // 视频/直播域（D6 逐域退役）：只退役控制面。播放面（清单/分片/mp4/缩略图）
  // 服务端按设计匿名（issue #7），不经过桥；上传在消费侧显式阻塞。名单与
  // `core_bridge` 的 `video_router` 逐条对齐。
  videoMeta: "video.meta",
  videoStats: "video.stats",
  videoView: "video.view",
  videoFeed: "video.feed",
  liveState: "live.state",
  liveStart: "live.start",
  liveStop: "live.stop",
  liveJoin: "live.join",
  liveLeave: "live.leave",
  liveDanmaku: "live.danmaku",

  // 内容域（D6 逐域退役）：moments/favorites/report/stickers 控制面经桥；
  // G05 契约面里服务端未对齐的部分（articles/publishers）在消费侧显式阻塞。
  // 名单与 `core_bridge` 的 `content_router` 逐条对齐。
  momentPublish: "moment.publish",
  momentFeed: "moment.feed",
  momentDetail: "moment.detail",
  momentRemove: "moment.remove",
  momentLike: "moment.like",
  momentUnlike: "moment.unlike",
  momentComment: "moment.comment",
  momentCommentRemove: "moment.comment.remove",
  reportCreate: "report.create",
  favoriteList: "favorite.list",
  favoriteOne: "favorite.one",
  favoriteRemove: "favorite.remove",
  favoriteAdd: "favorite.add",
  favoriteUpdate: "favorite.update",
  stickerPacks: "sticker.packs",
  momentMediaUrl: "moment.media_url",

  // 文件域（D6 逐域退役）：文件中心控制面（列表/改名/分享/删除）经桥打到 G04
  // `/api/v1/files*`。二进制上传/下载与备份清理计划无 JSON 流式提供者，在消费侧
  // 显式阻塞。名单与 `core_bridge` 的 `files_router` 逐条对齐。
  fileList: "file.list",
  fileRename: "file.rename",
  fileShare: "file.share",
  fileRemove: "file.remove",

  // 发现域（D6 逐域退役）：服务目录控制面（目录/报价/下单/查单/列举/退款）经桥
  // 打到 G07 服务目录服务端。识图/翻译（multipart + 外部 AI）无核心提供者，在
  // 消费侧显式阻塞。名单与 `core_bridge` 的 `discover_router` 逐条对齐。
  discoverServices: "discover.services",
  discoverQuote: "discover.quote",
  discoverOrderCreate: "discover.order_create",
  discoverOrderGet: "discover.order_get",
  discoverOrders: "discover.orders",
  discoverOrderRefund: "discover.order_refund",

  // 小程序域（D6 逐域退役）：目录/授权/卸载控制面经桥打到 G08 服务端。
  // install 与包体下载（二进制 + sha256 校验）无核心提供者，在消费侧显式阻塞。
  // 名单与 `core_bridge` 的 `mini_router` 逐条对齐。
  miniList: "mini.list",
  miniReleases: "mini.releases",
  miniGrants: "mini.grants",
  miniGrant: "mini.grant",
  miniRevoke: "mini.revoke",
  miniUninstall: "mini.uninstall",

  searchQuery: "search.query",
  preferencesGet: "preferences.get",
  preferencesSet: "preferences.set",
  secretsStatus: "secrets.status",
  secretsLock: "secrets.lock",
  secretsMigrate: "secrets.migrate",
  storageStatus: "storage.status",
  cacheClear: "storage.cache.clear",
  backupCreate: "backup.create",
  backupVerify: "backup.verify",
  backupImport: "backup.import",
  appInfo: "app.info",
} as const;

export function isErrorCode(value: string): value is ErrorCode {
  return [
    "UNAUTHENTICATED",
    "FORBIDDEN",
    "VALIDATION",
    "CONFLICT",
    "RATE_LIMITED",
    "REQUIRES_CONFIRMATION",
    "DEPENDENCY_UNAVAILABLE",
    "NOT_IMPLEMENTED",
    "OFFLINE",
    "CANCELLED",
    "INTERNAL",
  ].includes(value);
}

export function domainError(
  code: ErrorCode,
  message: string,
  requestId = "local",
  extra: Partial<DomainError> = {},
): DomainError {
  return { code, message, retryable: false, request_id: requestId, ...extra };
}

export function ok<T>(data: T): ActionResponse<T> {
  return { ok: true, data };
}

export function fail<T = never>(error: DomainError): ActionResponse<T> {
  return { ok: false, error };
}

let requestSeq = 0;

export function newRequest(actionId: ActionId, params: unknown, idempotencyKey?: string): ActionRequest {
  requestSeq += 1;
  const request: ActionRequest = {
    action_id: actionId,
    request_id: `ui-${Date.now().toString(36)}-${requestSeq}`,
    schema_version: 1,
    params,
  };
  if (idempotencyKey) request.idempotency_key = idempotencyKey;
  return request;
}

export function newIdempotencyKey(scope: string): string {
  const random = globalThis.crypto?.randomUUID?.() ?? Math.random().toString(36).slice(2);
  return `${scope}:${random}`;
}

/** User-facing message for an error; never echoes server internals. */
export function describeError(error: DomainError): string {
  switch (error.code) {
    case "UNAUTHENTICATED":
      return "登录已失效，请重新登录";
    case "FORBIDDEN":
      return "没有权限执行该操作";
    case "VALIDATION":
      return "输入有误，请检查后重试";
    case "CONFLICT":
      return "数据已变化，请刷新后重试";
    case "RATE_LIMITED":
      return "操作过于频繁，请稍后再试";
    case "REQUIRES_CONFIRMATION":
      return "需要再次确认";
    case "DEPENDENCY_UNAVAILABLE":
      return "依赖服务暂不可用";
    case "NOT_IMPLEMENTED":
      return "该能力尚未接入";
    case "OFFLINE":
      return "网络已断开";
    case "CANCELLED":
      return "操作已取消";
    case "INTERNAL":
      return "内部错误，请稍后重试";
  }
}
