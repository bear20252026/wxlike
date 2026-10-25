import { useEffect, useState } from "react";
import { Bell, BellRing } from "lucide-react";
import { ACTIONS } from "../../shell/core-contract";
import type { PermissionState } from "../../shell/platform/port";
import { useApp, useAppState, useCoreAction, useToast } from "../../shell/store";
import { SyncRangeControl } from "../auth/sync-range";
import { Button, Switch } from "../../shell/ui";

const PERMISSION_LABEL: Record<PermissionState, string> = {
  granted: "已授予",
  denied: "已拒绝",
  prompt: "尚未授权",
  unsupported: "当前平台不支持",
};

export function NotificationSection() {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const { platform } = useApp();
  const caps = platform.capabilities();
  const [permission, setPermission] = useState<PermissionState>("unsupported");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let alive = true;
    void platform.notifications.permission().then((value) => {
      if (alive) setPermission(value);
    });
    return () => {
      alive = false;
    };
  }, [platform]);

  const preview = state.preferences?.notification_preview ?? true;

  const request = async () => {
    if (busy) return;
    setBusy(true);
    const next = await platform.notifications.request();
    setPermission(next);
    setBusy(false);
    if (next === "granted") toast("通知权限已授予");
    else toast(PERMISSION_LABEL[next], "error");
  };

  const test = async () => {
    if (busy) return;
    setBusy(true);
    const result = await platform.notifications.show({
      title: "wxlike 通知测试",
      body: preview ? "预览已开启：这是一条测试通知的消息内容。" : "预览已关闭：你收到一条新通知。",
    });
    setBusy(false);
    if (result.ok) toast("已请求系统显示通知");
    else toast(result.message, "error");
  };

  const togglePreview = async (value: boolean) => {
    const response = await dispatch(ACTIONS.preferencesSet, { patch: { notification_preview: value } }, {
      idempotencyKey: `pref:notification_preview:${value}`,
    });
    if (response.ok) toast("通知预览设置已保存");
  };

  return (
    <div className="space-y-4">
      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
        <div className="flex items-start gap-3">
          <span className="grid h-9 w-9 place-items-center rounded-lg bg-[var(--accent-soft)] text-[var(--accent-ink)]">
            <BellRing size={16} />
          </span>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h2 className="text-[11.5px] font-semibold text-[var(--ink)]">系统通知</h2>
              <span className="rounded bg-[var(--surface-3)] px-1.5 py-0.5 text-[8.5px] text-[var(--muted)]">
                {caps.notifications.supported ? PERMISSION_LABEL[permission] : caps.notifications.reason}
              </span>
            </div>
            <p className="mt-1 text-[9.5px] leading-4 text-[var(--muted)]">
              通知由 PlatformPort 真实调用系统 API；浏览器需 HTTPS 且只能从用户手势请求权限，桌面端由 Tauri 宿主转发。
            </p>
            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              <Button size="sm" onClick={() => void request()} pending={busy} disabled={!caps.notifications.supported} data-testid="notification-request">
                <Bell size={12} /> 请求权限
              </Button>
              <Button size="sm" onClick={() => void test()} disabled={busy || permission !== "granted"} data-testid="notification-test">
                发送测试通知
              </Button>
            </div>
            {permission === "denied" ? (
              <p className="mt-2 text-[9px] leading-4 text-[var(--warn)]">
                系统已拒绝通知权限。浏览器需在站点设置里重新允许；桌面端需在系统设置中重新授权，应用不会再弹权限框。
              </p>
            ) : null}
          </div>
        </div>
        <div className="mt-3 flex items-center justify-between border-t border-[var(--line)] pt-3">
          <div>
            <div className="text-[11px] text-[var(--ink)]">通知显示消息预览</div>
            <div className="mt-0.5 text-[9px] text-[var(--muted)]">关闭后只显示“你收到一条新消息”，不泄露正文。</div>
          </div>
          <Switch label="通知显示消息预览" checked={preview} onChange={(value) => void togglePreview(value)} />
        </div>
      </section>

      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
        <SyncRangeControl />
      </section>
    </div>
  );
}
