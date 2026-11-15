/**
 * Content domain client (G11-01): content.* / favorite.* / publisher.* / sticker.*.
 *
 * Real today (server `crates/platform`):
 *   - moments: publish/feed/detail/remove/likes/comments  → `/api/moments*`
 *   - favorites: add/list/one/remove                      → `/api/favorites*`
 *   - report: `/api/reports`
 * Contract-gated (G05 ContentPort still open; the card allows fixture
 * development until the upstream merges, and the UI says so instead of
 * pretending): articles / publishers / sticker packs.
 *
 * Idempotency: every write carries an `Idempotency-Key` so a retry (double
 * click, network flap) cannot create a second like/comment/post.
 */

import { ApiError, blockedError, idemKey, request } from "../../lib/http";
import type {
  ArticleHead,
  ContentPorts,
  FavoriteQuery,
  FavoriteRecord,
  FavoriteSortKey,
  MomentComment,
  MomentDetail,
  MomentHead,
  PublishInput,
  PublisherRecord,
  RequestOpts,
  StickerPack,
} from "./model";

function q(params: Record<string, string | number | undefined>): string {
  const usp = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== "") usp.set(key, String(value));
  }
  const text = usp.toString();
  return text ? `?${text}` : "";
}

export class ContentApi implements ContentPorts {
  async publishMoment(input: PublishInput, opts?: RequestOpts): Promise<MomentHead> {
    const created = await request<MomentHead & { moment_id: string }>("POST", "/api/moments", {
      body: { text: input.text, media_ids: input.media_ids, visibility: input.visibility },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-publish"),
      signal: opts?.signal,
    });
    // 服务端只回 {moment_id} 时回读 detail 补全 head（不编造作者/计数）；
    // 若已带完整 head（夹具/旧服务端）则直接用。
    if (typeof created.author === "string" && typeof created.like_count === "number") return created;
    return this.moment(created.moment_id, opts);
  }

  async feed(
    since?: number,
    limit?: number,
    opts?: RequestOpts,
  ): Promise<{ items: MomentHead[]; next_cursor: number | null }> {
    const body = await request<{ items?: MomentHead[]; next_cursor?: number | null } | MomentHead[]>(
      "GET",
      `/api/moments/feed${q({ since, limit })}`,
      { signal: opts?.signal },
    );
    // Server replies `{items, next_cursor}`; tolerate a bare array defensively.
    const items = Array.isArray(body) ? body : (body.items ?? []);
    const next = Array.isArray(body) ? null : (body.next_cursor ?? null);
    return { items, next_cursor: next };
  }

  async moment(id: string, opts?: RequestOpts): Promise<MomentDetail> {
    return request<MomentDetail>("GET", `/api/moments/${encodeURIComponent(id)}`, {
      signal: opts?.signal,
    });
  }

