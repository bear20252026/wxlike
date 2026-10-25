import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeft, CheckCircle2, QrCode, RefreshCw, ShieldCheck, Smartphone, TimerOff, XCircle } from "lucide-react";
import { ACTIONS, type DomainError } from "../../shell/core-contract";
import type { QrState } from "../../shell/models";
import { useApp, useAppState, useCoreAction } from "../../shell/store";
import { useRoute } from "../../shell/router";
import { Button, ErrorState, Spinner, cn } from "../../shell/ui";

const STATE_TEXT: Record<QrState | "LOCAL_EXPIRED", { title: string; description: string; tone: "wait" | "ok" | "bad" }> = {
  CREATED: { title: "等待扫码", description: "打开手机 wxlike，扫描二维码后批准登录。", tone: "wait" },
  SCANNED: { title: "已扫码", description: "请在手机上确认登录。桌面不会自动进入账号。", tone: "wait" },
  WAITING_APPROVAL: { title: "等待手机确认", description: "请求已送达手机，批准后由核心签发会话。", tone: "wait" },
  APPROVED: { title: "已批准", description: "核心已签发会话，正在载入投影…", tone: "ok" },
  EXPIRED: { title: "二维码已过期", description: "为安全起见，过期的 challenge 不能复用，请重新生成。", tone: "bad" },
  REJECTED: { title: "登录被拒绝", description: "手机端拒绝了本次登录，或 challenge 已被撤销。", tone: "bad" },
  LOCAL_EXPIRED: { title: "二维码已过期", description: "本地计时已到；请刷新二维码后重新扫码。", tone: "bad" },
};

