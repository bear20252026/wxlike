/**
 * 朋友圈 (G11-01). Publish / feed / likes / comments / report / visibility,
 * every action against the real `/api/moments*` endpoints.
 *
 * behaviours the acceptance card calls out:
 * - 加载与保存错误保留草稿 — publish failure keeps the composer text.
 * - 笔记刷新保留 — the composer draft survives a page reload (localStorage).
 * - 所有三点菜单展开验收 — per-card and per-comment menus render real items.
 * - XSS payload 不执行 — body text renders through sanitizeRichText only.
 * - 写操作重复提交 — publish/like/comment guard on in-flight + idempotency key.
 * - 取消 — an in-flight publish can be cancelled; cancelling never toasts success.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { clearDraft, loadDraft, saveDraft } from "../../lib/draft";
import { errorText, request, userIdSafe } from "../../lib/http";
import { platform } from "../../lib/platform";
import { sanitizeRichText } from "../../lib/sanitize";
import type { ContentPorts } from "./model";
import type { MomentComment, MomentDetail, MomentHead, PublishInput, Visibility } from "./model";
import { Banner, DotMenu, EmptyState, GhostButton, Modal, PrimaryButton, formatTime } from "./ui";

const DRAFT_SCOPE = "moments-composer";
const VISIBILITIES: Array<{ value: Visibility; label: string }> = [
  { value: "public", label: "公开" },
  { value: "friends", label: "好友" },
  { value: "private", label: "私密" },
];

interface ComposerDraft {
  text: string;
  visibility: Visibility;
}

interface FeedState {
  items: MomentHead[];
  nextCursor: number | null;
  loading: boolean;
  error: string;
}

export function MomentsPage({ ports }: { ports: ContentPorts }) {
  const [feed, setFeed] = useState<FeedState>({ items: [], nextCursor: null, loading: true, error: "" });
  const loadSeq = useRef(0);

  const load = useCallback(
    async (portsRef: ContentPorts) => {
      const seq = ++loadSeq.current;
      setFeed((prev) => ({ ...prev, loading: true, error: "" }));
      try {
        const page = await portsRef.feed(undefined, 20);
        if (seq !== loadSeq.current) return;
        setFeed({ items: page.items, nextCursor: page.next_cursor, loading: false, error: "" });
      } catch (err) {
        if (seq !== loadSeq.current) return;
        setFeed((prev) => ({ ...prev, loading: false, error: errorText(err) }));
      }
    },
    [],
  );

  useEffect(() => {
    void load(ports);
    return () => {
      loadSeq.current += 1; // invalidate in-flight responses on unmount
    };
  }, [load, ports]);

  const prepend = (head: MomentHead) =>
    setFeed((prev) => ({ ...prev, items: [head, ...prev.items] }));

  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-4 overflow-y-auto p-4" data-testid="moments-page">
      <Composer ports={ports} onPublished={prepend} />
      <Banner tone="error" text={feed.error} />
      {feed.loading && feed.items.length === 0 ? (
        <div className="flex items-center justify-center gap-2 py-10 text-sm text-zinc-400" role="status">
          <span className="inline-block h-4 w-4 animate-spin rounded-full border-2 border-zinc-300 border-t-zinc-600" />
          正在加载朋友圈…
        </div>
      ) : null}
      {!feed.loading && feed.error === "" && feed.items.length === 0 ? (
        <EmptyState title="还没有动态" hint="发布第一条朋友圈吧" />
      ) : null}
      {feed.items.map((item) => (
        <MomentCard key={item.moment_id} head={item} ports={ports} onRemoved={(id) => setFeed((prev) => ({ ...prev, items: prev.items.filter((m) => m.moment_id !== id) }))} />
      ))}
      {feed.nextCursor !== null && !feed.loading ? (
        <GhostButton onClick={() => void loadOlder()}>加载更早的动态</GhostButton>
      ) : null}
    </div>
  );

  async function loadOlder() {
    if (feed.nextCursor === null) return;
    try {
      const page = await ports.feed(feed.nextCursor, 20);
      setFeed((prev) => ({
        items: [...prev.items, ...page.items],
        nextCursor: page.next_cursor,
        loading: false,
        error: "",
      }));
    } catch (err) {
      setFeed((prev) => ({ ...prev, error: errorText(err) }));
    }
  }
}

function Composer({ ports, onPublished }: { ports: ContentPorts; onPublished: (head: MomentHead) => void }) {
  const saved = loadDraft<ComposerDraft>(DRAFT_SCOPE);
  const [text, setText] = useState(saved?.text ?? "");
  const [visibility, setVisibility] = useState<Visibility>(saved?.visibility ?? "friends");
  const [mediaIds, setMediaIds] = useState<string[]>([]);
  const [publishing, setPublishing] = useState(false);
  const [error, setError] = useState("");
  const [okNote, setOkNote] = useState("");
  const publishAbort = useRef<AbortController | null>(null);
  const inFlight = useRef(false);

  useEffect(() => {
    saveDraft<ComposerDraft>(DRAFT_SCOPE, { text, visibility });
  }, [text, visibility]);

  const publish = async () => {
    if (inFlight.current) return; // 写操作重复提交：只放行一次
    inFlight.current = true;
    setPublishing(true);
    setError("");
    setOkNote("");
    const controller = new AbortController();
    publishAbort.current = controller;
    try {
      const input: PublishInput = { text: text.trim(), media_ids: mediaIds, visibility };
      const head = await ports.publishMoment(input, { signal: controller.signal });
      clearDraft(DRAFT_SCOPE);
      setText("");
      setMediaIds([]);
      setVisibility("friends");
      onPublished(head);
      setOkNote("已发布");
    } catch (err) {
      // 保存失败保留草稿（文本仍在输入框里，本地草稿也未被清除）
      setError(errorText(err));
    } finally {
      inFlight.current = false;
      setPublishing(false);
      publishAbort.current = null;
    }
  };

  const canCancel = publishing;
  const canPublish = !publishing && (text.trim().length > 0 || mediaIds.length > 0);

  return (
    <section className="rounded-2xl border border-zinc-200 bg-white p-4" aria-label="发布朋友圈">
      <textarea
        value={text}
        onChange={(event) => setText(event.target.value)}
        placeholder="这一刻的想法…"
        rows={3}
        maxLength={5000}
        data-testid="composer-text"
        className="w-full resize-none rounded-lg border border-zinc-200 p-2.5 text-sm outline-none focus:border-emerald-500"
      />
      {mediaIds.length > 0 ? (
        <p className="mt-1 text-xs text-zinc-400" data-testid="composer-media-count">
          已选择 {mediaIds.length} 张图片
        </p>
      ) : null}
      <div className="mt-2 flex items-center justify-between gap-2">
        <label className="flex items-center gap-1.5 text-sm text-zinc-500">
          可见性
          <select
            value={visibility}
            onChange={(event) => setVisibility(event.target.value as Visibility)}
            data-testid="composer-visibility"
            className="rounded-md border border-zinc-200 px-2 py-1 text-sm"
          >
            {VISIBILITIES.map((item) => (
              <option key={item.value} value={item.value}>
                {item.label}
              </option>
            ))}
          </select>
        </label>
        <div className="flex items-center gap-2">
          {canCancel ? (
            <GhostButton
              onClick={() => {
                publishAbort.current?.abort();
              }}
            >
              取消
            </GhostButton>
          ) : null}
          <PrimaryButton onClick={() => void publish()} disabled={!canPublish} busy={publishing} title="发布">
            发布
          </PrimaryButton>
        </div>
      </div>
      <div className="mt-2 space-y-2">
        <Banner tone="error" text={error} />
        <Banner tone="ok" text={okNote} />
      </div>
    </section>
  );
}

const VISIBILITY_LABEL: Record<string, string> = {
  public: "公开",
  friends: "好友",
  private: "私密",
};

function MomentCard({
  head,
  ports,
  onRemoved,
}: {
  head: MomentHead;
  ports: ContentPorts;
  onRemoved: (id: string) => void;
}) {
  const me = userIdSafe();
  const own = head.author === me;
  const [likeCount, setLikeCount] = useState(head.like_count);
  const [liked, setLiked] = useState(false);
  const [likeBusy, setLikeBusy] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [detail, setDetail] = useState<MomentDetail | null>(null);
  const [commentText, setCommentText] = useState("");
  const [commentBusy, setCommentBusy] = useState(false);
  const [error, setError] = useState("");
  const [reporting, setReporting] = useState(false);
  const likeInFlight = useRef(false);
  const commentInFlight = useRef(false);

  const toggleLike = async () => {
    if (likeInFlight.current) return;
    likeInFlight.current = true;
    setLikeBusy(true);
    setError("");
    const wasLiked = liked;
    // optimistic, rolled back on failure
    setLiked(!wasLiked);
    setLikeCount((count) => count + (wasLiked ? -1 : 1));
    try {
      const reply = wasLiked ? await ports.unlike(head.moment_id) : await ports.like(head.moment_id);
      if (typeof reply.like_count === "number") setLikeCount(reply.like_count);
    } catch (err) {
      setLiked(wasLiked);
      setLikeCount((count) => count + (wasLiked ? 1 : -1));
      setError(errorText(err));
    } finally {
      likeInFlight.current = false;
      setLikeBusy(false);
    }
  };

  const toggleComments = async () => {
    const next = !expanded;
    setExpanded(next);
    if (next && !detail) {
      try {
        setDetail(await ports.moment(head.moment_id));
      } catch (err) {
        setError(errorText(err));
      }
    }
  };

  const addComment = async () => {
    if (commentInFlight.current || !commentText.trim()) return;
    commentInFlight.current = true;
    setCommentBusy(true);
    setError("");
    try {
      const comment = await ports.comment(head.moment_id, commentText.trim());
      setDetail((prev) =>
        prev ? { ...prev, comments: [...prev.comments, comment], comment_count: prev.comment_count + 1 } : prev,
      );
      setCommentText("");
    } catch (err) {
      setError(errorText(err));
    } finally {
      commentInFlight.current = false;
      setCommentBusy(false);
    }
  };

  const removeComment = async (comment: MomentComment) => {
    try {
      await ports.removeComment(head.moment_id, comment.comment_id);
      setDetail((prev) =>
        prev
          ? { ...prev, comments: prev.comments.filter((c) => c.comment_id !== comment.comment_id), comment_count: Math.max(0, prev.comment_count - 1) }
          : prev,
      );
    } catch (err) {
      setError(errorText(err));
    }
  };

  const removeSelf = async () => {
    try {
      await ports.removeMoment(head.moment_id);
      onRemoved(head.moment_id);
    } catch (err) {
      setError(errorText(err));
    }
  };

  return (
    <article className="rounded-2xl border border-zinc-200 bg-white p-4" data-testid={`moment-${head.moment_id}`}>
      <header className="flex items-start justify-between gap-2">
        <div>
          <p className="text-sm font-semibold text-zinc-800">{head.author_name || head.author}</p>
          <p className="text-xs text-zinc-400">
            {formatTime(head.created_at)} · {VISIBILITY_LABEL[head.visibility] ?? head.visibility}
          </p>
        </div>
        <DotMenu
          items={
            own
              ? [{ label: "删除", onSelect: () => void removeSelf(), danger: true }]
              : [{ label: "举报", onSelect: () => setReporting(true) }]
          }
        />
      </header>

      {head.text ? (
        <div
          className="mt-2 text-sm leading-6 text-zinc-700"
          data-testid="moment-text"
          dangerouslySetInnerHTML={{ __html: sanitizeRichText(head.text) }}
        />
      ) : null}

      {head.media_ids.length > 0 ? (
        <div className={`mt-2 grid gap-1 ${head.media_ids.length === 1 ? "grid-cols-1" : "grid-cols-3"}`} data-testid="moment-media">
          {head.media_ids.map((mediaId) => (
            <MomentImage key={mediaId} mediaId={mediaId} />
          ))}
        </div>
      ) : null}

      <footer className="mt-3 flex items-center gap-3 text-sm text-zinc-500">
        <button
          type="button"
          onClick={() => void toggleLike()}
          disabled={likeBusy}
          data-testid={`like-${head.moment_id}`}
          className={`rounded-md px-2 py-1 transition hover:bg-zinc-100 disabled:opacity-50 ${liked ? "text-emerald-600" : ""}`}
        >
          {liked ? "已赞" : "赞"} · {likeCount}
        </button>
        <button
          type="button"
          onClick={() => void toggleComments()}
          data-testid={`comments-toggle-${head.moment_id}`}
          className="rounded-md px-2 py-1 transition hover:bg-zinc-100"
        >
          评论 · {detail ? detail.comments.length : head.comment_count}
        </button>
        <button
          type="button"
          onClick={() => void copyMoment(head)}
          className="rounded-md px-2 py-1 transition hover:bg-zinc-100"
          data-testid={`copy-${head.moment_id}`}
        >
          复制文本
        </button>
      </footer>

      {expanded ? (
        <div className="mt-2 space-y-2 rounded-lg bg-zinc-50 p-2.5" data-testid={`comments-${head.moment_id}`}>
          {(detail?.comments ?? []).map((comment) => (
            <div key={comment.comment_id} className="flex items-start justify-between gap-2 text-sm">
              <p className="text-zinc-600">
                <span className="font-medium text-zinc-800">{comment.author_name || comment.author}</span>
                ：{comment.text}
              </p>
              {comment.author === me ? (
                <DotMenu items={[{ label: "删除评论", onSelect: () => void removeComment(comment), danger: true }]} />
              ) : (
                <DotMenu items={[{ label: "举报评论", onSelect: () => setReporting(true) }]} />
              )}
            </div>
          ))}
          {detail && detail.comments.length === 0 ? <p className="text-xs text-zinc-400">还没有评论</p> : null}
          <div className="flex items-center gap-2">
            <input
              value={commentText}
              onChange={(event) => setCommentText(event.target.value)}
              placeholder="写评论…"
              maxLength={1000}
              data-testid={`comment-input-${head.moment_id}`}
              className="flex-1 rounded-md border border-zinc-200 px-2 py-1.5 text-sm outline-none focus:border-emerald-500"
            />
            <PrimaryButton onClick={() => void addComment()} busy={commentBusy} disabled={!commentText.trim()}>
              发送
            </PrimaryButton>
          </div>
        </div>
      ) : null}

      <div className="mt-2 space-y-2">
        <Banner tone="error" text={error} />
      </div>

      {reporting ? <ReportDialog ports={ports} momentId={head.moment_id} onClose={() => setReporting(false)} /> : null}
    </article>
  );

  async function copyMoment(item: MomentHead) {
    const ok = await platform().copyText(item.text);
    if (!ok) setError("复制失败");
  }
}

/** Signed-URL image: `/api/media/{id}/url` exists exactly for `<img>` consumers. */
function MomentImage({ mediaId }: { mediaId: string }) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const reply = await request<{ url?: string }>(
          "GET",
          `/api/media/${encodeURIComponent(mediaId)}/url`,
        );
        if (alive && reply && typeof reply.url === "string") setSrc(reply.url);
      } catch {
        if (alive) setFailed(true);
      }
    })();
    return () => {
      alive = false;
    };
  }, [mediaId]);
  if (failed) return <div className="aspect-square rounded-lg bg-zinc-100 text-center text-xs leading-[4rem] text-zinc-400">图片不可用</div>;
  if (!src) return <div className="aspect-square animate-pulse rounded-lg bg-zinc-100" />;
  return <img src={src} alt="朋友圈图片" className="aspect-square w-full rounded-lg object-cover" loading="lazy" />;
}

