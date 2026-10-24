/**
 * 收藏 (G11-01): list / search / sort / filter, detail actions — 编辑/复制/分享/删除.
 * Copy goes through PlatformPort (requirement 4), not navigator directly.
 */

import { useEffect, useMemo, useState } from "react";
import { errorText } from "../../lib/http";
import { platform } from "../../lib/platform";
import type { ContentPorts, FavoriteKindFilter, FavoriteRecord, FavoriteSortKey } from "./model";
import { Banner, DotMenu, EmptyState, GhostButton, Modal, PrimaryButton, formatTime } from "./ui";

const KIND_LABELS: Record<string, string> = {
  text: "文本",
  image: "图片",
  file: "文件",
  link: "链接",
  voice: "语音",
  video: "视频",
};

export function FavoritesPage({ ports }: { ports: ContentPorts }) {
  const [records, setRecords] = useState<FavoriteRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [keyword, setKeyword] = useState("");
  const [kind, setKind] = useState<FavoriteKindFilter>("all");
  const [sort, setSort] = useState<FavoriteSortKey>("newest");
  const [editing, setEditing] = useState<FavoriteRecord | null>(null);
  const [note, setNote] = useState("");

  useEffect(() => {
    let alive = true;
    setLoading(true);
    void ports
      .listFavorites()
      .then((items) => {
        if (alive) setRecords(items);
      })
      .catch((err: unknown) => {
        if (alive) setError(errorText(err));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [ports]);

  const visible = useMemo(() => {
    const kw = keyword.trim().toLowerCase();
    const filtered = records.filter((record) => {
      if (kind !== "all" && record.kind !== kind) return false;
      if (!kw) return true;
      return (
        record.text.toLowerCase().includes(kw) ||
        record.sender.toLowerCase().includes(kw) ||
        record.source_conv.toLowerCase().includes(kw)
      );
    });
    return [...filtered].sort((a, b) => {
      if (sort === "oldest") return a.created_at - b.created_at;
      if (sort === "sender") return a.sender.localeCompare(b.sender) || b.created_at - a.created_at;
      return b.created_at - a.created_at;
    });
  }, [records, keyword, kind, sort]);

  const remove = async (record: FavoriteRecord) => {
    setError("");
    try {
      await ports.removeFavorite(record.id);
      setRecords((prev) => prev.filter((item) => item.id !== record.id));
    } catch (err) {
      setError(errorText(err));
    }
  };

  const copy = async (record: FavoriteRecord) => {
    const ok = await platform().copyText(record.text);
    if (!ok) setError("复制失败");
  };

  const share = async (record: FavoriteRecord) => {
    // 分享 = 把收藏快照转发到会话，通过 PlatformPort 打开系统分享面（web 端为复制 + 打开提示）。
    const ok = await platform().copyText(`【收藏分享】${record.text}`);
    if (!ok) setError("分享失败");
  };

  const saveEdit = async () => {
    if (!editing) return;
    setError("");
    try {
      const updated = await ports.updateFavoriteNote(editing.id, note);
      setRecords((prev) => prev.map((item) => (item.id === updated.id ? updated : item)));
      setEditing(null);
    } catch (err) {
      setError(errorText(err));
    }
  };

  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4" data-testid="favorites-page">
      <div className="flex flex-wrap items-center gap-2">
        <input
          value={keyword}
          onChange={(event) => setKeyword(event.target.value)}
          placeholder="搜索收藏…"
          data-testid="favorites-search"
          className="min-w-48 flex-1 rounded-lg border border-zinc-200 px-3 py-1.5 text-sm outline-none focus:border-emerald-500"
        />
        <select
          value={kind}
          onChange={(event) => setKind(event.target.value as FavoriteKindFilter)}
          data-testid="favorites-kind"
          className="rounded-lg border border-zinc-200 px-2 py-1.5 text-sm"
        >
          <option value="all">全部类型</option>
          {Object.entries(KIND_LABELS).map(([value, label]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
        <select
          value={sort}
          onChange={(event) => setSort(event.target.value as FavoriteSortKey)}
          data-testid="favorites-sort"
          className="rounded-lg border border-zinc-200 px-2 py-1.5 text-sm"
        >
          <option value="newest">最新收藏</option>
          <option value="oldest">最早收藏</option>
          <option value="sender">按来源</option>
        </select>
      </div>

      <Banner tone="error" text={error} />
      {loading ? <p className="text-sm text-zinc-400" role="status">加载中…</p> : null}
      {!loading && visible.length === 0 && error === "" ? (
        <EmptyState title="暂无收藏" hint="在聊天中长按消息即可收藏" />
      ) : null}

      {visible.map((record) => (
        <article key={record.id} className="rounded-xl border border-zinc-200 bg-white p-3.5" data-testid={`favorite-${record.id}`}>
          <header className="flex items-start justify-between gap-2">
            <div className="text-xs text-zinc-400">
              <span className="rounded bg-zinc-100 px-1.5 py-0.5">{KIND_LABELS[record.kind] ?? record.kind}</span>
              <span className="ml-2">
                来自 {record.sender} · {formatTime(record.created_at)}
              </span>
            </div>
            <DotMenu
              items={[
                { label: "编辑", onSelect: () => { setEditing(record); setNote(record.text); } },
                { label: "复制", onSelect: () => void copy(record) },
                { label: "分享", onSelect: () => void share(record) },
                { label: "删除", onSelect: () => void remove(record), danger: true },
              ]}
            />
          </header>
          <p className="mt-2 whitespace-pre-wrap text-sm text-zinc-700">{record.text}</p>
        </article>
      ))}

      {editing ? (
        <Modal title="编辑收藏" onClose={() => setEditing(null)}>
          <div className="space-y-3">
            <textarea
              value={note}
              onChange={(event) => setNote(event.target.value)}
              rows={5}
              data-testid="favorite-edit-text"
              className="w-full resize-none rounded-lg border border-zinc-200 p-2.5 text-sm outline-none focus:border-emerald-500"
            />
            <div className="flex justify-end gap-2">
              <GhostButton onClick={() => setEditing(null)}>取消</GhostButton>
              <PrimaryButton onClick={() => void saveEdit()}>保存</PrimaryButton>
            </div>
          </div>
        </Modal>
      ) : null}
    </div>
  );
}
