import {
  ACTIONS,
  domainError,
  fail,
  newIdempotencyKey,
  ok,
  type ActionRequest,
  type ActionResponse,
  type CoreCapabilities,
  type CoreHandle,
  type DomainError,
  type EventCursor,
  type EventEnvelope,
  type Unsubscribe,
} from "../../core-contract";
import type {
  AppInfo,
  BackupInfo,
  ChatMessage,
  Contact,
  Conversation,
  FriendRequest,
  GroupMember,
  MessageBody,
  Page,
  Preferences,
  QrChallenge,
  QrState,
  RelationProfile,
  SearchCategory,
  SearchHit,
  SearchResults,
  SessionProjection,
  StorageStatus,
} from "../../models";
import {
  FIXTURE_DIRECTORY,
  createAlphaAccount,
  createBetaAccount,
  type FixtureAccount,
} from "./fixture-data";

export interface FixtureOptions {
  autoAdvance?: boolean;
  latencyMs?: number;
}

export interface FixtureCore extends CoreHandle {
  readonly transport: "fixture";
  /** Test/dev hooks. There is no production action that can mint a session from the desktop. */
  simulateQr(state: Extract<QrState, "SCANNED" | "WAITING_APPROVAL" | "APPROVED" | "EXPIRED" | "REJECTED">): void;
  setOffline(offline: boolean): void;
  currentSession(): SessionProjection;
  reset(): void;
}

const MAX_UPLOAD_BYTES = 1024 * 1024 * 1024;

function digest(input: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i += 1) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash.toString(16).padStart(8, "0");
}

