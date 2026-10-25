import { useState } from "react";
import { ArrowLeftRight, LockKeyhole, ShieldCheck } from "lucide-react";
import { ACTIONS, type DomainError } from "../../shell/core-contract";
import type { SessionProjection } from "../../shell/models";
import { useAppState, useCoreAction } from "../../shell/store";
import { useRoute } from "../../shell/router";
import { Avatar, Button, InlineError, TextField } from "../../shell/ui";
import { usePending } from "../../shell/hooks";

const REASON_TEXT: Record<NonNullable<SessionProjection["lock_reason"]>, string> = {
  manual: "你手动锁定了应用，内存中的凭据已清除。",
  resume: "应用从后台恢复，需要重新验证。",
  timeout: "长时间未操作，已自动锁定。",
};

export function LockScreen() {
  const state = useAppState();
  const dispatch = useCoreAction();
  const { go } = useRoute();
  const { pending, run } = usePending();
  const [password, setPassword] = useState("");
  const [error, setError] = useState<DomainError | null>(null);
  const account = state.session.account;

  const unlock = async () => {
    setError(null);
    if (!password) {
      setError({ code: "VALIDATION", message: "请输入口令", retryable: false, request_id: "local", field_errors: { password: "请输入口令" } });
      return;
    }
    await run(async () => {
      const response = await dispatch<SessionProjection>(ACTIONS.unlock, { password });
      setPassword("");
      if (response.ok) go({ id: "chats" }, { replace: true });
      else setError(response.error);
    });
  };

  const switchAccount = async () => {
    await dispatch(ACTIONS.logout, {}, { silent: true });
    go({ id: "login" }, { replace: true });
  };

  return (
    <main className="flex min-h-0 flex-1 items-center justify-center bg-[var(--app-bg)] px-6 py-10">
      <div className="w-full max-w-[360px]">
        <div className="flex flex-col items-center text-center">
          <Avatar src={account?.avatar} name={account?.name ?? "我"} size={64} rounded="rounded-[14px]" />
          <h1 className="mt-4 text-[18px] font-semibold text-[var(--ink)]">{account?.name ?? "wxlike"}</h1>
          <p className="mt-1 text-[10.5px] leading-5 text-[var(--muted)]">
            {state.session.lock_reason ? REASON_TEXT[state.session.lock_reason] : "应用已锁定。"}
          </p>
        </div>

        <form
          className="mt-6 rounded-xl border border-[var(--line-strong)] bg-[var(--surface-2)] p-5"
          onSubmit={(event) => {
            event.preventDefault();
            void unlock();
          }}
          noValidate
        >
          <div className="mb-3 flex items-center gap-2 text-[11px] text-[var(--ink-2)]">
            <LockKeyhole size={13} /> 解锁需要向核心验证口令
          </div>
          <TextField
            label="口令"
            id="unlock-password"
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            error={error?.field_errors?.password}
            autoFocus
            data-testid="unlock-password"
          />
          <InlineError error={error && !error.field_errors ? error : null} />
          <Button type="submit" variant="primary" size="lg" className="mt-3 w-full" pending={pending} data-testid="unlock-submit">
            解锁
          </Button>
          <button
            type="button"
            onClick={() => void switchAccount()}
            className="mt-3 flex w-full items-center justify-center gap-1.5 text-[10.5px] text-[var(--muted)] hover:text-[var(--ink)]"
            data-testid="unlock-switch"
          >
            <ArrowLeftRight size={11} /> 使用其他账号
          </button>
        </form>

        <p className="mt-4 flex items-start gap-1.5 text-[9.5px] leading-4 text-[var(--muted)]">
          <ShieldCheck size={12} className="mt-0.5 shrink-0" />
          口令只发送给 Rust 核心校验，界面不缓存、不写 localStorage，也不在日志中输出。
        </p>
      </div>
    </main>
  );
}