  async removeMoment(id: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("DELETE", `/api/moments/${encodeURIComponent(id)}`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-remove"),
      signal: opts?.signal,
    });
  }

  async like(id: string, opts?: RequestOpts): Promise<{ like_count: number }> {
    const reply = await request<{ like_count?: number }>(
      "POST",
      `/api/moments/${encodeURIComponent(id)}/likes`,
      { idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-like"), signal: opts?.signal },
    );
    // 服务端回 {liked,added} 不带 like_count 时回读 detail 取真实计数。
    if (typeof reply.like_count === "number") return { like_count: reply.like_count };
    const detail = await this.moment(id, opts);
    return { like_count: detail.like_count };
  }

  async unlike(id: string, opts?: RequestOpts): Promise<{ like_count: number }> {
    const reply = await request<{ like_count?: number }>(
      "DELETE",
      `/api/moments/${encodeURIComponent(id)}/likes`,
      { idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-unlike"), signal: opts?.signal },
    );
    if (typeof reply.like_count === "number") return { like_count: reply.like_count };
    const detail = await this.moment(id, opts);
    return { like_count: detail.like_count };
  }

  async comment(
    id: string,
    text: string,
    replyTo?: string,
    opts?: RequestOpts,
  ): Promise<MomentComment> {
    const created = await request<MomentComment & { comment_id: string }>(
      "POST",
      `/api/moments/${encodeURIComponent(id)}/comments`,
      {
        body: { text, reply_to_comment_id: replyTo ?? null },
        idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-comment"),
        signal: opts?.signal,
      },
    );
    // 已带完整评论（夹具/旧服务端）直接用；只回 {comment_id} 时回读 detail。
    if (typeof created.text === "string" && typeof created.author === "string") return created;
    const detail = await this.moment(id, opts);
    const comment = detail.comments.find((c) => c.comment_id === created.comment_id);
    if (!comment) throw new ApiError(502, "评论已提交但回读不到（不编造）", `/api/moments/${id}`, "server");
    return comment;
  }

  async removeComment(id: string, commentId: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>(
      "DELETE",
      `/api/moments/${encodeURIComponent(id)}/comments/${encodeURIComponent(commentId)}`,
      { idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-comment-remove"), signal: opts?.signal },
    );
  }

  async report(targetId: string, reason: string, detail = "", opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", "/api/reports", {
      body: { target_kind: "moment", target_id: targetId, reason, detail },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("report"),
      signal: opts?.signal,
    });
  }

  async listFavorites(query: FavoriteQuery = {}, opts?: RequestOpts): Promise<FavoriteRecord[]> {
    const body = await request<FavoriteRecord[] | { favorites?: FavoriteRecord[] }>("GET", "/api/favorites", { signal: opts?.signal });
    const all = Array.isArray(body) ? body : (body.favorites ?? []);
    return applyFavoriteQuery(all, query);
  }

  async favorite(id: string, opts?: RequestOpts): Promise<FavoriteRecord> {
    return request<FavoriteRecord>("GET", `/api/favorites/${encodeURIComponent(id)}`, {
      signal: opts?.signal,
    });
  }

  async removeFavorite(id: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("DELETE", `/api/favorites/${encodeURIComponent(id)}`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("favorite-remove"),
      signal: opts?.signal,
    });
  }

  /**
   * The server stores the message snapshot verbatim; the note edit is a
   * client-side re-save of the same record (add/remove round-trip with the
   * edited text). When G05 lands an inline PATCH this switches to it without
   * touching callers — they only see `ContentPorts`.
   */
  async updateFavoriteNote(id: string, text: string, opts?: RequestOpts): Promise<FavoriteRecord> {
    // 服务端 inline PUT（snapshot_label），不再 remove+add 往返。
    return request<FavoriteRecord>("PUT", `/api/favorites/${encodeURIComponent(id)}`, {
      body: { snapshot_label: text },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("favorite-edit"),
      signal: opts?.signal,
    });
  }

  // --- G05 ContentPort contract surface (upstream pending) -------------------
  // 服务端尚未对齐这些契约面（articles 列表/publisher 订阅/article watching）。
  // D6 退役后旧 HTTP 出口已拆，这里显式阻塞（501），页面渲染"上游未开通"，
  // 不回落旧会话、不伪造成功。等服务端对齐后接桥 action。

  async articles(_opts?: RequestOpts): Promise<ArticleHead[]> {
    throw blockedError("/api/articles", "文章列表（G05 ContentPort）尚无核心提供者");
  }

  async article(id: string, _opts?: RequestOpts): Promise<ArticleHead> {
    throw blockedError(`/api/articles/${id}`, "文章详情（G05 ContentPort）尚无核心提供者");
  }

  async publishers(_opts?: RequestOpts): Promise<PublisherRecord[]> {
    throw blockedError("/api/publishers", "订阅号列表（G05 ContentPort）尚无核心提供者");
  }

  async followPublisher(publisherId: string, _follow: boolean, _opts?: RequestOpts): Promise<PublisherRecord> {
    throw blockedError(`/api/publishers/${publisherId}/subscription`, "订阅（G05 ContentPort）尚无核心提供者");
  }

  async markWatching(articleId: string, _watching: boolean, _opts?: RequestOpts): Promise<void> {
    throw blockedError(`/api/articles/${articleId}/watching`, "在看（G05 ContentPort）尚无核心提供者");
  }

  async stickerPacks(_opts?: RequestOpts): Promise<StickerPack[]> {
    const body = await request<StickerPack[] | { packs?: StickerPack[] }>("GET", "/api/stickers/packs");
    return Array.isArray(body) ? body : (body.packs ?? []);
  }
}

/** Client-side narrowing/sort for the favorites list (server returns the full snapshot set). */
export function applyFavoriteQuery(records: FavoriteRecord[], query: FavoriteQuery): FavoriteRecord[] {
  const keyword = query.keyword?.trim().toLowerCase() ?? "";
  const kind = query.kind ?? "all";
  const sort: FavoriteSortKey = query.sort ?? "newest";
  let out = records.filter((record) => {
    if (kind !== "all" && record.kind !== kind) return false;
    if (!keyword) return true;
    return (
      record.text.toLowerCase().includes(keyword) ||
      record.sender.toLowerCase().includes(keyword) ||
      record.source_conv.toLowerCase().includes(keyword)
    );
  });
  out = [...out].sort((a, b) => {
    if (sort === "oldest") return a.created_at - b.created_at;
    if (sort === "sender") return a.sender.localeCompare(b.sender) || b.created_at - a.created_at;
    return b.created_at - a.created_at;
  });
  if (query.limit !== undefined) out = out.slice(0, query.limit);
  return out;
}
