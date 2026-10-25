import { useEffect, useState, type ReactNode } from "react";
import {
  AppWindow,
  Camera,
  Clipboard,
  FolderOpen,
  Keyboard,
  KeyRound,
  Power,
  Bell,
  PanelTop,
} from "lucide-react";
import type { PlatformCapability, PlatformResult } from "../../shell/platform/port";
import { useApp, useAppState, useToast } from "../../shell/store";
import { Button, cn } from "../../shell/ui";

function CapabilityRow({
  title,
  detail,
  capability,
  icon,
  children,
}: {
  title: string;
  detail: string;
  capability: PlatformCapability;
  icon: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="flex items-start gap-3 border-b border-[var(--line)] py-2.5 last:border-0">
      <span
        className={cn(
          "mt-0.5 grid h-7 w-7 shrink-0 place-items-center rounded-md",
          capability.supported
            ? "bg-[var(--accent-soft)] text-[var(--accent-ink)]"
            : "bg-[var(--surface-3)] text-[var(--muted)]",
        )}
      >
        {icon}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-[11.5px] text-[var(--ink)]">{title}</span>
          <span
            className={cn(
              "rounded px-1.5 py-0.5 text-[8.5px]",
              capability.supported
                ? "bg-[var(--accent-soft)] text-[var(--accent-ink)]"
                : "bg-[var(--surface-3)] text-[var(--muted)]",
            )}
            data-testid={`platform-cap-${title}`}
          >
            {capability.supported ? "可用" : "不可用"}
          </span>
        </div>
        <p className="mt-0.5 text-[9.5px] leading-4 text-[var(--muted)]">
          {capability.supported ? detail : capability.reason ?? "当前平台没有该能力"}
        </p>
      </div>
      {children ? <div className="shrink-0">{children}</div> : null}
    </div>
  );
}

/** Turns a PlatformResult into a message the user can act on; never fakes success. */
function resultText(result: PlatformResult<unknown>, success: string): { tone: "info" | "error"; text: string } {
  return result.ok ? { tone: "info", text: success } : { tone: "error", text: result.message };
}

const TEST_SHORTCUT = "CommandOrControl+Alt+9";

