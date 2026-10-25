import { useState } from "react";
import { ArrowLeft, History, ShieldCheck } from "lucide-react";
import { ACTIONS, type DomainError } from "../../shell/core-contract";
import type { SessionProjection } from "../../shell/models";
import { useAppState, useCoreAction } from "../../shell/store";
import { useRoute } from "../../shell/router";
import { Button, EmptyState, InlineError, Spinner } from "../../shell/ui";
import { usePending } from "../../shell/hooks";

export function RestorePage() {
  const state = useAppState();
  const dispatch = useCoreAction();
  const { go } = useRoute();
  const { pending, run } = usePending();
  const [error, setError] = useState<DomainError | null>(null);
  const [attempted, setAttempted] = useState(false);
  const days = state.preferences?.sync_range_days ?? 7;
  const range = days === 0 ? "全部历史" : `近 ${days} 天`;

  const restore = async () => {
    setError(null);
    await run(async () => {
      const response = await dispatch<SessionProjection>(ACTIONS.restore, {});
      setAttempted(true);
      if (response.ok) go({ id: "chats" }, { replace: true });
      else setError(response.error);
    });
  };

  return (
    <main className="flex min-h-0 flex-1 items-center justify-center overflow-y-auto bg-[var(--app-bg)] px-6 py-10">
      <div className="w-full max-w-[400px]">
        <div className="flex items-center justify-between">
          <h1 className="text-[19px] font-semibold text-[var(--ink)]">恢复会话</h1>
          <button
            type="button"
            onClick={() => go({ id: "login" })}
            className="flex items-center gap-1 text-[11px] text-[var(--muted)] hover:text-[var(--ink)]"
          >
            <ArrowLeft size={12} /> 返回登录
          </button>
        </div>
        <p className="mt-1 text-[11px] leading-5 text-[var(--muted)]">
          恢复使用系统密钥库中的会话令牌，由核心校验后返回投影。浏览器端没有密钥库，只能恢复当前标签页的会话。
        </p>

        <div className="mt-5 rounded-xl border border-[var(--line-strong)] bg-[var(--surface-2)] p-5">
          <div className="flex items-start gap-3">
            <span className="grid h-9 w-9 place-items-center rounded-lg bg-[var(--accent-soft)] text-[var(--accent-ink)]">
              <History size={16} />
            </span>
            <div className="text-[10.5px] leading-5 text-[var(--ink-2)]">
              <p className="text-[12px] font-medium text-[var(--ink)]">默认同步 {range}</p>
              <p className="mt-1">恢复后先载入近期增量；更早历史需要调整同步范围，补齐过程可取消也可断点继续。</p>
            </div>
          </div>

          {attempted && error ? (
            <div className="mt-4">
              <EmptyState
                icon={<ShieldCheck size={18} />}
                title="没有可恢复的会话"
                description={`核心返回：${error.message}。请回到登录页重新验证口令或扫码。`}
              />
              <div className="flex justify-center">
                <Button size="sm" variant="primary" onClick={() => go({ id: "login" })} data-testid="restore-to-login">
                  去登录
                </Button>
              </div>
            </div>
          ) : (
            <>
              <Button className="mt-4 w-full" variant="primary" size="lg" pending={pending} onClick={() => void restore()} data-testid="restore-submit">
                {pending ? <Spinner label="正在向核心校验" /> : "恢复上次会话"}
              </Button>
              <InlineError error={error} />
            </>
          )}
        </div>

        <p className="mt-4 text-[9.5px] leading-4 text-[var(--muted)]">
          恢复失败不会伪造登录态；界面只显示核心返回的会话投影。
        </p>
      </div>
    </main>
  );
}
