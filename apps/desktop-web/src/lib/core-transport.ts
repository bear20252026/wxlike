/**
 * 受控迁移层的核心路由（issue #294 / G10）：钱包域 → `CoreHandle`/action。
 *
 * ## 它解决的具体故障
 *
 * 迁移前钱包的调用链是 `features/commerce/api.ts` → `request()` → `http.ts` 的
 * `fetchTransport` → `baseUrl()`/`token()` 从 localStorage 里取一份**没人写过**的
 * 会话（`setBaseUrl`/`saveSession` 全仓零调用方）。后果就是卡面说的"分裂会话 +
 * 点进去全是空"：登录写进 Rust 核心，界面却拿另一份空 storage 发无鉴权 HTTP。
 *
 * ## 迁移后的形状（调用方一行不改）
 *
 * `features/commerce/api.ts` 由 G11 顺序消费，本卡不能同时改它，所以适配做在这一层：
 * 它仍然调 `request("GET", "/api/wallet/balance")`，装好之后这句话变成
 * `core.dispatch({ action_id: "wallet.balance.get", … })`，返回值由桥的形状投影回
 * `api.ts` 本来就在解析的服务端形状。base/Bearer 从此不进 webview（#293 把它们留在
 * Rust 侧的 `Http` 里），非 2xx 由桥映射成 `UNAUTHENTICATED`/`FORBIDDEN`…，本层再
 * 映射成 `ApiError`，**不折成空载荷**。
 *
 * ## 三条硬规矩
 *
 * 1. **不回落**：已迁移域的路径若没有对应 action（G06 的支付意图/提现/绑卡…），
 *    返回 `blocked`+501 的显式阻塞，绝不"先试试旧 HTTP"。
 * 2. **不编字段**：投影只搬运桥给的字段；缺字段就抛错，未知 kind 不猜方向。
 * 3. **不跨会话**：每次 dispatch 记 generation；会话一切换，旧代次的结果一律丢弃
 *    并把 `request_id` 从在途台账里摘掉（`resetCoreSession` 返回被丢了几条，
 *    用例据此断言，而不是靠注释说"这里会清"）。
 */

import {
  ApiError,
  blockedError,
  clearLegacySession,
  fetchTransport,
  isCoreOnlyPath,
  setFallbackTransport,
  type ApiErrorCode,
  type Json,
  type Transport,
} from "./http";
import { newRequest, ACTIONS, type CoreHandle, type DomainError } from "../shell/core-contract";

/**
 * 钱包 action 的名字只在 `shell/core-contract.ts` 的 `ACTIONS` 里有一份；这里只是
 * 给路由表起个短别名，避免在本文件再抄一遍字符串（#296 定的"名单只留一处"）。
 */
const WALLET_ACTIONS = {
  balanceGet: ACTIONS.walletBalanceGet,
  ledgerPage: ACTIONS.walletLedgerPage,
  transfer: ACTIONS.walletTransfer,
  redpacketCreate: ACTIONS.walletRedpacketCreate,
  redpacketList: ACTIONS.walletRedpacketList,
  redpacketGet: ACTIONS.walletRedpacketGet,
  redpacketClaim: ACTIONS.walletRedpacketClaim,
} as const;

/**
 * 通话 action 的名字同样只在 `shell/core-contract.ts` 的 `ACTIONS` 里有一份；
 * 这里只是给路由表起短别名（#296 的"名单只留一处"）。
 */
const CALL_ACTIONS = {
  state: ACTIONS.callState,
  start: ACTIONS.callStart,
  join: ACTIONS.callJoin,
  leave: ACTIONS.callLeave,
  mute: ACTIONS.callMute,
  muteAll: ACTIONS.callMuteAll,
  lock: ACTIONS.callLock,
  record: ACTIONS.callRecord,
  hand: ACTIONS.callHand,
  kick: ACTIONS.callKick,
  signalPost: ACTIONS.callSignalPost,
  signalTake: ACTIONS.callSignalTake,
} as const;

/**
 * 视频/直播 action 别名（名单只留一处，同钱包/通话）。只覆盖控制面；
 * 播放面匿名、上传显式阻塞，都不在这张表里。
 */
const VIDEO_ACTIONS = {
  meta: ACTIONS.videoMeta,
  stats: ACTIONS.videoStats,
  view: ACTIONS.videoView,
  feed: ACTIONS.videoFeed,
  liveState: ACTIONS.liveState,
  liveStart: ACTIONS.liveStart,
  liveStop: ACTIONS.liveStop,
  liveJoin: ACTIONS.liveJoin,
  liveLeave: ACTIONS.liveLeave,
  liveDanmaku: ACTIONS.liveDanmaku,
} as const;

/**
 * 内容域 action 别名（名单只留一处）。moments/favorites/report/stickers 控制面；
 * G05 未对齐面（articles/publishers）显式阻塞，不在这张表里。
 */
const CONTENT_ACTIONS = {
  momentPublish: ACTIONS.momentPublish,
  momentFeed: ACTIONS.momentFeed,
  momentDetail: ACTIONS.momentDetail,
  momentRemove: ACTIONS.momentRemove,
  momentLike: ACTIONS.momentLike,
  momentUnlike: ACTIONS.momentUnlike,
  momentComment: ACTIONS.momentComment,
  momentCommentRemove: ACTIONS.momentCommentRemove,
  reportCreate: ACTIONS.reportCreate,
  favoriteList: ACTIONS.favoriteList,
  favoriteOne: ACTIONS.favoriteOne,
  favoriteRemove: ACTIONS.favoriteRemove,
  favoriteAdd: ACTIONS.favoriteAdd,
  favoriteUpdate: ACTIONS.favoriteUpdate,
  stickerPacks: ACTIONS.stickerPacks,
  momentMediaUrl: ACTIONS.momentMediaUrl,
} as const;