export function PlatformSection() {
  const { platform } = useApp();
  const state = useAppState();
  const toast = useToast();
  const caps = platform.capabilities();
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [autostartNote, setAutostartNote] = useState<string | null>(null);
  const [shortcutRegistered, setShortcutRegistered] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    if (!caps.autostart.supported) return;
    void platform.autostart.get().then((result) => {
      if (!alive) return;
      if (result.ok) setAutostart(result.value);
      else setAutostartNote(result.message);
    });
    return () => {
      alive = false;
    };
  }, [platform, caps.autostart.supported]);

  const run = async (key: string, task: () => Promise<void>) => {
    if (busy) return;
    setBusy(key);
    try {
      await task();
    } finally {
      setBusy(null);
    }
  };

  const toggleAutostart = () =>
    run("autostart", async () => {
      const next = !autostart;
      const result = await platform.autostart.set(next);
      if (result.ok) {
        setAutostart(result.value);
        toast(result.value ? "已注册开机启动" : "已取消开机启动");
      } else {
        toast(result.message, "error");
      }
    });

  const testShortcut = () =>
    run("shortcut", async () => {
      const result = shortcutRegistered
        ? await platform.shortcuts.unregister(TEST_SHORTCUT)
        : await platform.shortcuts.register(TEST_SHORTCUT, () => toast("全局快捷键已触发"));
      if (result.ok) {
        setShortcutRegistered(!shortcutRegistered);
        toast(shortcutRegistered ? "已注销测试快捷键" : `已注册 ${TEST_SHORTCUT}，按一次可验证回调`);
      } else {
        toast(result.message, "error");
      }
    });

  const testClipboard = () =>
    run("clipboard", async () => {
      const result = await platform.clipboard.writeText(`wxlike 剪贴板检查 ${new Date().toLocaleTimeString()}`);
      const message = resultText(result, "已写入剪贴板，可粘贴验证");
      toast(message.text, message.tone);
    });

  const testNotification = () =>
    run("notification", async () => {
      const result = await platform.notifications.show({
        title: "wxlike 权限检查",
        body: state.preferences?.notification_preview === false ? "你收到一条新通知。" : "这是一条由 PlatformPort 发出的测试通知。",
      });
      const message = resultText(result, "已请求系统显示通知");
      toast(message.text, message.tone);
    });

  const testScreenshot = () =>
    run("screenshot", async () => {
      const result = await platform.screenshot.capture();
      if (result.ok) toast(`截图成功：${Math.round(result.value.png.byteLength / 1024)} KB`);
      else toast(result.message, "error");
    });

  const syncTray = () =>
    run("tray", async () => {
      const result = await platform.tray.setBadge(state.unreadTotal);
      const message = resultText(result, `托盘角标已更新为 ${state.unreadTotal}`);
      toast(message.text, message.tone);
    });

  return (
    <div className="space-y-4">
      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
        <div className="flex items-center justify-between gap-2">
          <div>
            <h2 className="text-[11.5px] font-semibold text-[var(--ink)]">PlatformPort 能力</h2>
            <p className="mt-1 text-[9.5px] leading-4 text-[var(--muted)]">
              每一项都由宿主真实调用；不可用时给出替代方案，而不是模拟成功。当前宿主：{caps.kind === "tauri" ? "Tauri 桌面" : "浏览器"}。
            </p>
          </div>
          <span className="rounded bg-[var(--surface-3)] px-1.5 py-0.5 text-[8.5px] text-[var(--muted)]" data-testid="platform-kind">
            {caps.kind}
          </span>
        </div>

        <div className="mt-2">
          <CapabilityRow
            title="窗口控制"
            detail="最小化、最大化、关闭由本机窗口管理器执行。"
            capability={caps.window_controls}
            icon={<AppWindow size={14} />}
          />
          <CapabilityRow
            title="系统托盘"
            detail="托盘角标显示未读总数，点击可唤起窗口（由宿主实现）。"
            capability={caps.tray}
            icon={<PanelTop size={14} />}
          >
            <Button size="sm" onClick={() => void syncTray()} pending={busy === "tray"} disabled={!caps.tray.supported} data-testid="platform-tray-test">
              同步角标
            </Button>
          </CapabilityRow>
          <CapabilityRow
            title="开机启动"
            detail={autostart === null ? "正在读取系统注册项…" : autostart ? "已注册为登录时启动。" : "当前未注册开机启动。"}
            capability={caps.autostart}
            icon={<Power size={14} />}
          >
            <Button
              size="sm"
              onClick={() => void toggleAutostart()}
              pending={busy === "autostart"}
              disabled={!caps.autostart.supported || autostart === null}
              data-testid="platform-autostart-test"
            >
              {autostart ? "取消注册" : "注册"}
            </Button>
          </CapabilityRow>
          <CapabilityRow
            title="全局快捷键"
            detail="宿主注册后，即使窗口不在前台也能唤起搜索；页面内 ⌘/Ctrl+K 始终可用。"
            capability={caps.global_shortcuts}
            icon={<Keyboard size={14} />}
          >
            <Button
              size="sm"
              onClick={() => void testShortcut()}
              pending={busy === "shortcut"}
              disabled={!caps.global_shortcuts.supported}
              data-testid="platform-shortcut-test"
            >
              {shortcutRegistered ? "注销测试键" : "注册测试键"}
            </Button>
          </CapabilityRow>
          <CapabilityRow
            title="系统通知"
            detail="权限与投递由操作系统决定；被拒绝时不会再次弹框。"
            capability={caps.notifications}
            icon={<Bell size={14} />}
          >
            <Button size="sm" onClick={() => void testNotification()} pending={busy === "notification"} disabled={!caps.notifications.supported} data-testid="platform-notification-test">
              发一条
            </Button>
          </CapabilityRow>
          <CapabilityRow
            title="剪贴板"
            detail="复制消息与导出备份都通过该能力写入。"
            capability={caps.clipboard}
            icon={<Clipboard size={14} />}
          >
            <Button size="sm" onClick={() => void testClipboard()} pending={busy === "clipboard"} disabled={!caps.clipboard.supported} data-testid="platform-clipboard-test">
              写入测试
            </Button>
          </CapabilityRow>
          <CapabilityRow
            title="屏幕截图"
            detail="截图结果可直接作为图片消息上传。"
            capability={caps.screenshot}
            icon={<Camera size={14} />}
          >
            <Button size="sm" onClick={() => void testScreenshot()} pending={busy === "screenshot"} disabled={!caps.screenshot.supported} data-testid="platform-screenshot-test">
              截一张
            </Button>
          </CapabilityRow>
          <CapabilityRow
            title="文件选择与保存"
            detail="上传使用分块读取；导出备份使用系统保存对话框。"
            capability={caps.files}
            icon={<FolderOpen size={14} />}
          />
          <CapabilityRow
            title="密钥存储"
            detail={
              caps.secrets.storage === "os_keychain"
                ? "令牌存放在操作系统密钥库，自动登录可用。"
                : "没有系统密钥库，凭据只保留在内存/会话存储，自动登录受限。"
            }
            capability={caps.secrets}
            icon={<KeyRound size={14} />}
          />
        </div>

        {autostartNote ? (
          <p className="mt-2 text-[9px] leading-4 text-[var(--warn)]" role="alert">
            {autostartNote}
          </p>
        ) : null}
      </section>

      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
        <h2 className="text-[11.5px] font-semibold text-[var(--ink)]">核心契约</h2>
        <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1.5 text-[10px]">
          <dt className="text-[var(--muted)]">契约版本</dt>
          <dd className="text-[var(--ink-2)]" data-testid="core-contract-version">
            {state.capabilities?.contract_version ?? "宿主未报告"}
          </dd>
          <dt className="text-[var(--muted)]">传输</dt>
          <dd className="text-[var(--ink-2)]">{state.capabilities?.transport ?? "未知"}</dd>
          <dt className="text-[var(--muted)]">动作目录</dt>
          <dd className="text-[var(--ink-2)]">{state.capabilities?.actions.length ?? 0} 个 action_id</dd>
          <dt className="text-[var(--muted)]">扫码登录</dt>
          <dd className="text-[var(--ink-2)]">{state.capabilities?.qr_login ? "核心支持" : "未报告支持"}</dd>
          <dt className="text-[var(--muted)]">系统密钥库</dt>
          <dd className="text-[var(--ink-2)]">{state.capabilities?.secrets_os_backed ? "可用" : "不可用"}</dd>
          <dt className="text-[var(--muted)]">同步范围</dt>
          <dd className="text-[var(--ink-2)]">{state.capabilities?.sync_range ? "可调" : "按默认 7 天"}</dd>
        </dl>
        {state.capabilities?.notes?.length ? (
          <ul className="mt-2 space-y-1 border-t border-[var(--line)] pt-2 text-[9px] leading-4 text-[var(--warn)]">
            {state.capabilities.notes.map((note) => (
              <li key={note}>{note}</li>
            ))}
          </ul>
        ) : null}
        <p className="mt-2 text-[9px] leading-4 text-[var(--muted)]">
          界面只通过 <code>CoreHandle.dispatch/subscribe</code> 与 Rust 交互；没有 HTTP 直连，也没有本地会话兜底。
          {state.capabilities?.transport === "fixture" ? " 当前为开发 fixture，不作为验收证据。" : ""}
        </p>
      </section>
    </div>
  );
}
