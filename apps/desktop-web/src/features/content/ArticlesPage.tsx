/**
 * 订阅号文章 (G11-01): 阅读/关注/在看.
 *
 * The article/publisher surface belongs to G05 ContentPort and is not on the
 * server yet. The page renders the real upstream state instead of sample
 * data: when the endpoint is absent it shows the explicit "上游未开通" banner,
 * and once G05 ships the same code reads the real list. Rich text is
 * sanitized on render (the contract promises purified docs, the client
 * re-checks anyway).
 */

import { useEffect, useState } from "react";
import { ApiError, errorText } from "../../lib/http";
import { sanitizeRichText } from "../../lib/sanitize";
import type { ArticleHead, ContentPorts, PublisherRecord } from "./model";
import { Banner, EmptyState, GhostButton, PrimaryButton } from "./ui";

export function ArticlesPage({ ports }: { ports: ContentPorts }) {
  const [articles, setArticles] = useState<ArticleHead[] | null>(null);
  const [publishers, setPublishers] = useState<PublisherRecord[]>([]);
  const [error, setError] = useState("");
  const [upstreamMissing, setUpstreamMissing] = useState(false);
  const [reading, setReading] = useState<ArticleHead | null>(null);

  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const [list, pubs] = await Promise.all([ports.articles(), ports.publishers()]);
        if (!alive) return;
        setArticles(list);
        setPublishers(pubs);
      } catch (err) {
        if (!alive) return;
        if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
          setUpstreamMissing(true);
        } else {
          setError(errorText(err));
        }
        setArticles([]);
      }
    })();
    return () => {
      alive = false;
    };
  }, [ports]);

  const toggleFollow = async (publisher: PublisherRecord) => {
    setError("");
    try {
      const updated = await ports.followPublisher(publisher.publisher_id, !publisher.subscribed);
      setPublishers((prev) => prev.map((item) => (item.publisher_id === updated.publisher_id ? updated : item)));
    } catch (err) {
      setError(errorText(err));
    }
  };

  const markWatching = async (article: ArticleHead) => {
    setError("");
    try {
      await ports.markWatching(article.article_id, true);
      setReading((prev) => (prev && prev.article_id === article.article_id ? { ...prev, reading_count: (prev.reading_count ?? 0) + 1 } : prev));
    } catch (err) {
      setError(errorText(err));
    }
  };

  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4" data-testid="articles-page">
      <Banner tone="info" text={upstreamMissing ? "文章与订阅号服务（G05 ContentPort）尚未开通，以下为契约界面，暂不可读写。" : ""} />
      <Banner tone="error" text={error} />

      {publishers.length > 0 ? (
        <section aria-label="订阅号" className="flex flex-wrap gap-2">
          {publishers.map((publisher) => (
            <button
              key={publisher.publisher_id}
              type="button"
              onClick={() => void toggleFollow(publisher)}
              data-testid={`publisher-${publisher.publisher_id}`}
              className={`rounded-full border px-3 py-1 text-sm transition ${
                publisher.subscribed
                  ? "border-emerald-200 bg-emerald-50 text-emerald-700"
                  : "border-zinc-200 bg-white text-zinc-600 hover:bg-zinc-50"
              }`}
            >
              {publisher.name}
              {publisher.verified ? " ✓" : ""} · {publisher.subscribed ? "已关注" : "关注"}
            </button>
          ))}
        </section>
      ) : null}

      {articles === null ? <p className="text-sm text-zinc-400" role="status">加载中…</p> : null}
      {articles !== null && articles.length === 0 && !upstreamMissing ? (
        <EmptyState title="暂无文章" hint="关注订阅号后，新文章会出现在这里" />
      ) : null}

      {(articles ?? []).map((article) => (
        <article key={article.article_id} className="rounded-xl border border-zinc-200 bg-white p-4" data-testid={`article-${article.article_id}`}>
          <div className="flex items-start justify-between gap-2">
            <div>
              <p className="text-xs text-zinc-400">{article.publisher_name}</p>
              <h3 className="mt-0.5 text-base font-semibold text-zinc-800">{article.title}</h3>
            </div>
            <GhostButton onClick={() => setReading(article)}>阅读</GhostButton>
          </div>
        </article>
      ))}

      {reading ? (
        <div className="fixed inset-0 z-50 overflow-y-auto bg-white" role="dialog" aria-label={`阅读：${reading.title}`} data-testid="article-reader">
          <div className="mx-auto max-w-2xl p-6">
            <div className="mb-4 flex items-center justify-between">
              <h2 className="text-xl font-bold text-zinc-900">{reading.title}</h2>
              <GhostButton onClick={() => setReading(null)}>关闭</GhostButton>
            </div>
            <p className="mb-3 text-xs text-zinc-400">
              {reading.publisher_name} · v{reading.doc_version}
            </p>
            {/* Server contract: purified HTML. Client re-sanitizes before render. */}
            <div className="prose prose-zinc max-w-none text-sm leading-7 text-zinc-700" dangerouslySetInnerHTML={{ __html: sanitizeRichText(reading.doc_html) }} />
            <div className="mt-6 flex gap-2">
              <PrimaryButton
                onClick={() => {
                  void markWatching(reading);
                }}
                title="在看"
              >
                在看
              </PrimaryButton>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
