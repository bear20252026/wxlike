import { useEffect, useState } from "react";
import { KeyRound, QrCode, ShieldCheck } from "lucide-react";
import { ACTIONS, type DomainError } from "../../shell/core-contract";
import type { SessionProjection } from "../../shell/models";
import { useApp, useAppState, useCoreAction } from "../../shell/store";
import { useRoute } from "../../shell/router";
import { Button, InlineError, TextField } from "../../shell/ui";
import { usePending } from "../../shell/hooks";

export function LoginPage() {
  const { platform } = useApp();
  const state = useAppState();
  const dispatch = useCoreAction();
  const { go } = useRoute();
  const { pending, run } = usePending();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<DomainError | null>(null);
  const capabilities = platform.capabilities();
  const osBackedSecrets = capabilities.secrets.storage === "os_keychain";

  useEffect(() => {
    if (state.session.phase === "authenticated") go({ id: "chats" }, { replace: true });
  }, [state.session.phase, go]);

  const submit = async () => {
    const errors: Record<string, string> = {};
    if (!username.trim()) errors.username = "请输入账号";
    if (!password) errors.password = "请输入口令";
    setFieldErrors(errors);
    setError(null);
    if (Object.keys(errors).length) return;
    await run(async () => {
      const response = await dispatch<SessionProjection>(
        ACTIONS.login,
        {
          username: username.trim(),
          password,
          remember: remember && osBackedSecrets,
          device_label: navigator.userAgent.slice(0, 120),
        },
        { idempotencyKey: `login:${username.trim()}` },
      );
      if (response.ok) {
        setPassword("");
        go({ id: "chats" }, { replace: true });
      } else {
        setError(response.error);
        setPassword("");
        document.getElementById("login-password")?.focus();
      }
    });
  };

  return (
    <main className="grid min-h-0 flex-1 grid-cols-[1.05fr_1fr] overflow-hidden bg-[var(--app-bg)]">
      <section className="relative hidden flex-col justify-between overflow-hidden bg-[var(--rail)] p-10 text-white md:flex">
        <div className="flex items-center gap-2 text-[13px] font-semibold tracking-wide">
          <span className="grid h-8 w-8 place-items-center rounded-lg bg-[var(--accent)] text-white">
            <QrCode size={16} />
          </span>
          wxlike
        </div>
        <div className="max-w-[360px]">
          <h1 className="text-[34px] font-semibold leading-[1.15] tracking-tight">
            沟通，
            <br />
            回到简单。
          </h1>
          <p className="mt-4 text-[12px] leading-6 text-white/60">
            桌面与网页共用同一套 Rust 核心投影。登录、锁定与恢复都由核心给出状态，界面只负责呈现与操作。
          </p>
        </div>
        <ul className="space-y-2 text-[11px] text-white/55">
          <li className="flex items-center gap-2">
            <ShieldCheck size={14} /> 口令不写入 localStorage 或日志
          </li>
          <li className="flex items-center gap-2">
            <KeyRound size={14} /> 自动登录仅在系统密钥库可用时开启
          </li>
        </ul>
      </section>

      <section className="flex items-center justify-center px-8 py-10">
        <div className="w-full max-w-[340px]">
          <h2 className="text-[20px] font-semibold text-[var(--ink)]">登录 wxlike</h2>
          <p className="mt-1 text-[11px] text-[var(--muted)]">使用账号口令，或在手机上扫码批准。</p>

          <form
            className="mt-6 space-y-3"
            onSubmit={(event) => {
              event.preventDefault();
              void submit();
            }}
            noValidate
          >
            <TextField
              label="账号"
              name="username"
              autoComplete="username"
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              placeholder="账号或昵称"
              error={fieldErrors.username}
              autoFocus
              data-testid="login-username"
            />
            <TextField
              label="口令"
              id="login-password"
              name="password"
              type="password"
              autoComplete="current-password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              placeholder="输入口令"
              error={fieldErrors.password}
              data-testid="login-password"
            />
            <label className="flex items-start gap-2 pt-1 text-[10.5px] leading-4 text-[var(--ink-2)]">
              <input
                type="checkbox"
                className="mt-0.5 accent-[var(--accent)]"
                checked={remember}
                disabled={!osBackedSecrets}
                onChange={(event) => setRemember(event.target.checked)}
                data-testid="login-remember"
              />
              <span>
                在此设备自动登录
                <span className="block text-[9.5px] text-[var(--muted)]">
                  {osBackedSecrets
                    ? "凭据由系统密钥库保存；注销时撤销"
                    : "浏览器无法访问系统密钥库：本次会话有效，不跨重启保存"}
                </span>
              </span>
            </label>
            <InlineError error={error} />
            <Button
              type="submit"
              variant="primary"
              size="lg"
              className="w-full"
              pending={pending}
              data-testid="login-submit"
            >
              登录
            </Button>
          </form>

          <div className="mt-5 flex items-center justify-between text-[11px]">
            <button
              type="button"
              className="text-[var(--accent-ink)] hover:underline"
              onClick={() => go({ id: "qr_login" })}
              data-testid="login-to-qr"
            >
              手机扫码登录
            </button>
            <button
              type="button"
              className="text-[var(--muted)] hover:text-[var(--ink)]"
              onClick={() => go({ id: "restore" })}
            >
              恢复上次会话
            </button>
          </div>

          <p className="mt-8 border-t border-[var(--line)] pt-3 text-[9.5px] leading-4 text-[var(--muted)]">
            {state.capabilities?.notes?.[0] ?? "登录结果以 Rust 核心返回的会话投影为准，界面不自行签发会话。"}
          </p>
        </div>
      </section>
    </main>
  );
}

/** Used inside settings for the same credential-policy explanation. */
export function CredentialPolicyNote() {
  const { platform } = useApp();
  const caps = platform.capabilities();
  return (
    <p className="text-[10px] leading-4 text-[var(--muted)]">
      {caps.secrets.storage === "os_keychain"
        ? "自动登录令牌只写入系统密钥库；注销时由核心撤销会话。"
        : "当前平台没有系统密钥库：只保留会话级凭据，关闭页面即失效；口令永不写入 localStorage。"}
    </p>
  );
}
