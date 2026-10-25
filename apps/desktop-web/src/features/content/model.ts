/** Shared types for the content feature (G11-01). Mirrors `crates/platform/src/moments.rs` / `favorites.rs`. */

export type Visibility = "public" | "friends" | "private";

export interface MomentHead {
  moment_id: string;
  author: string;
  author_name: string;
  text: string;
  media_ids: string[];
  visibility: string;
  created_at: number;
  like_count: number;
  comment_count: number;
}

export interface MomentComment {
  comment_id: string;
  author: string;
  author_name?: string;
  text: string;
  reply_to_comment_id: string | null;
  created_at: number;
}

export interface MomentDetail extends MomentHead {
  comments: MomentComment[];
  liked_by_me?: boolean;
}

export interface PublishInput {
  text: string;
  media_ids: string[];
  visibility: Visibility;
}

export interface FavoriteRecord {
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

export type FavoriteSortKey = "newest" | "oldest" | "sender";

export type FavoriteKindFilter = "all" | "text" | "image" | "file" | "link" | "voice" | "video";

export interface FavoriteQuery {
  keyword?: string;
  kind?: FavoriteKindFilter;
  sort?: FavoriteSortKey;
  limit?: number;
}

/** G05 ContentPort contract shape (`/api/articles`, `/api/publishers`); upstream not yet on the server. */
export interface ArticleHead {
  article_id: string;
  publisher_id: string;
  publisher_name: string;
  title: string;
  /** Pre-sanitized HTML per contract (server purifies before storage). */
  doc_html: string;
  doc_version: number;
  published_at: number;
  reading_count?: number;
}

export interface PublisherRecord {
  publisher_id: string;
  name: string;
  verified: boolean;
  subscribed: boolean;
  watching: boolean;
}

export interface StickerPack {
  pack_id: string;
  name: string;
  /** Emoji-style stickers keep the feature self-contained; image packs resolve through media. */
  items: string[];
}

export interface ContentPorts {
  publishMoment(input: PublishInput, opts?: RequestOpts): Promise<MomentHead>;
  feed(since?: number, limit?: number, opts?: RequestOpts): Promise<{ items: MomentHead[]; next_cursor: number | null }>;
  moment(id: string, opts?: RequestOpts): Promise<MomentDetail>;
  removeMoment(id: string, opts?: RequestOpts): Promise<void>;
  like(id: string, opts?: RequestOpts): Promise<{ like_count: number }>;
  unlike(id: string, opts?: RequestOpts): Promise<{ like_count: number }>;
  comment(id: string, text: string, replyTo?: string, opts?: RequestOpts): Promise<MomentComment>;
  removeComment(id: string, commentId: string, opts?: RequestOpts): Promise<void>;
  report(targetId: string, reason: string, detail?: string, opts?: RequestOpts): Promise<void>;
  listFavorites(query?: FavoriteQuery, opts?: RequestOpts): Promise<FavoriteRecord[]>;
  favorite(id: string, opts?: RequestOpts): Promise<FavoriteRecord>;
  removeFavorite(id: string, opts?: RequestOpts): Promise<void>;
  updateFavoriteNote(id: string, text: string, opts?: RequestOpts): Promise<FavoriteRecord>;
  articles(opts?: RequestOpts): Promise<ArticleHead[]>;
  article(id: string, opts?: RequestOpts): Promise<ArticleHead>;
  publishers(opts?: RequestOpts): Promise<PublisherRecord[]>;
  followPublisher(publisherId: string, follow: boolean, opts?: RequestOpts): Promise<PublisherRecord>;
  markWatching(articleId: string, watching: boolean, opts?: RequestOpts): Promise<void>;
  stickerPacks(opts?: RequestOpts): Promise<StickerPack[]>;
}

export interface RequestOpts {
  signal?: AbortSignal;
  idempotencyKey?: string;
}
