import { useEffect, useState } from "react";
import { ACTIONS } from "../../shell/core-contract";
import { useCoreAction } from "../../shell/store";
import { useAsync } from "../../shell/hooks";
import { cn } from "../../shell/ui";

interface SyncRange {
  days: 0 | 7 | 30 | 90;
  incremental_default_days: number;
  can_view_further: boolean;
}

const OPTIONS: { value: SyncRange["days"]; label: string; hint: string }[] = [
  { value: 7, label: "近 7 天", hint: "默认增量，首次登录最快" },
  { value: 30, label: "近 30 天", hint: "补齐一个月历史" },
  { value: 90, label: "近 90 天", hint: "补齐三个月历史" },
  { value: 0, label: "全部历史", hint: "耗时与流量最大，可随时取消" },
];

export function SyncRangeControl() {
  const dispatch = useCoreAction();
  const state = useAsync<SyncRange>(() => dispatch(ACTIONS.syncRangeGet, {}), []);
  const [days, setDays] = useState<SyncRange["days"]>(7);
  const [saving, setSaving] = useState<SyncRange["days"] | null>(null);

  useEffect(() => {
    if (state.data) setDays(state.data.days);
  }, [state.data]);

  const save = async (next: SyncRange["days"]) => {
    if (next === days || saving !== null) return;
    setSaving(next);
    const response = await dispatch<{ sync_range_days: SyncRange["days"] }>(ACTIONS.syncRangeSet, { days: next }, {
      idempotencyKey: `sync-range:${next}`,
    });
    if (response.ok) {
      setDays(next);
      state.reload();
    }
    setSaving(null);
  };

  return (
    <div>
      <div className="flex items-center justify-between">
        <div>
          <div className="text-[12px] font-medium text-[var(--ink)]">同步范围</div>
          <p className="mt-0.5 text-[10px] leading-4 text-[var(--muted)]">
            {state.data
              ? `默认先同步近 ${state.data.incremental_default_days} 天增量，再按选择补齐历史。`
              : state.loading
                ? "正在读取同步范围…"
                : state.error?.message ?? "同步范围不可用"}
          </p>
        </div>
        {state.loading ? <span className="text-[9.5px] text-[var(--muted)]">读取中…</span> : null}
      </div>
      <div className="mt-2 grid grid-cols-2 gap-1.5" role="radiogroup" aria-label="同步范围">
        {OPTIONS.map((option) => (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={days === option.value}
            disabled={Boolean(state.error)}
            onClick={() => void save(option.value)}
            data-testid={`sync-range-${option.value}`}
            className={cn(
              "rounded-lg border px-2.5 py-2 text-left transition disabled:opacity-50",
              days === option.value
                ? "border-[var(--accent)] bg-[var(--accent-soft)]"
                : "border-[var(--line-strong)] bg-[var(--surface)] hover:bg-[var(--surface-3)]",
            )}
          >
            <span className="flex items-center justify-between text-[11px] font-medium text-[var(--ink)]">
              {option.label}
              {saving === option.value ? <span className="text-[9px] text-[var(--muted)]">保存中</span> : null}
            </span>
            <span className="mt-0.5 block text-[9px] leading-4 text-[var(--muted)]">{option.hint}</span>
          </button>
        ))}
      </div>
      {state.error ? (
        <button type="button" onClick={state.reload} className="mt-1.5 text-[9.5px] text-[var(--accent-ink)] hover:underline">
          重试读取
        </button>
      ) : null}
    </div>
  );
}
