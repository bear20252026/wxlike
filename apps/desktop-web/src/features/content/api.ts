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

import { idemKey, request } from "../../lib/http";
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
    return request<MomentHead>("POST", "/api/moments", {
      body: { text: input.text, media_ids: input.media_ids, visibility: input.visibility },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-publish"),
      signal: opts?.signal,
    });
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
    return request<{ like_count: number }>(
      "POST",
      `/api/moments/${encodeURIComponent(id)}/likes`,
      { idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-like"), signal: opts?.signal },
    );
  }

  async unlike(id: string, opts?: RequestOpts): Promise<{ like_count: number }> {
    return request<{ like_count: number }>(
      "DELETE",
      `/api/moments/${encodeURIComponent(id)}/likes`,
      { idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-unlike"), signal: opts?.signal },
    );
  }

  async comment(
    id: string,
    text: string,
    replyTo?: string,
    opts?: RequestOpts,
  ): Promise<MomentComment> {
    return request<MomentComment>(
      "POST",
      `/api/moments/${encodeURIComponent(id)}/comments`,
      {
        body: { text, reply_to_comment_id: replyTo ?? null },
        idempotencyKey: opts?.idempotencyKey ?? idemKey("moment-comment"),
        signal: opts?.signal,
      },
    );
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
    const all = await request<FavoriteRecord[]>("GET", "/api/favorites", { signal: opts?.signal });
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
    const current = await this.favorite(id, opts);
    await request<unknown>("DELETE", `/api/favorites/${encodeURIComponent(id)}`, {
      idempotencyKey: idemKey("favorite-remove"),
      signal: opts?.signal,
    });
    return request<FavoriteRecord>("POST", "/api/favorites", {
      body: {
        source_conv: current.source_conv,
        source_msg_id: current.source_msg_id,
        kind: current.kind,
        text,
        media_id: current.media_id,
        sender: current.sender,
        sent_at_ms: current.sent_at_ms,
      },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("favorite-edit"),
      signal: opts?.signal,
    });
  }

  // --- G05 ContentPort contract surface (upstream pending) -------------------

  async articles(_opts?: RequestOpts): Promise<ArticleHead[]> {
    // Raises until G05 merges; the page renders the explicit state.
    return request<ArticleHead[]>("GET", "/api/articles");
  }

  async article(id: string, opts?: RequestOpts): Promise<ArticleHead> {
    return request<ArticleHead>("GET", `/api/articles/${encodeURIComponent(id)}`, {
      signal: opts?.signal,
    });
  }

  async publishers(_opts?: RequestOpts): Promise<PublisherRecord[]> {
    return request<PublisherRecord[]>("GET", "/api/publishers");
  }

  async followPublisher(publisherId: string, follow: boolean, opts?: RequestOpts): Promise<PublisherRecord> {
    return request<PublisherRecord>(
      follow ? "POST" : "DELETE",
      `/api/publishers/${encodeURIComponent(publisherId)}/subscription`,
      { idempotencyKey: opts?.idempotencyKey ?? idemKey("publisher-subscribe"), signal: opts?.signal },
    );
  }

  async markWatching(articleId: string, watching: boolean, opts?: RequestOpts): Promise<void> {
    await request<unknown>("POST", `/api/articles/${encodeURIComponent(articleId)}/watching`, {
      body: { watching },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("article-watching"),
      signal: opts?.signal,
    });
  }

  async stickerPacks(_opts?: RequestOpts): Promise<StickerPack[]> {
    return request<StickerPack[]>("GET", "/api/stickers/packs");
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