export function QrLoginPage() {
  const { fixture, source } = useApp();
  const state = useAppState();
  const dispatch = useCoreAction();
  const { go } = useRoute();
  const [error, setError] = useState<DomainError | null>(null);
  const [creating, setCreating] = useState(false);
  const [localExpired, setLocalExpired] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const mounted = useRef(true);
  const qr = state.qr;

  const create = useCallback(async () => {
    setCreating(true);
    setError(null);
    setLocalExpired(false);
    const response = await dispatch(ACTIONS.qrCreate, {});
    if (!mounted.current) return;
    if (!response.ok) setError(response.error);
    setCreating(false);
  }, [dispatch]);

  useEffect(() => {
    mounted.current = true;
    void create();
    return () => {
      mounted.current = false;
    };
  }, [create]);

  useEffect(() => {
    if (state.session.phase === "authenticated") go({ id: "chats" }, { replace: true });
  }, [state.session.phase, go]);

  useEffect(() => {
    if (!qr?.expires_at_ms) return;
    const tick = () => {
      const stamp = Date.now();
      setNow(stamp);
      setLocalExpired(new Date(qr.expires_at_ms).getTime() <= stamp && !["APPROVED", "EXPIRED", "REJECTED"].includes(qr.state));
    };
    tick();
    const timer = setInterval(tick, 1000);
    return () => clearInterval(timer);
  }, [qr?.expires_at_ms, qr?.state]);

  const view = qr && localExpired && !["APPROVED", "EXPIRED", "REJECTED"].includes(qr.state) ? "LOCAL_EXPIRED" : qr?.state;
  const text = view ? STATE_TEXT[view] : null;
  const mobileConfirmed = qr?.state === "SCANNED" || qr?.state === "WAITING_APPROVAL";
  const terminal = view === "EXPIRED" || view === "REJECTED" || view === "LOCAL_EXPIRED";

  const switchAccount = async () => {
    await dispatch(ACTIONS.qrSwitchAccount, {});
    await create();
  };

  const cancelAndBack = async () => {
    await dispatch(ACTIONS.qrCancel, {}, { silent: true });
    go({ id: "login" });
  };

  return (
    <main className="grid min-h-0 flex-1 grid-cols-1 overflow-y-auto bg-[var(--app-bg)] lg:grid-cols-[1fr_1.05fr]">
      <section className="hidden flex-col justify-between bg-[var(--rail)] p-10 text-white lg:flex">
        <div className="flex items-center gap-2 text-[13px] font-semibold">
          <span className="grid h-8 w-8 place-items-center rounded-lg bg-[var(--accent)]">
            <QrCode size={16} />
          </span>
          wxlike
        </div>
        <div className="max-w-[340px]">
          <h1 className="text-[30px] font-semibold leading-tight">手机批准，桌面进入。</h1>
          <p className="mt-4 text-[12px] leading-6 text-white/60">
            扫码只是发起 challenge。只有手机端确认后，核心才会签发会话；桌面不会、也不能自行批准。
          </p>
          <ul className="mt-6 space-y-2 text-[11px] text-white/55">
            <li className="flex items-center gap-2"><Smartphone size={13} /> 六种状态全部来自核心事件</li>
            <li className="flex items-center gap-2"><ShieldCheck size={13} /> 切换账号会撤销旧 challenge 与投影</li>
            <li className="flex items-center gap-2"><RefreshCw size={13} /> 过期二维码不可复用</li>
          </ul>
        </div>
        <p className="text-[10px] text-white/35">登录后默认同步近 7 天消息，可在设置中查看更多历史。</p>
      </section>

      <section className="flex items-center justify-center px-6 py-10">
        <div className="w-full max-w-[380px]">
          <div className="flex items-center justify-between">
            <h2 className="text-[19px] font-semibold text-[var(--ink)]">扫码登录</h2>
            <button
              type="button"
              onClick={() => void cancelAndBack()}
              className="flex items-center gap-1 text-[11px] text-[var(--muted)] hover:text-[var(--ink)]"
              data-testid="qr-back"
            >
              <ArrowLeft size={12} /> 账号登录
            </button>
          </div>
          <p className="mt-1 text-[11px] text-[var(--muted)]">使用手机 wxlike 扫描下方二维码。</p>

          <div className="mt-5 rounded-xl border border-[var(--line-strong)] bg-[var(--surface-2)] p-5">
            <div className="relative mx-auto grid h-[220px] w-[220px] place-items-center overflow-hidden rounded-lg bg-white">
              {qr?.qr_svg ? (
                <img
                  src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(qr.qr_svg)}`}
                  alt="登录二维码"
                  className={cn("h-full w-full object-contain", (terminal || mobileConfirmed) && "opacity-20 blur-[2px]")}
                  data-testid="qr-image"
                />
              ) : (
                <div className="grid place-items-center text-[var(--muted)]">
                  {creating ? <Spinner label="生成中" /> : error ? <XCircle size={28} /> : <QrCode size={48} strokeWidth={1} />}
                </div>
              )}
              {view === "APPROVED" ? (
                <span className="absolute grid place-items-center text-[var(--accent)]"><CheckCircle2 size={44} /></span>
              ) : terminal ? (
                <span className="absolute grid place-items-center text-[var(--danger)]"><TimerOff size={40} /></span>
              ) : mobileConfirmed ? (
                <span className="absolute flex flex-col items-center gap-1 text-[11px] font-medium text-[var(--ink)]">
                  <Smartphone size={26} />
                  请在手机上确认
                </span>
              ) : null}
            </div>

            <div className="mt-4 text-center" role="status" aria-live="polite">
              {error ? (
                <div className="text-[11px] text-[var(--danger)]">二维码生成失败：{error.message}</div>
              ) : text ? (
                <>
                  <div
                    className={cn(
                      "text-[13px] font-medium",
                      text.tone === "ok" ? "text-[var(--accent-ink)]" : text.tone === "bad" ? "text-[var(--danger)]" : "text-[var(--ink)]",
                    )}
                    data-testid="qr-state"
                  >
                    {text.title}
                  </div>
                  <p className="mt-1 text-[10.5px] leading-4 text-[var(--muted)]">{text.description}</p>
                </>
              ) : (
                <div className="text-[11px] text-[var(--muted)]">正在向核心申请 challenge…</div>
              )}
              {qr && !terminal && !error ? (
                <p className="mt-2 text-[9px] tabular-nums text-[var(--muted)]" data-testid="qr-countdown">
                  challenge {qr.challenge_id} · 剩余 {Math.max(0, Math.ceil((new Date(qr.expires_at_ms).getTime() - now) / 1000))}s
                </p>
              ) : null}
            </div>

            <div className="mt-4 flex items-center justify-center gap-2">
              <Button size="sm" onClick={() => void create()} pending={creating} data-testid="qr-refresh">
                <RefreshCw size={12} /> {terminal ? "重新生成" : "刷新二维码"}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => void switchAccount()} data-testid="qr-switch">
                切换账号
              </Button>
            </div>
          </div>

          {source === "fixture" && fixture && import.meta.env.DEV ? (
            <div className="mt-4 rounded-lg border border-dashed border-[var(--warn)]/50 bg-[var(--warn)]/5 p-3">
              <p className="text-[9.5px] leading-4 text-[var(--warn)]">
                开发夹具：以下按钮只在 DEV 环境出现，直接调用测试钩子模拟手机事件，不是产品能力，也不产生可验收的会话签发。
              </p>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {(["SCANNED", "WAITING_APPROVAL", "APPROVED", "EXPIRED", "REJECTED"] as const).map((next) => (
                  <button
                    key={next}
                    type="button"
                    onClick={() => fixture.simulateQr(next)}
                    className="rounded border border-[var(--line-strong)] bg-[var(--surface)] px-2 py-1 text-[9px] text-[var(--ink-2)] hover:bg-[var(--surface-3)]"
                    data-testid={`qr-sim-${next.toLowerCase()}`}
                  >
                    模拟 {next}
                  </button>
                ))}
              </div>
            </div>
          ) : null}

          {error && !qr ? (
            <div className="mt-3">
              <ErrorState error={error} compact onRetry={() => void create()} />
            </div>
          ) : null}

          <p className="mt-6 border-t border-[var(--line)] pt-3 text-[9.5px] leading-4 text-[var(--muted)]">
            扫码成功不会自动登录；批准、过期、拒绝都由核心状态机推进。桌面端的确认按钮不会出现在生产构建中。
          </p>
        </div>
      </section>
    </main>
  );
}