/**
 * 文件域 action 别名（名单只留一处）。文件中心控制面（列表/改名/分享/删除）；
 * 二进制上传/下载与备份清理计划显式阻塞，不在这张表里。
 */
const FILES_ACTIONS = {
  list: ACTIONS.fileList,
  rename: ACTIONS.fileRename,
  share: ACTIONS.fileShare,
  remove: ACTIONS.fileRemove,
} as const;

/**
 * 发现域 action 别名（名单只留一处）。服务目录控制面（目录/报价/下单/查单/
 * 列举/退款）；识图/翻译（multipart + 外部 AI）显式阻塞，不在这张表里。
 */
const DISCOVER_ACTIONS = {
  services: ACTIONS.discoverServices,
  quote: ACTIONS.discoverQuote,
  orderCreate: ACTIONS.discoverOrderCreate,
  orderGet: ACTIONS.discoverOrderGet,
  orders: ACTIONS.discoverOrders,
  orderRefund: ACTIONS.discoverOrderRefund,
} as const;

export interface CoreSessionSnapshot {
  installed: boolean;
  generation: number;
  /** 在途（已 dispatch、还没落地）的 request_id。会话切换会清空这张台账。 */
  pending: string[];
  /** 本代次被丢弃的旧会话结果条数（可观测的"不泄露"计数）。 */
  dropped: number;
}

interface Session {
  core: CoreHandle | null;
  generation: number;
  pending: Set<string>;
  dropped: number;
}

let session: Session = { core: null, generation: 0, pending: new Set(), dropped: 0 };

/** 装上 Rust 核心：把默认出口换成"按域路由"。返回装好后的代次。 */
export function installCoreSession(core: CoreHandle): number {
  session = { ...session, core };
  setFallbackTransport(coreRoutingTransport());
  return session.generation;
}

/** 卸载（宿主没了/测试收尾）：默认出口退回遗留 fetch。 */
export function uninstallCoreSession(): void {
  session = { ...session, core: null };
  setFallbackTransport(null);
}

/**
 * 服务器切换 / 账号切换 / 退出登录时必须走这里。三件事一次做完，返回值让断言有得比：
 *
 * * `generation += 1`：旧代次在途请求落地时**一律丢弃**（`staleError`），所以
 *   上一个用户的转账/领红包结果不会写进新会话的界面；
 * * `pending` 台账清空、`dropped` 计数增加 —— 用例断言"切换前挂着的这条确实被算了"；
 * * `clearLegacySession()` 抹掉 localStorage 里残留的 bearer/base/身份（分裂会话的
 *   另一半来源），返回实际删掉的键名，断言得以落在"真的删了哪几个键"。
 */
export function resetCoreSession(reason: string): {
  generation: number;
  dropped: number;
  clearedKeys: string[];
  reason: string;
} {
  const dropped = session.pending.size;
  session = {
    core: session.core,
    generation: session.generation + 1,
    pending: new Set(),
    dropped: session.dropped + dropped,
  };
  const clearedKeys = clearLegacySession();
  return { generation: session.generation, dropped, clearedKeys, reason };
}

export function coreSessionSnapshot(): CoreSessionSnapshot {
  return {
    installed: session.core !== null,
    generation: session.generation,
    pending: [...session.pending],
    dropped: session.dropped,
  };
}

/** 只给用例读：重置全部模块级状态（代次与台账），避免跨用例串味。 */
export function __resetCoreSessionForTests(): void {
  session = { core: null, generation: 0, pending: new Set(), dropped: 0 };
  setFallbackTransport(null);
}

// ---------------------------------------------------------------------------
// 钱包路由表：URL → action →（响应投影）
// ---------------------------------------------------------------------------

interface Match {
  params: Record<string, string>;
  query: URLSearchParams;
}

interface WalletRoute {
  method: "GET" | "POST" | "DELETE" | "PUT" | "PATCH";
  /** 路径模板，`:x` 为段参数；不含 query。 */
  pattern: string;
  action: string;
  /** 从 HTTP 形状拼 action params。缺字段就少传，由桥如实报错——这里不兜默认值。 */
  toParams(match: Match, body: Json): Json;
  /** 桥的返回形状 → `features/commerce/api.ts` 本来就在解析的形状。 */
  project?(data: unknown): unknown;
}