function encodeUtf8(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

function toBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function fromBase64(value: string): string {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return new TextDecoder().decode(bytes);
}

function previewOf(body: MessageBody): string {
  switch (body.kind) {
    case "text":
      return body.text ?? "";
    case "image":
      return "[图片]";
    case "voice":
      return `[语音] ${Math.round((body.duration_ms ?? 0) / 1000)}″`;
    case "file":
      return `[文件] ${body.name ?? ""}`;
    case "sticker":
      return body.sticker ?? "[表情]";
    case "link":
      return body.text ?? "[链接]";
    case "system":
      return body.text ?? "";
  }
}

function emptyError(code: DomainError["code"], message: string, requestId: string): ActionResponse<never> {
  return fail(domainError(code, message, requestId));
}

export function createFixtureCore(options: FixtureOptions = {}): FixtureCore {
  const autoAdvance = options.autoAdvance ?? true;
  const latency = options.latencyMs ?? 0;
  const accounts = new Map<string, FixtureAccount>([
    ["user-alpha", createAlphaAccount()],
    ["user-beta", createBetaAccount()],
  ]);
  let session: SessionProjection = { phase: "signed_out" };
  let challenge: QrChallenge | null = null;
  let offline = false;
  let eventSeq = 0;
  let messageSeq = 100;
  let requestSeq = 100;
  const listeners = new Set<(event: EventEnvelope) => void>();
  const idempotency = new Map<string, { action_id: string; digest: string; response: ActionResponse<unknown> }>();
  const uploads = new Map<string, { name: string; size: number; mime: string; received: number }>();
  const timers = new Set<ReturnType<typeof setTimeout>>();
  let secretsLocked = false;

  function later(fn: () => void, ms: number): void {
    const timer = setTimeout(() => {
      timers.delete(timer);
      fn();
    }, ms);
    timers.add(timer);
  }

  function emit(event_type: string, aggregate_id: string, payload: unknown): void {
    eventSeq += 1;
    const envelope: EventEnvelope = {
      event_id: `fixture-evt-${eventSeq}`,
      aggregate_id,
      aggregate_version: String(eventSeq),
      event_type,
      server_time_ms: new Date().toISOString(),
      payload,
    };
    for (const listener of listeners) listener(envelope);
  }

  function account(): FixtureAccount | null {
    if (session.phase === "authenticated" || session.phase === "locked") {
      const found = accounts.get(session.account?.user_id ?? "");
      return found ?? null;
    }
    return null;
  }

  function authenticate(userId: string): SessionProjection {
    const found = accounts.get(userId);
    if (!found) return { phase: "signed_out" };
    session = {
      phase: "authenticated",
      account: {
        user_id: found.user_id,
        name: found.name,
        avatar: found.avatar,
        device_id: "device-fixture",
        session_id: `session-${found.user_id}`,
      },
    };
    emit("session.state", found.user_id, session);
    return session;
  }

  function convOf(user: FixtureAccount, convId: string): Conversation {
    const conv = user.conversations.find((item) => item.conv_id === convId);
    if (!conv) throw new Error(`conversation ${convId} not found`);
    return conv;
  }

  function nextSeq(user: FixtureAccount, convId: string): string {
    const rows = user.messages[convId] ?? [];
    const max = rows.reduce((acc, msg) => Math.max(acc, Number(msg.seq)), 0);
    return String(max + 1);
  }

  function upsertConversation(user: FixtureAccount, conv: Conversation): void {
    const index = user.conversations.findIndex((item) => item.conv_id === conv.conv_id);
    if (index >= 0) user.conversations[index] = conv;
    else user.conversations.unshift(conv);
    emit("conversation.upsert", conv.conv_id, conv);
    emit("unread.updated", user.user_id, {
      total: user.conversations.reduce((sum, item) => sum + item.unread, 0),
    });
  }

  function addMessage(user: FixtureAccount, msg: ChatMessage): void {
    user.messages[msg.conv_id] = [...(user.messages[msg.conv_id] ?? []), msg];
    const conv = convOf(user, msg.conv_id);
    upsertConversation(user, {
      ...conv,
      last_preview: previewOf(msg.body),
      last_at_ms: msg.created_at_ms,
    });
    emit("message.created", msg.conv_id, msg);
  }

  function updateMessage(user: FixtureAccount, msg: ChatMessage): void {
    const rows = user.messages[msg.conv_id] ?? [];
    user.messages[msg.conv_id] = rows.map((item) => (item.msg_id === msg.msg_id ? msg : item));
    emit("message.updated", msg.conv_id, msg);
  }

  function advanceSend(user: FixtureAccount, msgId: string): void {
    if (!autoAdvance) return;
    const [convKey, realId] = msgId.split(":");
    const find = () => (user.messages[convKey] ?? []).find((m) => m.msg_id === realId);
    later(() => {
      const found = find();
      if (!found || found.state !== "LOCAL_QUEUED") return;
      updateMessage(user, { ...found, state: "SENDING" });
      later(() => {
        const sending = find();
        if (!sending || sending.state !== "SENDING") return;
        if (offline) {
          updateMessage(user, {
            ...sending,
            state: "FAILED_RETRYABLE",
            error: { code: "OFFLINE", message: "网络已断开，消息在本地队列", retryable: true },
          });
          return;
        }
        updateMessage(user, { ...sending, state: "SERVER_ACCEPTED" });
        later(() => {
          const accepted = find();
          if (!accepted || accepted.state !== "SERVER_ACCEPTED") return;
          updateMessage(user, { ...accepted, state: "DEVICE_STORED" });
        }, 40);
      }, 30);
    }, 20);
  }

  function contactById(user: FixtureAccount, userId: string): Contact | undefined {
    return user.contacts.find((item) => item.user_id === userId);
  }

  function directoryById(userId: string) {
    return FIXTURE_DIRECTORY.find((item) => item.user_id === userId);
  }

  function ensureContact(user: FixtureAccount, userId: string): Contact {
    const existing = contactById(user, userId);
    if (existing) return existing;
    const directory = directoryById(userId);
    if (!directory) throw new Error(`unknown user ${userId}`);
    const created: Contact = {
      user_id: directory.user_id,
      name: directory.name,
      region: directory.region,
      note: directory.note,
      avatar: "",
      tags: [],
      star: false,
      online: false,
      blocked: false,
    };
    user.contacts = [...user.contacts, created];
    emit("contact.updated", created.user_id, created);
    return created;
  }

  function profileOf(user: FixtureAccount, userId: string): RelationProfile | null {
    const contact = contactById(user, userId);
    const directory = directoryById(userId);
    const base = contact ?? directory;
    if (!base) return null;
    const common = user.groups
      .filter((group) => group.members.some((member) => member.user_id === userId))
      .map((group) => ({ group_id: group.group_id, name: group.name }));
    return {
      user_id: userId,
      name: base.name,
      avatar: "avatar" in base ? base.avatar ?? "" : "",
      note: contact?.note ?? ("note" in base ? base.note : undefined),
      region: base.region,
      signature: undefined,
      is_friend: Boolean(contact),
      blocked: contact?.blocked ?? false,
      common_groups: common,
    };
  }

  function searchHits(user: FixtureAccount, query: string, category: SearchCategory): SearchResults {
    const q = query.trim().toLowerCase();
    const hits: SearchHit[] = [];
    const matches = (value: string | undefined) => Boolean(value && value.toLowerCase().includes(q));

    if (q && (category === "all" || category === "chats")) {
      for (const conv of user.conversations) {
        if (matches(conv.title) || matches(conv.last_preview)) {
          hits.push({
            id: `chat-${conv.conv_id}`,
            kind: "chat",
            title: conv.title,
            subtitle: conv.last_preview,
            conv_id: conv.conv_id,
          });
        }
      }
      for (const [convId, rows] of Object.entries(user.messages)) {
        for (const msg of rows) {
          if (msg.body.kind === "text" && matches(msg.body.text)) {
            hits.push({
              id: `msg-${msg.msg_id}`,
              kind: "chat",
              title: convOf(user, convId).title,
              subtitle: previewOf(msg.body),
              conv_id: convId,
              snippet: msg.body.text,
            });
          }
        }
      }
    }
    if (q && (category === "all" || category === "contacts")) {
      for (const contact of user.contacts) {
        if (contact.blocked) continue;
        if (matches(contact.name) || matches(contact.note) || contact.tags.some(matches)) {
          hits.push({
            id: `contact-${contact.user_id}`,
            kind: "contact",
            title: contact.name,
            subtitle: contact.note ?? contact.region ?? "",
            user_id: contact.user_id,
          });
        }
      }
    }
    if (q && (category === "all" || category === "groups")) {
      for (const group of user.groups) {
        if (matches(group.name)) {
          hits.push({
            id: `group-${group.group_id}`,
            kind: "group",
            title: group.name,
            subtitle: `${group.members.length} 位成员`,
            group_id: group.group_id,
            conv_id: group.group_id === "group-spring" ? "conv-spring" : undefined,
          });
        }
      }
    }

    const unavailable =
      category === "content" || category === "all"
        ? [
            {
              category: "content" as const,
              reason: "内容/朋友圈搜索由 G05 提供，尚未接入（G05-05），不把静态数据当搜索结果",
            },
          ]
        : undefined;
    return { category, hits, unavailable };
  }

  async function handle(request: ActionRequest): Promise<ActionResponse<unknown>> {
    const params = (request.params ?? {}) as Record<string, unknown>;
    const requestId = request.request_id;
    const actionId = request.action_id;

    // messageSend is accepted into the local outbox even while offline; the
    // state machine moves it to FAILED_RETRYABLE instead of rejecting at the UI.
    if (offline && request.action_id !== ACTIONS.sessionProjection && request.action_id !== ACTIONS.messageSend) {
      return emptyError("OFFLINE", "网络已断开", requestId);
    }

    switch (request.action_id) {
      case ACTIONS.sessionProjection:
        return ok(session);

      case ACTIONS.login: {
        const username = String(params.username ?? "").trim();
        const password = String(params.password ?? "");
        const field_errors: Record<string, string> = {};
        if (!username) field_errors.username = "请输入账号";
        if (!password) field_errors.password = "请输入口令";
        if (Object.keys(field_errors).length) {
          return fail(
            domainError("VALIDATION", "账号或口令为空", requestId, { field_errors }),
          );
        }
        const found = [...accounts.values()].find(
          (item) => item.user_id === username || item.name === username,
        );
        if (!found || found.password !== password) {
          return emptyError("UNAUTHENTICATED", "账号或口令不正确", requestId);
        }
        return ok(authenticate(found.user_id));
      }

      case ACTIONS.logout: {
        const user = account();
        session = { phase: "signed_out" };
        challenge = null;
        secretsLocked = false;
        emit("session.state", user?.user_id ?? "session", session);
        if (user) emit("search.invalidated", user.user_id, {});
        return ok({ revoked: true });
      }

      case ACTIONS.lock: {
        if (!account()) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        session = { phase: "locked", account: session.account, lock_reason: "manual" };
        secretsLocked = true;
        emit("session.state", session.account?.user_id ?? "session", session);
        return ok(session);
      }

      case ACTIONS.unlock: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "会话已过期", requestId);
        const password = String(params.password ?? "");
        if (password !== user.password) {
          return emptyError("UNAUTHENTICATED", "口令不正确", requestId);
        }
        session = { phase: "authenticated", account: session.account };
        secretsLocked = false;
        emit("session.state", user.user_id, session);
        return ok(session);
      }

      case ACTIONS.restore: {
        if (!account()) return emptyError("UNAUTHENTICATED", "没有可恢复的会话", requestId);
        session = { phase: "authenticated", account: session.account };
        emit("session.state", session.account?.user_id ?? "session", session);
        return ok(session);
      }

      case ACTIONS.qrCreate: {
        if (challenge && challenge.state !== "EXPIRED" && challenge.state !== "REJECTED") {
          return ok(challenge);
        }
        const challengeId = `qr-${Date.now().toString(36)}`;
        const payload = `wxlike://login/${challengeId}`;
        challenge = {
          challenge_id: challengeId,
          state: "CREATED",
          payload,
          qr_svg: `<svg xmlns="http://www.w3.org/2000/svg" width="220" height="220" role="img" aria-label="扫码登录（fixture 占位，非真实二维码）"><rect width="220" height="220" fill="#fff"/><rect x="20" y="20" width="60" height="60" fill="#111"/><rect x="140" y="20" width="60" height="60" fill="#111"/><rect x="20" y="140" width="60" height="60" fill="#111"/><text x="110" y="115" text-anchor="middle" font-size="12" fill="#666">FIXTURE</text></svg>`,
          expires_at_ms: new Date(Date.now() + 120_000).toISOString(),
          account_hint: typeof params.account_hint === "string" ? params.account_hint : undefined,
        };
        emit("qr.state", challengeId, challenge);
        return ok(challenge);
      }

      case ACTIONS.qrCancel: {
        if (!challenge) return ok({ cancelled: true });
        challenge = { ...challenge, state: "REJECTED" };
        emit("qr.state", challenge.challenge_id, challenge);
        return ok(challenge);
      }

      case ACTIONS.qrSwitchAccount: {
        if (challenge) {
          challenge = { ...challenge, state: "REJECTED" };
          emit("qr.state", challenge.challenge_id, challenge);
        }
        challenge = null;
        session = { phase: "signed_out" };
        emit("session.state", "session", session);
        return ok({ revoked: true });
      }

      case ACTIONS.syncRangeGet: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        return ok({
          days: user.preferences.sync_range_days,
          incremental_default_days: 7,
          can_view_further: user.preferences.sync_range_days !== 7,
        });
      }

      case ACTIONS.syncRangeSet: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const days = Number(params.days);
        if (![0, 7, 30, 90].includes(days)) {
          return fail(domainError("VALIDATION", "同步范围不合法", requestId, { field_errors: { days: "只能是 7/30/90/全部" } }));
        }
        user.preferences = { ...user.preferences, sync_range_days: days as Preferences["sync_range_days"] };
        emit("preferences.updated", user.user_id, user.preferences);
        return ok(user.preferences);
      }

      case ACTIONS.conversationsList: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const filter = String(params.filter ?? "all");
        const query = String(params.query ?? "").trim().toLowerCase();
        const limit = Math.min(Number(params.limit ?? 30), 100);
        const offset = Number(params.cursor ?? 0);
        let rows = [...user.conversations];
        if (filter === "unread") rows = rows.filter((conv) => conv.unread > 0);
        if (filter === "groups") rows = rows.filter((conv) => conv.kind === "group");
        if (query) {
          rows = rows.filter(
            (conv) =>
              conv.title.toLowerCase().includes(query) ||
              conv.last_preview.toLowerCase().includes(query),
          );
        }
        rows.sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.last_at_ms.localeCompare(a.last_at_ms));
        const items = rows.slice(offset, offset + limit);
        const next = offset + limit < rows.length ? String(offset + limit) : null;
        return ok<Page<Conversation>>({ items, next_cursor: next, has_more: next !== null });
      }

      case ACTIONS.messagesPage: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const convId = String(params.conv_id ?? "");
        if (!user.conversations.some((conv) => conv.conv_id === convId)) {
          return emptyError("FORBIDDEN", "不在该会话中", requestId);
        }
        const limit = Math.min(Number(params.limit ?? 30), 100);
        const all = [...(user.messages[convId] ?? [])].sort((a, b) => Number(a.seq) - Number(b.seq));
        const before = params.cursor ? Number(params.cursor) : null;
        const eligible = before === null ? all : all.filter((msg) => Number(msg.seq) < before);
        const items = eligible.slice(Math.max(0, eligible.length - limit));
        const hasMore = eligible.length > items.length;
        return ok<Page<ChatMessage>>({
          items,
          next_cursor: hasMore ? items[0]?.seq ?? null : null,
          has_more: hasMore,
        });
      }

      case ACTIONS.messageSend: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const convId = String(params.conv_id ?? "");
        const body = params.body as MessageBody | undefined;
        const field_errors: Record<string, string> = {};
        if (!user.conversations.some((conv) => conv.conv_id === convId)) field_errors.conv_id = "会话不存在";
        if (!body || typeof body !== "object") field_errors.body = "消息内容为空";
        if (body?.kind === "text" && !body.text?.trim()) field_errors.body = "不能发送空消息";
        if (body?.kind === "file" && (!body.media_id || !body.name)) field_errors.body = "文件尚未上传完成";
        if (Object.keys(field_errors).length) {
          return fail(domainError("VALIDATION", "消息参数不合法", requestId, { field_errors }));
        }
        const clientMsgId = String(params.client_msg_id ?? newIdempotencyKey("msg"));
        const msgId = `msg-${messageSeq++}`;
        const now = new Date().toISOString();
        const msg: ChatMessage = {
          msg_id: msgId,
          conv_id: convId,
          seq: nextSeq(user, convId),
          sender: { user_id: user.user_id, name: user.name, avatar: user.avatar },
          direction: "out",
          body: body!,
          state: "LOCAL_QUEUED",
          created_at_ms: now,
          client_msg_id: clientMsgId,
        };
        addMessage(user, msg);
        advanceSend(user, `${convId}:${msgId}`);
        return ok({ msg_id: msgId, client_msg_id: clientMsgId, state: "LOCAL_QUEUED" as const });
      }

      case ACTIONS.messageRetry: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const msgId = String(params.msg_id ?? "");
        const convId = String(params.conv_id ?? "");
        const msg = (user.messages[convId] ?? []).find((item) => item.msg_id === msgId);
        if (!msg) return emptyError("VALIDATION", "消息不存在", requestId);
        if (msg.direction !== "out") return emptyError("FORBIDDEN", "只能重试自己发送的消息", requestId);
        if (msg.state !== "FAILED_RETRYABLE") {
          return emptyError("CONFLICT", "当前状态不可重试", requestId);
        }
        const next: ChatMessage = { ...msg, state: "LOCAL_QUEUED", error: undefined };
        updateMessage(user, next);
        advanceSend(user, `${convId}:${msgId}`);
        return ok({ msg_id: msgId, state: "LOCAL_QUEUED" as const });
      }

      case ACTIONS.messageRecall: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const convId = String(params.conv_id ?? "");
        const msgId = String(params.msg_id ?? "");
        const msg = (user.messages[convId] ?? []).find((item) => item.msg_id === msgId);
        if (!msg) return emptyError("VALIDATION", "消息不存在", requestId);
        if (msg.direction !== "out") return emptyError("FORBIDDEN", "只能撤回自己发送的消息", requestId);
        const age = Date.now() - new Date(msg.created_at_ms).getTime();
        if (age > 120_000) return emptyError("FORBIDDEN", "已超过可撤回时间", requestId);
        updateMessage(user, { ...msg, recalled: true, state: "RECALLED", body: { kind: "system", text: "你撤回了一条消息" } });
        emit("search.invalidated", user.user_id, {});
        return ok({ recalled: true });
      }

      case ACTIONS.messageForward: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const msgIds = Array.isArray(params.msg_ids) ? (params.msg_ids as string[]) : [];
        const targets = Array.isArray(params.target_conv_ids) ? (params.target_conv_ids as string[]) : [];
        if (!msgIds.length) return fail(domainError("VALIDATION", "请选择要转发的消息", requestId));
        if (!targets.length) return fail(domainError("VALIDATION", "请选择转发目标", requestId));
        const sourceRows = Object.values(user.messages).flat();
        const sources = msgIds.map((id) => sourceRows.find((msg) => msg.msg_id === id));
        if (sources.some((msg) => !msg)) return emptyError("VALIDATION", "部分消息不存在", requestId);
        let created = 0;
        for (const target of targets) {
          if (!user.conversations.some((conv) => conv.conv_id === target)) {
            return emptyError("FORBIDDEN", "没有转发到该会话的权限", requestId);
          }
          for (const source of sources) {
            const msg: ChatMessage = {
              msg_id: `msg-${messageSeq++}`,
              conv_id: target,
              seq: nextSeq(user, target),
              sender: { user_id: user.user_id, name: user.name, avatar: user.avatar },
              direction: "out",
              body: { ...source!.body },
              state: "LOCAL_QUEUED",
              created_at_ms: new Date().toISOString(),
            };
            addMessage(user, msg);
            advanceSend(user, `${target}:${msg.msg_id}`);
            created += 1;
          }
        }
        return ok({ created });
      }

      case ACTIONS.messageRead: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const conv = convOf(user, String(params.conv_id ?? ""));
        upsertConversation(user, { ...conv, unread: 0 });
        return ok({ unread: 0 });
      }

      case ACTIONS.conversationPin:
      case ACTIONS.conversationMute: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const conv = convOf(user, String(params.conv_id ?? ""));
        const value = Boolean(params.value);
        const updated =
          request.action_id === ACTIONS.conversationPin
            ? { ...conv, pinned: value }
            : { ...conv, muted: value };
        upsertConversation(user, updated);
        return ok(updated);
      }

      case ACTIONS.conversationReadAll: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        user.conversations = user.conversations.map((conv) => ({ ...conv, unread: 0 }));
        for (const conv of user.conversations) emit("conversation.upsert", conv.conv_id, conv);
        emit("unread.updated", user.user_id, { total: 0 });
        return ok({ total: 0 });
      }

      case ACTIONS.conversationEnsure: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const peer = String(params.user_id ?? "");
        const existing = user.conversations.find((conv) => conv.peer_user_id === peer);
        if (existing) return ok(existing);
        const contact = contactById(user, peer);
        if (!contact) return emptyError("FORBIDDEN", "不是好友，不能发起会话", requestId);
        const conv: Conversation = {
          conv_id: `conv-${peer}`,
          kind: "direct",
          title: contact.name,
          peer_user_id: peer,
          unread: 0,
          pinned: false,
          muted: false,
          last_preview: "",
          last_at_ms: new Date().toISOString(),
          draft: "",
        };
        upsertConversation(user, conv);
        user.messages[conv.conv_id] = [];
        return ok(conv);
      }

      case ACTIONS.draftGet: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        return ok({ text: user.drafts[String(params.conv_id ?? "")] ?? "" });
      }

      case ACTIONS.draftSave: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const convId = String(params.conv_id ?? "");
        convOf(user, convId);
        const text = String(params.text ?? "");
        if (text) user.drafts[convId] = text;
        else delete user.drafts[convId];
        return ok({ saved: true });
      }

      case ACTIONS.draftClear: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        delete user.drafts[String(params.conv_id ?? "")];
        return ok({ cleared: true });
      }

      case ACTIONS.mediaUploadBegin: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const size = Number(params.size ?? 0);
        if (size <= 0 || size > MAX_UPLOAD_BYTES) {
          return fail(
            domainError("VALIDATION", "文件大小超出预算", requestId, {
              field_errors: { size: "单文件上限 1GB" },
            }),
          );
        }
        const uploadId = `upload-${Date.now().toString(36)}`;
        uploads.set(uploadId, {
          name: String(params.name ?? "file"),
          size,
          mime: String(params.mime ?? "application/octet-stream"),
          received: 0,
        });
        return ok({ upload_id: uploadId, chunk_size: 4 * 1024 * 1024 });
      }

      case ACTIONS.mediaUploadPart: {
        const upload = uploads.get(String(params.upload_id ?? ""));
        if (!upload) return emptyError("VALIDATION", "上传会话不存在", requestId);
        const bytes = Number(params.length ?? 0);
        upload.received += bytes;
        if (upload.received > upload.size) {
          return emptyError("VALIDATION", "上传超出声明大小", requestId);
        }
        return ok({ received: upload.received, size: upload.size });
      }

      case ACTIONS.mediaUploadComplete: {
        const uploadId = String(params.upload_id ?? "");
        const upload = uploads.get(uploadId);
        if (!upload) return emptyError("VALIDATION", "上传会话不存在", requestId);
        if (upload.received !== upload.size) {
          return emptyError("CONFLICT", "上传未完成", requestId);
        }
        uploads.delete(uploadId);
        return ok({ media_id: `media-${uploadId}`, name: upload.name, size: upload.size, mime: upload.mime });
      }

      case ACTIONS.mediaUploadCancel: {
        uploads.delete(String(params.upload_id ?? ""));
        return ok({ cancelled: true });
      }

      case ACTIONS.relationsList: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const query = String(params.query ?? "").trim().toLowerCase();
        const tag = params.tag ? String(params.tag) : null;
        const starOnly = Boolean(params.star);
        let rows = user.contacts.filter((contact) => !contact.blocked);
        if (query) {
          rows = rows.filter(
            (contact) =>
              contact.name.toLowerCase().includes(query) ||
              (contact.note ?? "").toLowerCase().includes(query),
          );
        }
        if (tag) rows = rows.filter((contact) => contact.tags.includes(tag));
        if (starOnly) rows = rows.filter((contact) => contact.star);
        rows = [...rows].sort((a, b) => Number(b.star) - Number(a.star) || a.name.localeCompare(b.name));
        const tags = [...new Set(user.contacts.flatMap((contact) => contact.tags))].sort();
        return ok({ items: rows, tags });
      }

      case ACTIONS.requestsIncoming: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        return ok<FriendRequest[]>(user.requests);
      }

      case ACTIONS.requestSend: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const target = String(params.user_id ?? "");
        const message = String(params.message ?? "").trim();
        const field_errors: Record<string, string> = {};
        if (!target) field_errors.target = "请选择联系人";
        if (message.length > 50) field_errors.message = "附言不能超过 50 字";
        if (target === user.user_id) field_errors.target = "不能添加自己";
        if (contactById(user, target)) field_errors.target = "对方已经是好友";
        if (Object.keys(field_errors).length) {
          return fail(domainError("VALIDATION", "好友申请参数不合法", requestId, { field_errors }));
        }
        const requestIdOut = `out-${requestSeq++}`;
        return ok({ request_id: requestIdOut, status: "PENDING" as const });
      }

      case ACTIONS.requestAccept:
      case ACTIONS.requestDecline: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const incomingId = String(params.request_id ?? "");
        const incoming = user.requests.find((item) => item.request_id === incomingId);
        if (!incoming) return emptyError("VALIDATION", "申请不存在", requestId);
        if (incoming.status !== "PENDING") return emptyError("CONFLICT", "该申请已处理", requestId);
        const accept = actionId === ACTIONS.requestAccept;
        incoming.status = accept ? "ACCEPTED" : "DECLINED";
        emit("friend_request.updated", incoming.request_id, incoming);
        if (accept) {
          const contact = ensureContact(user, incoming.from.user_id);
          emit("search.invalidated", user.user_id, {});
          return ok({ status: "ACCEPTED" as const, contact });
        }
        return ok({ status: "DECLINED" as const });
      }

      case ACTIONS.relationBlock: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const contact = contactById(user, String(params.user_id ?? ""));
        if (!contact) return emptyError("VALIDATION", "不是好友", requestId);
        contact.blocked = Boolean(params.blocked);
        emit("contact.updated", contact.user_id, contact);
        emit("search.invalidated", user.user_id, {});
        return ok(contact);
      }

      case ACTIONS.relationNote:
      case ACTIONS.relationTag:
      case ACTIONS.relationStar: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const contact = contactById(user, String(params.user_id ?? ""));
        if (!contact) return emptyError("VALIDATION", "不是好友", requestId);
        if (request.action_id === ACTIONS.relationNote) {
          const note = String(params.note ?? "").trim();
          if (note.length > 32) {
            return fail(domainError("VALIDATION", "备注过长", requestId, { field_errors: { note: "最多 32 字" } }));
          }
          contact.note = note;
        } else if (request.action_id === ACTIONS.relationTag) {
          const tags = Array.isArray(params.tags) ? (params.tags as string[]) : [];
          if (tags.length > 10) return emptyError("VALIDATION", "标签最多 10 个", requestId);
          contact.tags = tags.map((tag) => tag.trim()).filter(Boolean).slice(0, 10);
        } else {
          contact.star = Boolean(params.star);
        }
        emit("contact.updated", contact.user_id, contact);
        emit("search.invalidated", user.user_id, {});
        return ok(contact);
      }

      case ACTIONS.relationProfile: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const profile = profileOf(user, String(params.user_id ?? ""));
        if (!profile) return emptyError("VALIDATION", "用户不存在", requestId);
        return ok(profile);
      }

      case ACTIONS.commonGroups: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const userId = String(params.user_id ?? "");
        const profile = profileOf(user, userId);
        if (!profile) return emptyError("VALIDATION", "用户不存在", requestId);
        return ok(profile.common_groups);
      }

      case ACTIONS.groupMembers: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const group = user.groups.find((item) => item.group_id === String(params.group_id ?? ""));
        if (!group) return emptyError("FORBIDDEN", "不在该群中", requestId);
        return ok({ group_id: group.group_id, name: group.name, my_role: group.my_role, members: group.members });
      }

      case ACTIONS.groupKick: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const group = user.groups.find((item) => item.group_id === String(params.group_id ?? ""));
        if (!group) return emptyError("FORBIDDEN", "不在该群中", requestId);
        if (group.my_role === "member") return emptyError("FORBIDDEN", "只有群主或管理员可以移除成员", requestId);
        const target = String(params.user_id ?? "");
        const targetMember = group.members.find((member) => member.user_id === target);
        if (!targetMember) return emptyError("VALIDATION", "成员不存在", requestId);
        if (targetMember.role === "owner") return emptyError("FORBIDDEN", "不能移除群主", requestId);
        group.members = group.members.filter((member) => member.user_id !== target);
        emit("group.members.updated", group.group_id, {
          group_id: group.group_id,
          members: group.members,
        });
        emit("search.invalidated", user.user_id, {});
        return ok({ removed: target });
      }

      case ACTIONS.groupSettings: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const group = user.groups.find((item) => item.group_id === String(params.group_id ?? ""));
        if (!group) return emptyError("FORBIDDEN", "不在该群中", requestId);
        if (group.my_role === "member") return emptyError("FORBIDDEN", "只有群主或管理员可以修改群设置", requestId);
        const name = String(params.name ?? "").trim();
        if (!name) return fail(domainError("VALIDATION", "群名称不能为空", requestId, { field_errors: { name: "请输入群名称" } }));
        group.name = name;
        emit("group.updated", group.group_id, { group_id: group.group_id, name });
        emit("search.invalidated", user.user_id, {});
        return ok({ group_id: group.group_id, name });
      }

      case ACTIONS.searchQuery: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const query = String(params.query ?? "");
        const category = String(params.category ?? "all") as SearchCategory;
        if (!["all", "chats", "contacts", "groups", "content"].includes(category)) {
          return fail(domainError("VALIDATION", "未知搜索分类", requestId, { field_errors: { category: "分类不合法" } }));
        }
        return ok(searchHits(user, query, category));
      }

      case ACTIONS.preferencesGet: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        return ok(user.preferences);
      }

      case ACTIONS.preferencesSet: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const patch = params.patch as Partial<Preferences> | undefined;
        if (!patch || typeof patch !== "object") {
          return emptyError("VALIDATION", "缺少设置内容", requestId);
        }
        if (patch.theme && !["light", "dark", "system"].includes(patch.theme)) {
          return fail(domainError("VALIDATION", "主题取值不合法", requestId, { field_errors: { theme: "light/dark/system" } }));
        }
        user.preferences = { ...user.preferences, ...patch };
        emit("preferences.updated", user.user_id, user.preferences);
        return ok(user.preferences);
      }

      case ACTIONS.secretsStatus: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        return ok({
          locked: secretsLocked,
          os_backed: false,
          passphrase_migration: "available" as const,
          note: "fixture 不写 OS 密钥库；生产由 G02/G01 提供",
        });
      }

      case ACTIONS.secretsLock: {
        if (!account()) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        secretsLocked = true;
        return ok({ locked: true });
      }

      case ACTIONS.secretsMigrate: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const current = String(params.current_password ?? "");
        const next = String(params.new_password ?? "");
        const field_errors: Record<string, string> = {};
        if (current !== user.password) field_errors.current_password = "当前口令不正确";
        if (next.length < 8) field_errors.new_password = "新口令至少 8 位";
        if (current && next && current === next) field_errors.new_password = "新口令不能与当前口令相同";
        if (Object.keys(field_errors).length) {
          return fail(domainError("VALIDATION", "口令迁移参数不合法", requestId, { field_errors }));
        }
        user.password = next;
        return ok({ migrated: true, os_backed: false });
      }

      case ACTIONS.storageStatus: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const messageBytes = Object.values(user.messages)
          .flat()
          .reduce((sum, msg) => sum + (msg.body.text?.length ?? 0), 0) * 2;
        return ok<StorageStatus>({
          cache_bytes: 48 * 1024 * 1024,
          message_bytes: messageBytes,
          media_bytes: 0,
        });
      }

      case ACTIONS.cacheClear: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        return ok({ freed_bytes: 48 * 1024 * 1024, kept: ["未发送 outbox", "草稿", "密钥"] });
      }

      case ACTIONS.backupCreate: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const snapshot = {
          format: "wxlike-fixture-backup-v1",
          exported_at: new Date().toISOString(),
          user_id: user.user_id,
          contacts: user.contacts,
          groups: user.groups,
          conversations: user.conversations,
          messages: user.messages,
          preferences: user.preferences,
        };
        const json = JSON.stringify(snapshot);
        const bytes = encodeUtf8(json);
        const info: BackupInfo = {
          file_name: `wxlike-backup-${user.user_id}.json`,
          size: bytes.byteLength,
          digest: digest(json),
          created_at_ms: new Date().toISOString(),
        };
        return ok({ info, base64: toBase64(bytes) });
      }

      case ACTIONS.backupVerify: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        const raw = String(params.base64 ?? "");
        try {
          const json = fromBase64(raw);
          const parsed = JSON.parse(json) as { format?: string; user_id?: string; exported_at?: string };
          if (parsed.format !== "wxlike-fixture-backup-v1") {
            return ok({ valid: false, reason: "备份格式不匹配", digest: digest(json) });
          }
          if (parsed.user_id !== user.user_id) {
            return ok({ valid: false, reason: "备份属于其他账号", digest: digest(json) });
          }
          return ok({ valid: true, digest: digest(json), exported_at: parsed.exported_at });
        } catch {
          return ok({ valid: false, reason: "文件不是有效的备份", digest: "" });
        }
      }

      case ACTIONS.backupImport: {
        const user = account();
        if (!user) return emptyError("UNAUTHENTICATED", "尚未登录", requestId);
        if (params.confirmation !== "IMPORT") {
          return emptyError("REQUIRES_CONFIRMATION", "导入会覆盖本地数据，需要再次确认", requestId);
        }
        try {
          const parsed = JSON.parse(fromBase64(String(params.base64 ?? ""))) as {
            format?: string;
            user_id?: string;
            contacts?: Contact[];
            groups?: FixtureAccount["groups"];
            conversations?: Conversation[];
            messages?: Record<string, ChatMessage[]>;
            preferences?: Preferences;
          };
          if (parsed.format !== "wxlike-fixture-backup-v1" || parsed.user_id !== user.user_id) {
            return emptyError("VALIDATION", "备份与当前账号不匹配", requestId);
          }
          user.contacts = parsed.contacts ?? user.contacts;
          user.groups = parsed.groups ?? user.groups;
          user.conversations = parsed.conversations ?? user.conversations;
          user.messages = parsed.messages ?? user.messages;
          user.preferences = parsed.preferences ?? user.preferences;
          emit("contact.updated", user.user_id, { imported: true });
          emit("search.invalidated", user.user_id, {});
          return ok({ imported: true });
        } catch {
          return emptyError("VALIDATION", "备份文件损坏", requestId);
        }
      }

      case ACTIONS.appInfo:
        return ok<AppInfo>({
          name: "wxlike desktop-web",
          version: import.meta.env.VITE_APP_VERSION ?? "0.1.0",
          commit: import.meta.env.VITE_GIT_SHA ?? "dev",
          built_at: import.meta.env.VITE_BUILD_TIME ?? "local",
          licenses: [
            { name: "wxlike", license: "AGPL-3.0-only" },
            { name: "React", license: "MIT", url: "https://react.dev" },
            { name: "lucide-react", license: "ISC", url: "https://lucide.dev" },
            { name: "Tailwind CSS", license: "MIT", url: "https://tailwindcss.com" },
          ],
        });

      default:
        return emptyError("NOT_IMPLEMENTED", `fixture 未实现 ${request.action_id}`, requestId);
    }
  }

  return {
    transport: "fixture",
    async dispatch<T>(request: ActionRequest): Promise<ActionResponse<T>> {
      if (latency > 0) await new Promise((resolve) => setTimeout(resolve, latency));
      const key = request.idempotency_key;
      if (key) {
        const prior = idempotency.get(key);
        if (prior) {
          const requestDigest = digest(JSON.stringify(request.params));
          if (prior.action_id !== request.action_id || prior.digest !== requestDigest) {
            return fail(
              domainError("CONFLICT", "同一幂等键携带了不同参数", request.request_id),
            );
          }
          return prior.response as ActionResponse<T>;
        }
      }
      const response = await handle(request);
      if (key && response.ok) {
        idempotency.set(key, {
          action_id: request.action_id,
          digest: digest(JSON.stringify(request.params)),
          response,
        });
      }
      return response as ActionResponse<T>;
    },
    subscribe(_cursor: EventCursor | null, handler: (event: EventEnvelope) => void): Unsubscribe {
      listeners.add(handler);
      return () => listeners.delete(handler);
    },
    async capabilities(): Promise<CoreCapabilities> {
      return {
        contract_version: "fixture",
        transport: "fixture",
        actions: Object.values(ACTIONS),
        qr_login: true,
        secrets_os_backed: false,
        sync_range: true,
        notes: ["fixture 适配器仅用于界面开发/单元测试，不能作为集成验收证据"],
      };
    },
    simulateQr(state) {
      if (!challenge) return;
      if (state === "APPROVED") {
        const userId = challenge.account_hint ?? "user-alpha";
        challenge = { ...challenge, state: "APPROVED" };
        emit("qr.state", challenge.challenge_id, challenge);
        authenticate(userId);
        return;
      }
      challenge = { ...challenge, state };
      emit("qr.state", challenge.challenge_id, challenge);
      if (state === "EXPIRED" || state === "REJECTED") challenge = null;
    },
    setOffline(value) {
      offline = value;
    },
    currentSession() {
      return session;
    },
    reset() {
      for (const timer of timers) clearTimeout(timer);
      timers.clear();
      accounts.clear();
      accounts.set("user-alpha", createAlphaAccount());
      accounts.set("user-beta", createBetaAccount());
      session = { phase: "signed_out" };
      challenge = null;
      offline = false;
      idempotency.clear();
      uploads.clear();
      secretsLocked = false;
    },
  };
}

export type { Contact, Conversation, GroupMember };
