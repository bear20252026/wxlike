import { useEffect, useRef, useState } from "react";
import { Search, X } from "lucide-react";
import type { SearchHit, SearchResults } from "../../shell/models";
import { useAppState, useCoreAction } from "../../shell/store";
import { ACTIONS } from "../../shell/core-contract";
import { useDebounced } from "../../shell/hooks";
import { openHit } from "./open-hit";

export function SearchOverlay({ open, onClose }: { open: boolean; onClose: () => void }) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const [query, setQuery] = useState("");
  const debounced = useDebounced(query, 180);
  const [results, setResults] = useState<SearchResults | null>(null);
  const [loading, setLoading] = useState(false);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const ticket = useRef(0);

  useEffect(() => {
    if (open) {
      setQuery("");
      setResults(null);
      setActive(0);
      setTimeout(() => inputRef.current?.focus(), 0);
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  useEffect(() => {
    if (!open) return;
    const text = debounced.trim();
    if (!text) {
      setResults(null);
      setLoading(false);
      return;
    }
    const current = ++ticket.current;
    setLoading(true);
    void dispatch<SearchResults>(ACTIONS.searchQuery, { query: text, category: "all" }, { silent: true }).then((response) => {
      if (current !== ticket.current) return;
      setResults(response.ok ? response.data : { category: "all", hits: [] });
      setActive(0);
      setLoading(false);
    });
  }, [debounced, open, dispatch, state.searchEpoch]);

  if (!open) return null;
  const hits = results?.hits.slice(0, 8) ?? [];

  const pick = (hit: SearchHit) => {
    onClose();
    openHit(hit);
  };

  return (
    <div className="fixed inset-0 z-[70] flex items-start justify-center bg-black/35 px-6 pt-[70px] backdrop-blur-[2px]" onMouseDown={(event) => event.target === event.currentTarget && onClose()} data-testid="search-overlay">
      <div className="pop-in w-full max-w-[560px] overflow-hidden rounded-xl border border-[var(--line-strong)] bg-[var(--surface-2)] shadow-[var(--shadow-pop)]">
        <div className="flex h-[50px] items-center gap-2 border-b border-[var(--line)] px-4">
          <Search size={15} className="text-[var(--muted)]" />
          <input
            ref={inputRef}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setActive((index) => Math.min(hits.length - 1, index + 1));
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setActive((index) => Math.max(0, index - 1));
              } else if (event.key === "Enter" && hits[active]) {
                event.preventDefault();
                pick(hits[active]);
              }
            }}
            placeholder="搜索聊天、联系人、群聊…"
            aria-label="全局搜索"
            className="flex-1 bg-transparent text-[13px] text-[var(--ink)] outline-none placeholder:text-[var(--muted)]"
            data-testid="search-overlay-input"
          />
          {loading ? <span className="text-[9px] text-[var(--muted)]">搜索中</span> : null}
          <button type="button" onClick={onClose} aria-label="关闭搜索" className="text-[var(--muted)] hover:text-[var(--ink)]" data-testid="search-overlay-close">
            <X size={14} />
          </button>
          <kbd className="rounded border border-[var(--line-strong)] bg-[var(--surface)] px-1.5 py-0.5 text-[8px] text-[var(--muted)]">ESC</kbd>
        </div>

        <div className="max-h-[360px] overflow-y-auto scrollbar-thin p-1.5">
          {!debounced.trim() ? (
            <div className="px-3 py-6 text-center text-[10px] text-[var(--muted)]">
              输入关键词开始搜索；结果按分类由核心查询，过期会话与已退群不会出现在结果里。
            </div>
          ) : hits.length === 0 ? (
            <div className="px-3 py-6 text-center text-[10px] text-[var(--muted)]">
              {loading ? "正在查询…" : `没有找到「${debounced.trim()}」相关结果`}
            </div>
          ) : (
            hits.map((hit, index) => (
              <button
                key={hit.id}
                type="button"
                onMouseEnter={() => setActive(index)}
                onClick={() => pick(hit)}
                className={`flex w-full items-center gap-2 rounded-md px-2.5 py-2 text-left ${
                  index === active ? "bg-[var(--accent-soft)]" : "hover:bg-[var(--surface-3)]"
                }`}
                data-testid={`search-hit-${hit.id}`}
              >
                <span className="w-10 shrink-0 rounded bg-[var(--surface-3)] px-1 py-0.5 text-center text-[8px] text-[var(--muted)]">
                  {hit.kind === "chat" ? "聊天" : hit.kind === "contact" ? "联系人" : hit.kind === "group" ? "群聊" : "内容"}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[11px] text-[var(--ink)]">{hit.title}</span>
                  <span className="block truncate text-[9px] text-[var(--muted)]">{hit.snippet ?? hit.subtitle}</span>
                </span>
              </button>
            ))
          )}
          {results?.unavailable?.map((item) => (
            <p key={item.category} className="mt-1 rounded bg-[var(--warn)]/8 px-2.5 py-1.5 text-[8.5px] leading-4 text-[var(--warn)]" data-testid={`search-unavailable-${item.category}`}>
              {item.reason}
            </p>
          ))}
        </div>
        <div className="flex items-center justify-between border-t border-[var(--line)] px-3 py-1.5 text-[8.5px] text-[var(--muted)]">
          <span>↑↓ 选择 · Enter 打开</span>
          <span>⌘K / Ctrl+K 打开</span>
        </div>
      </div>
    </div>
  );
}