export const WALLET_ROUTES: readonly WalletRoute[] = [
  {
    method: "GET",
    pattern: "/api/wallet/balance",
    action: WALLET_ACTIONS.balanceGet,
    toParams: () => ({}),
    // 桥：{balance_minor, currency}；api.ts：{balance}（整数分）
    project: (data) => ({ balance: needNumber(data, "balance_minor"), currency: needString(data, "currency") }),
  },
  {
    method: "GET",
    pattern: "/api/wallet/ledger",
    action: WALLET_ACTIONS.ledgerPage,
    toParams: ({ query }) => {
      const params: Json = {};
      const since = query.get("since");
      const limit = query.get("limit");
      if (since !== null) params.since = Number(since);
      if (limit !== null) params.limit = Number(limit);
      return params;
    },
    project: (data) => ({
      entries: needArray(data, "items").map((row) => ledgerRow(row)),
      cursor: optionalNumberOrNull(data, "next_cursor"),
      has_more: needBoolean(data, "has_more"),
    }),
  },
  {
    method: "POST",
    pattern: "/api/wallet/transfer",
    action: WALLET_ACTIONS.transfer,
    // api.ts 发的是服务端字段名 amount（整数分），桥的入参叫 amount_minor。
    toParams: (_match, body) => ({
      to_uid: body.to_uid,
      amount_minor: body.amount,
      memo: body.memo ?? null,
    }),
    project: (data) => ({ transfer_id: needString(data, "transfer_id") }),
  },
  {
    method: "POST",
    pattern: "/api/wallet/redpacket",
    action: WALLET_ACTIONS.redpacketCreate,
    toParams: (_match, body) => ({
      amount_minor: body.amount,
      count: body.count,
      memo: body.memo ?? null,
      conv_id: body.conv_id ?? null,
    }),
    project: (data) => ({ redpacket_id: needString(data, "redpacket_id") }),
  },
  {
    method: "GET",
    pattern: "/api/wallet/redpacket",
    action: WALLET_ACTIONS.redpacketList,
    toParams: ({ query }) => {
      const params: Json = {};
      const limit = query.get("limit");
      if (limit !== null) params.limit = Number(limit);
      return params;
    },
    // 桥对这条如实回 NOT_IMPLEMENTED（服务端没有红包列表路由），所以没有可投影的
    // 数据形状；本条存在的意义是让这个路径"登记在册"，不会偷偷回落到旧 HTTP。
  },
  {
    method: "GET",
    pattern: "/api/wallet/redpacket/:redpacket_id",
    action: WALLET_ACTIONS.redpacketGet,
    toParams: (match) => ({ redpacket_id: match.params.redpacket_id }),
    project: (data) => redpacketRecord(data),
  },
  {
    method: "POST",
    pattern: "/api/wallet/redpacket/:redpacket_id/claim",
    action: WALLET_ACTIONS.redpacketClaim,
    toParams: (match) => ({ redpacket_id: match.params.redpacket_id }),
    // 桥：{redpacket_id, amount_minor}；api.ts 读 {amount}
    project: (data) => ({ amount: needNumber(data, "amount_minor"), redpacket_id: needString(data, "redpacket_id") }),
  },
];

export function walletRouteCensus(): string[] {
  return WALLET_ROUTES.map((route) => `${route.method} ${route.pattern} -> ${route.action}`);
}

// ---------------------------------------------------------------------------
// 通话路由表：URL → action →（响应投影）
//
// 与钱包同机制：`features/calls/api.ts` 仍调 `request("POST", "/api/call/…")`，
// 装好核心会话后这句话变成 `core.dispatch({ action_id: "call.…", … })`。控制类
// 端点服务端回 204，桥读成 {}，这里无需投影；call.state 把服务端的
// {conv_id,live,locked,…} 投影成 api.ts 本来就在解析的 CallState 形状。
// ---------------------------------------------------------------------------

