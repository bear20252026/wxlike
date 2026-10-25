/**
 * 表情 (G11-01): sticker packs via contract (sticker.*), recent stickers kept
 * locally. Selecting a sticker reports back through `onPick` (chat composer
 * consumes it; G10 owns the chat surface — this panel only owns the picker).
 */

import { useEffect, useState } from "react";
import { loadDraft, saveDraft } from "../../lib/draft";
import type { ContentPorts, StickerPack } from "./model";

const RECENT_SCOPE = "sticker-recent";
const RECENT_MAX = 16;

export function StickerPanel({
  ports,
  onPick,
}: {
  ports: ContentPorts;
  onPick: (sticker: string) => void;
}) {
  const [packs, setPacks] = useState<StickerPack[]>([]);
  const [active, setActive] = useState<string>("");
  const [recent, setRecent] = useState<string[]>(() => loadDraft<string[]>(RECENT_SCOPE) ?? []);
  const [error, setError] = useState("");

  useEffect(() => {
    let alive = true;
    void ports
      .stickerPacks()
      .then((list) => {
        if (!alive) return;
        setPacks(list);
        setActive(list[0]?.pack_id ?? "");
      })
      .catch((err: unknown) => {
        if (alive) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      alive = false;
    };
  }, [ports]);

  const pick = (sticker: string) => {
    const next = [sticker, ...recent.filter((item) => item !== sticker)].slice(0, RECENT_MAX);
    setRecent(next);
    saveDraft(RECENT_SCOPE, next);
    onPick(sticker);
  };

  const current = packs.find((pack) => pack.pack_id === active) ?? packs[0];

  return (
    <div className="w-72 rounded-xl border border-zinc-200 bg-white p-2" data-testid="sticker-panel">
      {recent.length > 0 ? (
        <section aria-label="最近使用" className="mb-2">
          <p className="px-1 pb-1 text-xs text-zinc-400">最近使用</p>
          <div className="grid grid-cols-8 gap-0.5">
            {recent.map((sticker) => (
              <button key={`recent-${sticker}`} type="button" onClick={() => pick(sticker)} className="rounded-md p-1 text-xl hover:bg-zinc-100">
                {sticker}
              </button>
            ))}
          </div>
        </section>
      ) : null}
      {error ? <p className="px-1 text-xs text-red-500">{error}</p> : null}
      {packs.length > 1 ? (
        <div className="mb-1 flex gap-1 overflow-x-auto px-1" role="tablist">
          {packs.map((pack) => (
            <button
              key={pack.pack_id}
              type="button"
              role="tab"
              aria-selected={pack.pack_id === current?.pack_id}
              onClick={() => setActive(pack.pack_id)}
              className={`whitespace-nowrap rounded-md px-2 py-0.5 text-xs ${pack.pack_id === current?.pack_id ? "bg-emerald-50 text-emerald-700" : "text-zinc-500 hover:bg-zinc-100"}`}
            >
              {pack.name}
            </button>
          ))}
        </div>
      ) : null}
      <div className="grid max-h-56 grid-cols-8 gap-0.5 overflow-y-auto" data-testid="sticker-grid">
        {(current?.items ?? []).map((sticker, index) => (
          <button key={`${current?.pack_id}-${index}`} type="button" onClick={() => pick(sticker)} className="rounded-md p-1 text-xl hover:bg-zinc-100">
            {sticker}
          </button>
        ))}
      </div>
    </div>
  );
}
