import { useState } from "react";
import { Search, X } from "lucide-react";
import { ACTIONS } from "../../shell/core-contract";
import type { SearchCategory, SearchResults } from "../../shell/models";
import { useAppState, useCoreAction } from "../../shell/store";
import { useAsync, useDebounced } from "../../shell/hooks";
import { EmptyState, ErrorState, LoadingState, cn } from "../../shell/ui";
import { openHit } from "./open-hit";

const CATEGORIES: { id: SearchCategory; label: string }[] = [
  { id: "all", label: "综合" },
  { id: "chats", label: "聊天记录" },
  { id: "contacts", label: "联系人" },
  { id: "groups", label: "群聊" },
  { id: "content", label: "内容" },
];

const KIND_LABEL: Record<string, string> = { chat: "聊天记录", contact: "联系人", group: "群聊", content: "内容" };

export function SearchPage({ initialQuery }: { initialQuery: string }) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const [query, setQuery] = useState(initialQuery);
  const [category, setCategory] = useState<SearchCategory>("all");
  const debounced = useDebounced(query, 220);
  const results = useAsync<SearchResults>(async () => {
    const text = debounced.trim();
    if (!text) return { ok: true, data: { category, hits: [] } };
    const response = await dispatch<SearchResults>(ACTIONS.searchQuery, { query: text, category }, { silent: true });
    return response.ok ? { ok: true, data: response.data } : response;
  }, [debounced, category, state.searchEpoch]);

  const hits = results.data?.hits ?? [];
  const grouped = hits.reduce<Record<string, typeof hits>>((acc, hit) => {
    acc[hit.kind] = [...(acc[hit.kind] ?? []), hit];
    return acc;
  }, {});

  return (
    <div className="flex min-h-0 flex-1 flex-col bg-[var(--surface)]">
      <header className="shrink-0 border-b border-[var(--line)] bg-[var(--surface-2)] px-4 py-3">
        <div className="flex items-center gap-2">
          <h1 className="text-[14px] font-semibold text-[var(--ink)]">搜索</h1>
          <span className="text-[9px] text-[var(--muted)]">按分类向核心查询；删除好友或退群后会立即刷新</span>
        </div>
        <div className="mt-2 flex h-9 items-center gap-2 rounded-lg border border-[var(--line-strong)] bg-[var(--surface)] px-3 focus-within:border-[var(--accent)]">
          <Search size={14} className="text-[var(--muted)]" />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="搜索聊天记录、联系人、群聊"
            aria-label="搜索"
            autoFocus
            className="flex-1 bg-transparent text-[12px] text-[var(--ink)] outline-none placeholder:text-[var(--muted)]"
            data-testid="search-input"
          />
          {query ? (
            <button type="button" aria-label="清空" onClick={() => setQuery("")} className="text-[var(--muted)] hover:text-[var(--ink)]">
              <X size={12} />
            </button>
          ) : null}
        </div>
        <div className="mt-2 flex items-center gap-1.5">
          {CATEGORIES.map((item) => (
            <button
              key={item.id}
              type="button"
              onClick={() => setCategory(item.id)}
              className={cn(
                "rounded-full px-2.5 py-1 text-[9.5px] transition",
                category === item.id ? "bg-[var(--ink)] text-[var(--app-bg)]" : "text-[var(--muted)] hover:bg-[var(--surface-3)]",
              )}
              data-testid={`search-category-${item.id}`}
            >
              {item.label}
            </button>
          ))}
          {results.data ? <span className="ml-auto text-[9px] text-[var(--muted)]">{hits.length} 条结果</span> : null}
        </div>
      </header>

      <div className="scrollbar-thin min-h-0 flex-1 overflow-y-auto px-5 py-4">
        {!debounced.trim() ? (
          <EmptyState
            icon={<Search size={20} />}
            title="输入关键词开始搜索"
            description="空关键词不会发起查询；结果来自核心的 search.query，不使用静态演示数据兜底。"
          />
        ) : results.loading ? (
          <LoadingState label="正在检索…" />
        ) : results.error ? (
          <ErrorState error={results.error} onRetry={results.reload} />
        ) : hits.length === 0 && !results.data?.unavailable?.length ? (
          <EmptyState title={`没有找到「${debounced.trim()}」`} description="换个关键词，或切换分类查看。" />
        ) : (
          <div className="mx-auto max-w-[760px] space-y-4">
            {Object.entries(grouped).map(([kind, rows]) => (
              <section key={kind}>
                <h2 className="mb-1 text-[9.5px] font-medium uppercase tracking-wide text-[var(--muted)]">{KIND_LABEL[kind] ?? kind}</h2>
                <ul className="divide-y divide-[var(--line)] overflow-hidden rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)]">
                  {rows.map((hit) => (
                    <li key={hit.id}>
                      <button
                        type="button"
                        onClick={() => openHit(hit)}
                        className="flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-[var(--surface-3)]"
                        data-testid={`search-result-${hit.id}`}
                      >
                        <span className="min-w-0 flex-1">
                          <span className="block truncate text-[11.5px] font-medium text-[var(--ink)]">{hit.title}</span>
                          <span className="block truncate text-[9.5px] text-[var(--muted)]">{hit.snippet ?? hit.subtitle}</span>
                        </span>
                        <span className="shrink-0 text-[8.5px] text-[var(--muted)]">
                          {hit.conv_id ?? hit.user_id ?? hit.group_id ?? ""}
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              </section>
            ))}
            {results.data?.unavailable?.map((item) => (
              <p key={item.category} className="rounded border border-[var(--warn)]/30 bg-[var(--warn)]/8 px-3 py-2 text-[9px] leading-5 text-[var(--warn)]" data-testid={`search-unavailable-${item.category}`}>
                {item.reason}
              </p>
            ))}
          </div>
        )}
      </div>
      <div className="flex h-7 shrink-0 items-center justify-between border-t border-[var(--line)] px-4 text-[9px] text-[var(--muted)]">
        <span>分类：{CATEGORIES.find((item) => item.id === category)?.label}</span>
        <span>搜索 epoch {state.searchEpoch} · 关系变更后自动重查</span>
      </div>
    </div>
  );
}