export const CALL_ROUTES: readonly WalletRoute[] = [
  {
    method: "GET",
    pattern: "/api/call/:conv",
    action: CALL_ACTIONS.state,
    toParams: (match) => ({ conv: match.params.conv }),
    // 桥透传服务端 {conv_id, live, locked, recording, participants}；
    // api.ts 的 CallState 读 {conv, state, participants, recording}。
    project: (data) => {
      const row = asRecord(data, "call.state");
      const live = row.live === true;
      const locked = row.locked === true;
      return {
        conv: needString(row, "conv_id"),
        state: live ? (locked ? "locked" : "active") : "idle",
        participants: needArray(row, "participants"),
        ...(typeof row.recording === "boolean" ? { recording: row.recording } : {}),
      };
    },
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/start",
    action: CALL_ACTIONS.start,
    toParams: (match, body) => ({
      conv: match.params.conv,
      ...(body.capacity !== undefined ? { capacity: body.capacity } : {}),
    }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/join",
    action: CALL_ACTIONS.join,
    toParams: (match) => ({ conv: match.params.conv }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/leave",
    action: CALL_ACTIONS.leave,
    toParams: (match) => ({ conv: match.params.conv }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/mute",
    action: CALL_ACTIONS.mute,
    // target 可选：旧前端单人静音不传 target，服务端按 422 拒绝——保持透传，不兜默认。
    toParams: (match, body) => ({
      conv: match.params.conv,
      track: body.track,
      muted: body.muted,
      ...(body.target !== undefined ? { target: body.target } : {}),
    }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/mute-all",
    action: CALL_ACTIONS.muteAll,
    toParams: (match, body) => ({ conv: match.params.conv, track: body.track, muted: body.muted }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/lock",
    action: CALL_ACTIONS.lock,
    toParams: (match, body) => ({ conv: match.params.conv, value: body.value }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/record",
    action: CALL_ACTIONS.record,
    toParams: (match, body) => ({ conv: match.params.conv, value: body.value }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/hand",
    action: CALL_ACTIONS.hand,
    toParams: (match, body) => ({ conv: match.params.conv, value: body.value }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/kick",
    action: CALL_ACTIONS.kick,
    toParams: (match, body) => ({ conv: match.params.conv, target: body.target }),
  },
  {
    method: "POST",
    pattern: "/api/call/:conv/signal",
    action: CALL_ACTIONS.signalPost,
    // 信令信封不透明：整个 body 作为 signal 透传，桥不改写内容。
    toParams: (match, body) => ({ conv: match.params.conv, signal: body }),
  },
  {
    method: "GET",
    pattern: "/api/call/:conv/signal",
    action: CALL_ACTIONS.signalTake,
    toParams: (match) => ({ conv: match.params.conv }),
    // 桥回 {signals}；api.ts 读 reply.signals ?? []，原样透传。
  },
];

export function callRouteCensus(): string[] {
  return CALL_ROUTES.map((route) => `${route.method} ${route.pattern} -> ${route.action}`);
}

// ---------------------------------------------------------------------------
// 视频/直播路由表：只覆盖控制面。
//
// `features/video/api.ts` 的控制面调用经此映射到 video.*/live.* action；桥透传
// 服务端形状，feed 的行映射（mapFeedRow）仍留在 api.ts。播放面（play.mp4/
// playlist.m3u8/thumb.png/分片）服务端按设计匿名，不在 CORE_ONLY 拒绝面里；
// 上传（POST /api/video）无流式提供者，归入 BLOCKED_WALLET_PREFIXES 显式阻塞。
// ---------------------------------------------------------------------------

export const VIDEO_ROUTES: readonly WalletRoute[] = [
  {
    method: "GET",
    pattern: "/api/video/:id",
    action: VIDEO_ACTIONS.meta,
    toParams: (match) => ({ video_id: match.params.id }),
  },
  {
    method: "GET",
    pattern: "/api/video/:id/stats",
    action: VIDEO_ACTIONS.stats,
    toParams: (match) => ({ video_id: match.params.id }),
  },
  {
    method: "POST",
    pattern: "/api/video/:id/view",
    action: VIDEO_ACTIONS.view,
    toParams: (match, body) => ({ video_id: match.params.id, viewer: body.viewer, watched_ms: body.watched_ms }),
  },
  {
    method: "GET",
    pattern: "/api/videos/feed",
    action: VIDEO_ACTIONS.feed,
    toParams: ({ query }) => {
      const params: Json = { mode: query.get("mode") ?? "hot" };
      const cursor = query.get("cursor");
      const limit = query.get("limit");
      if (cursor !== null) params.cursor = cursor;
      if (limit !== null) params.limit = Number(limit);
      return params;
    },
  },
  {
    method: "GET",
    pattern: "/api/live/:channel",
    action: VIDEO_ACTIONS.liveState,
    toParams: (match) => ({ channel: match.params.channel }),
  },
  {
    method: "POST",
    pattern: "/api/live/:channel/start",
    action: VIDEO_ACTIONS.liveStart,
    toParams: (match, body) => ({ channel: match.params.channel, recording: body.recording }),
  },
  {
    method: "POST",
    pattern: "/api/live/:channel/stop",
    action: VIDEO_ACTIONS.liveStop,
    toParams: (match) => ({ channel: match.params.channel }),
  },
  {
    method: "POST",
    pattern: "/api/live/:channel/join",
    action: VIDEO_ACTIONS.liveJoin,
    toParams: (match) => ({ channel: match.params.channel }),
  },
  {
    method: "POST",
    pattern: "/api/live/:channel/leave",
    action: VIDEO_ACTIONS.liveLeave,
    toParams: (match) => ({ channel: match.params.channel }),
  },
  {
    method: "POST",
    pattern: "/api/live/:channel/danmaku",
    action: VIDEO_ACTIONS.liveDanmaku,
    toParams: (match, body) => ({ channel: match.params.channel, text: body.text }),
  },
];

export function videoRouteCensus(): string[] {
  return VIDEO_ROUTES.map((route) => `${route.method} ${route.pattern} -> ${route.action}`);
}

// ---------------------------------------------------------------------------
// 内容路由表：moments/favorites/report/stickers 控制面。
//
// 服务端形状与 api.ts 期望形状有几处出入，投影在这里对齐（同钱包口径）：
// feed 信封 {moments}→{items}；favorites 信封 {favorites}→裸数组；packs 信封
// {packs}→裸数组；detail 的评论 reply_to→reply_to_comment_id。publish/comment
// 服务端只回 id，由 api.ts 回读补全（见 features/content/api.ts）。
// G05 未对齐面（articles/publishers）不在这张表里，归 BLOCKED 显式阻塞。
// ---------------------------------------------------------------------------

function momentCommentRow(row: unknown): Json {
  const r = asRecord(row, "comment");
  return {
    comment_id: needString(r, "comment_id"),
    author: needString(r, "author"),
    author_name: typeof r.author_name === "string" ? r.author_name : undefined,
    text: needString(r, "text"),
    reply_to_comment_id:
      typeof r.reply_to_comment_id === "string"
        ? r.reply_to_comment_id
        : typeof r.reply_to === "string" && r.reply_to !== ""
          ? r.reply_to
          : null,
    created_at: needNumber(r, "created_at"),
  };
}

export const CONTENT_ROUTES: readonly WalletRoute[] = [
  {
    method: "POST",
    pattern: "/api/moments",
    action: CONTENT_ACTIONS.momentPublish,
    toParams: (_m, body) => ({ text: body.text, media_ids: body.media_ids, visibility: body.visibility }),
    // 服务端只回 {moment_id}；api.ts 回读 detail 补全 head。
  },
  {
    method: "GET",
    pattern: "/api/moments/feed",
    action: CONTENT_ACTIONS.momentFeed,
    toParams: ({ query }) => {
      const params: Json = {};
      const since = query.get("since");
      const limit = query.get("limit");
      if (since !== null) params.since = Number(since);
      if (limit !== null) params.limit = Number(limit);
      return params;
    },
    project: (data) => {
      const row = asRecord(data, "feed");
      return { items: needArray(row, "moments"), next_cursor: optionalNumberOrNull(row, "next_cursor") };
    },
  },
  {
    method: "GET",
    pattern: "/api/moments/:id",
    action: CONTENT_ACTIONS.momentDetail,
    toParams: (m) => ({ moment_id: m.params.id }),
    project: (data) => {
      const row = asRecord(data, "detail");
      return { ...row, comments: needArray(row, "comments").map(momentCommentRow) };
    },
  },
  {
    method: "DELETE",
    pattern: "/api/moments/:id",
    action: CONTENT_ACTIONS.momentRemove,
    toParams: (m) => ({ moment_id: m.params.id }),
  },
  {
    method: "POST",
    pattern: "/api/moments/:id/likes",
    action: CONTENT_ACTIONS.momentLike,
    toParams: (m) => ({ moment_id: m.params.id }),
    // 服务端回 {liked,added}，无 like_count；api.ts 保留乐观计数。
  },
  {
    method: "DELETE",
    pattern: "/api/moments/:id/likes",
    action: CONTENT_ACTIONS.momentUnlike,
    toParams: (m) => ({ moment_id: m.params.id }),
  },
  {
    method: "POST",
    pattern: "/api/moments/:id/comments",
    action: CONTENT_ACTIONS.momentComment,
    toParams: (m, body) => ({ moment_id: m.params.id, text: body.text, reply_to_comment_id: body.reply_to_comment_id }),
    // 服务端只回 {comment_id}；api.ts 回读 detail 补全评论。
  },
  {
    method: "DELETE",
    pattern: "/api/moments/:id/comments/:cid",
    action: CONTENT_ACTIONS.momentCommentRemove,
    toParams: (m) => ({ moment_id: m.params.id, comment_id: m.params.cid }),
  },
  {
    method: "POST",
    pattern: "/api/reports",
    action: CONTENT_ACTIONS.reportCreate,
    toParams: (_m, body) => ({ target_kind: body.target_kind, target_id: body.target_id, reason: body.reason, detail: body.detail }),
  },
  {
    method: "GET",
    pattern: "/api/favorites",
    action: CONTENT_ACTIONS.favoriteList,
    toParams: () => ({}),
    project: (data) => needArray(asRecord(data, "favorites"), "favorites"),
  },
  {
    method: "POST",
    pattern: "/api/favorites",
    action: CONTENT_ACTIONS.favoriteAdd,
    toParams: (_m, body) => ({ body }),
  },
  {
    method: "GET",
    pattern: "/api/favorites/:id",
    action: CONTENT_ACTIONS.favoriteOne,
    toParams: (m) => ({ id: m.params.id }),
  },
  {
    method: "DELETE",
    pattern: "/api/favorites/:id",
    action: CONTENT_ACTIONS.favoriteRemove,
    toParams: (m) => ({ id: m.params.id }),
  },
  {
    method: "PUT",
    pattern: "/api/favorites/:id",
    action: CONTENT_ACTIONS.favoriteUpdate,
    toParams: (m, body) => ({ id: m.params.id, snapshot_label: body.snapshot_label }),
  },
  {
    method: "GET",
    pattern: "/api/stickers/packs",
    action: CONTENT_ACTIONS.stickerPacks,
    toParams: () => ({}),
    project: (data) => needArray(asRecord(data, "packs"), "packs"),
  },
  {
    method: "GET",
    pattern: "/api/media/:id/url",
    action: CONTENT_ACTIONS.momentMediaUrl,
    toParams: (m) => ({ media_id: m.params.id }),
    // 服务端回 {url, expires_at}；MomentImage 读 reply.url，原样透传。
  },
];

export function contentRouteCensus(): string[] {
  return CONTENT_ROUTES.map((route) => `${route.method} ${route.pattern} -> ${route.action}`);
}

// ---------------------------------------------------------------------------
// 文件路由表：文件中心控制面（列表/改名/分享/删除）。
//
// 前端旧路径 `/api/files*` 经此映射到 file.* action，桥打到 G04 `/api/v1/files*`。
// 服务端形状与 api.ts 期望形状有几处出入，投影在这里对齐（同钱包/内容口径）：
// 列表 FilePage 信封 {items}→裸数组，FileItem→FileRecord（file_id→media_id、
// object_id→digest 的内容地址、mime→content_type、created_at→uploaded_at）；
// 分享 ShareGrant→{url, expires_at}。二进制上传/下载（/api/media）与备份清理
// 计划（/api/backups）无 JSON 流式提供者，归 BLOCKED 显式阻塞，不在这张表里。
// ---------------------------------------------------------------------------

/** G04 FileItem → api.ts 的 FileRecord。内容地址即 digest（media_id 是 sha256）。 */
function fileRecord(row: unknown): Json {
  const r = asRecord(row, "file");
  return {
    media_id: needString(r, "file_id"),
    name: needString(r, "name"),
    size: needNumber(r, "size"),
    content_type: needString(r, "mime"),
    digest: needString(r, "digest"),
    uploaded_at: needNumber(r, "created_at"),
    visibility: "private",
  };
}

export const FILES_ROUTES: readonly WalletRoute[] = [
  {
    method: "GET",
    pattern: "/api/files",
    action: FILES_ACTIONS.list,
    toParams: ({ query }) => {
      const params: Json = {};
      const category = query.get("category");
      const sort = query.get("sort");
      const cursor = query.get("cursor");
      const limit = query.get("limit");
      if (category !== null) params.category = category;
      if (sort !== null) params.sort = sort;
      if (cursor !== null) params.cursor = cursor;
      if (limit !== null) params.limit = Number(limit);
      return params;
    },
    // 桥透传 G04 FilePage {items, next_cursor, has_more}；api.ts 读裸数组。
    project: (data) => needArray(asRecord(data, "files"), "items").map(fileRecord),
  },
  {
    method: "PATCH",
    pattern: "/api/files/:id",
    action: FILES_ACTIONS.rename,
    toParams: (m, body) => ({ file_id: m.params.id, name: body.name }),
    project: (data) => fileRecord(data),
  },
  {
    method: "POST",
    pattern: "/api/files/:id/share",
    action: FILES_ACTIONS.share,
    toParams: (m) => ({ file_id: m.params.id }),
    // 桥透传 ShareGrant {share_id, token, url, expires_at}；api.ts 读 {url, expires_at}。
    project: (data) => {
      const r = asRecord(data, "share");
      return { url: needString(r, "url"), expires_at: needNumber(r, "expires_at") };
    },
  },
  {
    method: "DELETE",
    pattern: "/api/files/:id",
    action: FILES_ACTIONS.remove,
    toParams: (m) => ({ file_id: m.params.id }),
    // 服务端 204 → 桥 {}；api.ts remove 不读返回体。
  },
];

export function filesRouteCensus(): string[] {
  return FILES_ROUTES.map((route) => `${route.method} ${route.pattern} -> ${route.action}`);
}
// ---------------------------------------------------------------------------
// 发现路由表：服务目录控制面（目录/报价/下单/查单/列举/退款）。
//
// 前端旧路径 /api/services、/api/quotes、/api/orders* 经此映射到 discover.*
// action，桥打到 G07 服务目录服务端。服务端已把内部模型投影成前端契约形状
// （ServiceEntry/Quote/ServiceOrder），这里只做信封拆包：services 与 orders 的
// {items}→裸数组。识图/翻译（/api/vision）无核心提供者，归 BLOCKED 显式阻塞。
// ---------------------------------------------------------------------------

export const DISCOVER_ROUTES: readonly WalletRoute[] = [
  {
    method: "GET",
    pattern: "/api/services",
    action: DISCOVER_ACTIONS.services,
    toParams: () => ({}),
    project: (data) => needArray(asRecord(data, "services"), "items"),
  },
  {
    method: "GET",
    pattern: "/api/quotes",
    action: DISCOVER_ACTIONS.quote,
    toParams: ({ query }) => ({ service_id: query.get("service_id") }),
    // 服务端已投影成 {quote_id, service_id, price_minor, currency, expires_at}。
  },
  {
    method: "POST",
    pattern: "/api/orders",
    action: DISCOVER_ACTIONS.orderCreate,
    toParams: (_m, body) => ({ service_id: body.service_id, quote_id: body.quote_id }),
    // 服务端已投影成 ServiceOrder。
  },
  {
    method: "GET",
    pattern: "/api/orders/:id",
    action: DISCOVER_ACTIONS.orderGet,
    toParams: (m) => ({ order_id: m.params.id }),
  },
  {
    method: "GET",
    pattern: "/api/orders",
    action: DISCOVER_ACTIONS.orders,
    toParams: () => ({}),
    project: (data) => needArray(asRecord(data, "orders"), "items"),
  },
  {
    method: "POST",
    pattern: "/api/orders/:id/refund",
    action: DISCOVER_ACTIONS.orderRefund,
    toParams: (m) => ({ order_id: m.params.id }),
  },
];

export function discoverRouteCensus(): string[] {
  return DISCOVER_ROUTES.map((route) => `${route.method} ${route.pattern} -> ${route.action}`);
}

/**
 * 已迁移域里**明确没有 provider** 的出口：登记在此，运行时一律 `blocked`（501），
 * 静态检查据此确认"钱包域的每条路径都有归属"——要么进 `WALLET_ROUTES` 走核心，
 * 要么进这张表显式阻塞。两条都不在就是"偷偷回落到旧 HTTP"，判红。
 *
 * 这些都是 G06 CommercePort 的契约面（`features/commerce/api.ts` 还在按服务端 REST
 * 形状调用它们）。#293 没有交付这些 action，所以这里不猜、不代打、不返回空数组。
 */
export const BLOCKED_WALLET_PREFIXES: readonly string[] = [
  "/api/payment-intents",
  "/api/payments",
  "/api/withdrawals",
  "/api/topups",
  "/api/cards",
  "/api/entitlements",
  // 视频上传（multipart 二进制）：无流式提供者，显式阻塞，不回落旧会话。
  "/api/video",
  // 内容域 G05 契约面：服务端尚未对齐（articles 列表/publisher 订阅/article
  // watching），显式阻塞，不回落旧会话、不伪造成功。
  "/api/articles",
  "/api/publishers",
  // 文件域二进制面（multipart 上传 / Range 下载）：无 JSON 流式核心提供者，
  // 显式阻塞（同视频上传口径）。媒体签名 URL（/api/media/:id/url）是内容域
  // 认证读面，由 CONTENT_ROUTES 精确路由先命中，不受这条粗前缀影响。
  "/api/media",
  // 文件域备份/清理计划：服务端导出包计划与前端清理契约未对齐，显式阻塞。
  "/api/backups",
  // 发现域识图/翻译（multipart + 外部 AI）：无核心提供者，显式阻塞。
  "/api/vision",
];

/** 路径是否落在某条路由模板上（`:x` 段通配；模板末尾可再带子段）。 */
function matchesPattern(bare: string, pattern: string): boolean {
  const parts = bare.split("/").filter(Boolean);
  const template = pattern.split("/").filter(Boolean);
  if (parts.length < template.length) return false;
  return template.every((token, i) => token.startsWith(":") || token === parts[i]);
}

/** 这个已迁移域路径是否有明确归属（路由 or 显式阻塞）。
 *
 * 先查精确路由模板、再查阻塞前缀：媒体签名 URL（`/api/media/:id/url`，内容域
 * 路由）必须先于文件域的 `/api/media` 阻塞前缀命中，否则会被粗前缀误判成阻塞。 */
export function isRoutedOrBlocked(path: string): boolean {
  const bare = path.split("?")[0] ?? path;
  if (
    [...WALLET_ROUTES, ...CALL_ROUTES, ...VIDEO_ROUTES, ...CONTENT_ROUTES, ...FILES_ROUTES, ...DISCOVER_ROUTES].some((route) =>
      matchesPattern(bare, route.pattern),
    )
  ) {
    return true;
  }
  return BLOCKED_WALLET_PREFIXES.some((prefix) => bare === prefix || bare.startsWith(`${prefix}/`));
}

/** 路径（去掉 query）+ 方法 → 指定路由表里的条目；不匹配返回 null。 */
function matchRoute(
  routes: readonly WalletRoute[],
  method: string,
  path: string,
): { route: WalletRoute; match: Match } | null {
  const [bare = "", text = ""] = path.split("?");
  const parts = bare.split("/").filter(Boolean);
  for (const route of routes) {
    if (route.method !== method) continue;
    const template = route.pattern.split("/").filter(Boolean);
    if (template.length !== parts.length) continue;
    const params: Record<string, string> = {};
    let ok = true;
    for (let i = 0; i < template.length; i += 1) {
      const token = template[i] as string;
      const actual = parts[i] as string;
      if (token.startsWith(":")) params[token.slice(1)] = decodeURIComponent(actual);
      else if (token !== actual) {
        ok = false;
        break;
      }
    }
    if (ok) return { route, match: { params, query: new URLSearchParams(text) } };
  }
  return null;
}

/** 已迁移域的路由表（按域顺序匹配；各域控制面 URL 模板互不重叠）。 */
const CORE_ROUTES: readonly WalletRoute[] = [...WALLET_ROUTES, ...CALL_ROUTES, ...VIDEO_ROUTES, ...CONTENT_ROUTES, ...FILES_ROUTES, ...DISCOVER_ROUTES];

// ---------------------------------------------------------------------------
// 投影用到的严格读数（缺字段就抛，不兜默认值）
// ---------------------------------------------------------------------------

function asRecord(value: unknown, field: string): Json {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new ApiError(502, `核心响应里 ${field} 不是对象（不猜形状）`, field, "server");
  }
  return value as Json;
}

function needNumber(value: unknown, key: string): number {
  const row = asRecord(value, key);
  const raw = row[key];
  if (typeof raw !== "number" || !Number.isSafeInteger(raw)) {
    throw new ApiError(502, `核心响应缺 ${key}（整数分），界面不编造金额`, key, "server");
  }
  return raw;
}

function optionalNumberOrNull(value: unknown, key: string): number | null {
  const raw = asRecord(value, key)[key];
  if (raw === null || raw === undefined) return null;
  if (typeof raw !== "number" || !Number.isSafeInteger(raw)) {
    throw new ApiError(502, `核心响应的 ${key} 不是整数游标，界面不编造分页位置`, key, "server");
  }
  return raw;
}

function needString(value: unknown, key: string): string {
  const raw = asRecord(value, key)[key];
  if (typeof raw !== "string") {
    throw new ApiError(502, `核心响应缺 ${key}，界面不编造标识`, key, "server");
  }
  return raw;
}

function needBoolean(value: unknown, key: string): boolean {
  const raw = asRecord(value, key)[key];
  if (typeof raw !== "boolean") {
    throw new ApiError(502, `核心响应缺 ${key}，界面不猜分页状态`, key, "server");
  }
  return raw;
}

function needOptStringOrNull(value: unknown, key: string): string | null {
  const raw = asRecord(value, key)[key];
  if (raw === null || raw === undefined) return null;
  if (typeof raw !== "string") {
    throw new ApiError(502, `核心响应的 ${key} 不是字符串`, key, "server");
  }
  return raw;
}

function needArray(value: unknown, key: string): unknown[] {
  const raw = asRecord(value, key)[key];
  if (!Array.isArray(raw)) {
    throw new ApiError(502, `核心响应缺 ${key} 数组`, key, "server");
  }
  return raw;
}

function needIsoMs(value: unknown, key: string): number {
  const text = needString(value, key);
  const ms = Date.parse(text);
  if (Number.isNaN(ms)) {
    throw new ApiError(502, `核心响应的 ${key} 不是可解析的时间（${text}）`, key, "server");
  }
  return ms;
}

/**
 * kind → 记账方向。**这份名单必须与 `crates/platform/src/domain/wallet.rs` 的
 * `mod kind` 一致**（`core-transport.test.ts` 直接读那个 Rust 源文件比对，防止两边各写一套）。
 * 桥刻意不给金额补符号（方向是服务端口径），界面要显示 `+/-` 就只能按 kind 判，
 * 判不出来就报错 —— 认不出的 kind 兜成"收入"是把账做错。
 */
const LEDGER_CREDIT = new Set(["transfer_in", "redpacket_in", "redpacket_refund", "fund_in"]);
const LEDGER_DEBIT = new Set(["transfer_out", "redpacket_out"]);

export const LEDGER_KINDS = [...LEDGER_CREDIT, ...LEDGER_DEBIT].sort();

function ledgerRow(row: unknown): Json {
  const source = asRecord(row, "entry");
  const kind = needString(source, "kind");
  const amount = needNumber(source, "amount_minor");
  const signed = LEDGER_CREDIT.has(kind) ? amount : LEDGER_DEBIT.has(kind) ? -amount : null;
  if (signed === null) {
    throw new ApiError(502, `账本出现未知 kind="${kind}"：方向由服务端决定，界面不猜`, "wallet.ledger", "blocked");
  }
  return {
    id: needNumber(source, "entry_id"),
    kind,
    amount: signed,
    counterparty: needString(source, "counterparty"),
    ref_id: needString(source, "ref_id"),
    memo: needString(source, "memo"),
    created_at: needIsoMs(source, "created_at_ms"),
  };
}

function redpacketRecord(data: unknown): Json {
  const row = asRecord(data, "redpacket");
  const record: Json = {
    redpacket_id: needString(row, "redpacket_id"),
    total_minor: needNumber(row, "total_minor"),
    count: needNumber(row, "count"),
    remaining: needNumber(row, "remaining"),
    memo: needOptStringOrNull(row, "memo"),
    expires_at: needIsoMs(row, "expire_at_ms"),
    status: needString(row, "status"),
  };
  // claimed_by_me 需要"当前身份"，桥没给身份口径，界面就不自己判（不编造）。
  return record;
}

// ---------------------------------------------------------------------------
// 错误映射与 transport
// ---------------------------------------------------------------------------

/** 桥的 ErrorCode → 遗留 ApiError：一一对应，绝不把失败折成"空但成功"。 */
export function toApiError(error: DomainError, path: string): ApiError {
  const table: Record<DomainError["code"], { status: number; code: ApiErrorCode }> = {
    UNAUTHENTICATED: { status: 401, code: "unauthorized" },
    FORBIDDEN: { status: 403, code: "forbidden" },
    VALIDATION: { status: 400, code: "bad_request" },
    CONFLICT: { status: 409, code: "conflict" },
    RATE_LIMITED: { status: 429, code: "rate_limited" },
    REQUIRES_CONFIRMATION: { status: 409, code: "conflict" },
    DEPENDENCY_UNAVAILABLE: { status: 503, code: "server" },
    // NOT_IMPLEMENTED = 服务端/桥确实没有这条能力：501 且 code=blocked，
    // 业务侧 isUpstreamMissing() 认 501，于是显示"上游未开通"而不是假数据。
    NOT_IMPLEMENTED: { status: 501, code: "blocked" },
    OFFLINE: { status: 0, code: "network" },
    CANCELLED: { status: 0, code: "cancelled" },
    INTERNAL: { status: 0, code: "server" },
  };
  const mapped = table[error.code] ?? { status: 0, code: "server" as ApiErrorCode };
  return new ApiError(mapped.status, error.message || `核心返回 ${error.code}`, path, mapped.code);
}

function staleError(path: string, generation: number): ApiError {
  return new ApiError(
    0,
    `会话已切换（请求属于第 ${generation} 代，当前第 ${session.generation} 代）：结果按旧用户任务丢弃，不写入新会话`,
    path,
    "cancelled",
  );
}

/**
 * 装上后的默认出口。未迁移的域原样转给遗留 `fetchTransport`（它们的阻塞状态由
 * `app.tsx` 的组合根横幅表达，迁移由各自 provider 卡负责），已迁移域只有核心一条路。
 */
function coreRoutingTransport(): Transport {
  return async (method, path, opts = {}) => {
    if (!isCoreOnlyPath(path)) return fetchTransport(method, path, opts);

    const core = session.core;
    if (!core) {
      throw blockedError(path, "组合根尚未装上 Rust 核心会话");
    }
    const hit = matchRoute(CORE_ROUTES, method, path);
    if (!hit) {
      throw blockedError(
        path,
        `${method} ${path} 在核心桥里没有对应 action（该能力没有 provider，不允许回落到遗留 HTTP）`,
      );
    }
    const { route, match } = hit;
    const body = (opts.body ?? {}) as Json;
    const request = newRequest(route.action, route.toParams(match, body), opts.idempotencyKey);
    const generation = session.generation;
    session.pending.add(request.request_id);

    if (opts.signal?.aborted) {
      session.pending.delete(request.request_id);
      throw new ApiError(0, "已取消", path, "cancelled");
    }

    try {
      // 调用方给了 signal 就一定要尊重：桥/IPC 这层没有撤销语义（ActionRequest 不带
      // signal），所以取消表现成"结果被丢弃"，而不是"服务端那笔没发生"。绝不把取消
      // 折成一次成功，也不把半截数据交回去。
      const dispatch = core.dispatch<unknown>(request);
      const response = opts.signal ? await raceAbort(dispatch, opts.signal, path) : await dispatch;
      if (generation !== session.generation) throw staleError(path, generation);
      if (!response.ok) throw toApiError(response.error, path);
      return route.project ? route.project(response.data) : response.data;
    } catch (err) {
      if (err instanceof ApiError) throw err;
      throw new ApiError(0, `核心不可达: ${err instanceof Error ? err.message : String(err)}`, path, "network");
    } finally {
      session.pending.delete(request.request_id);
    }
  };
}

/** signal 触发时让调用方的 Promise 先失败；核心那条 dispatch 继续跑完但不落地。 */
function raceAbort<T>(promise: Promise<T>, signal: AbortSignal, path: string): Promise<T> {
  if (signal.aborted) return Promise.reject(new ApiError(0, "已取消", path, "cancelled"));
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(new ApiError(0, "已取消（核心结果稍后到达，按旧请求丢弃）", path, "cancelled"));
    signal.addEventListener("abort", onAbort, { once: true });
    promise
      .then((value) => {
        signal.removeEventListener("abort", onAbort);
        resolve(value);
      })
      .catch((err) => {
        signal.removeEventListener("abort", onAbort);
        reject(err instanceof Error ? err : new Error(String(err)));
      });
  });
}