const REPORT_REASONS = ["垃圾营销", "违法违规", "侮辱谩骂", "侵权", "其他"];

function ReportDialog({
  ports,
  momentId,
  onClose,
}: {
  ports: ContentPorts;
  momentId: string;
  onClose: () => void;
}) {
  const [reason, setReason] = useState(REPORT_REASONS[0] ?? "其他");
  const [detail, setDetail] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [done, setDone] = useState(false);

  const submit = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await ports.report(momentId, reason, detail);
      setDone(true);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={done ? "举报已提交" : "举报该条动态"} onClose={onClose}>
      {done ? (
        <div className="space-y-3">
          <p className="text-sm text-zinc-600">感谢反馈，管理员会尽快处理。</p>
          <PrimaryButton onClick={onClose}>好的</PrimaryButton>
        </div>
      ) : (
        <div className="space-y-3">
          <label className="block text-sm text-zinc-600">
            举报原因
            <select
              value={reason}
              onChange={(event) => setReason(event.target.value)}
              data-testid="report-reason"
              className="mt-1 w-full rounded-md border border-zinc-200 px-2 py-1.5 text-sm"
            >
              {REPORT_REASONS.map((item) => (
                <option key={item}>{item}</option>
              ))}
            </select>
          </label>
          <label className="block text-sm text-zinc-600">
            补充说明（可选）
            <textarea
              value={detail}
              onChange={(event) => setDetail(event.target.value)}
              rows={3}
              data-testid="report-detail"
              className="mt-1 w-full resize-none rounded-md border border-zinc-200 px-2 py-1.5 text-sm"
            />
          </label>
          <Banner tone="error" text={error} />
          <div className="flex justify-end gap-2">
            <GhostButton onClick={onClose}>取消</GhostButton>
            <PrimaryButton onClick={() => void submit()} busy={busy}>
              提交举报
            </PrimaryButton>
          </div>
        </div>
      )}
    </Modal>
  );
}
