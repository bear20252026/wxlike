/**
 * 小程序目录 (G11-05).
 *
 * - 每张应用卡渲染真实 app_id 与版本（无 app_id 的卡不允许出现）。
 * - 安装/更新/撤权均为弹窗确认，写操作幂等；安装前客户端校验
 *   manifest digest（与发布记录一致才提交）。
 * - 网页型小程序宿主：iframe sandbox（无 same-origin、无顶层导航）+
 *   origin 白名单检查（仅 https 且命中 allowlist 才可打开）。
 * - 目录数据为 G08 MiniAppPort 契约（未开通时明确呈现）。
 */

import { useCallback, useEffect, useState } from "react";
import { errorText } from "../../lib/http";
import { MiniAppApi, isUpstreamMissing } from "./api";
import { MINI_SANDBOX_ATTRS, checkOrigin } from "./model";
import type { MiniAppRecord, MiniPorts } from "./model";
import { Banner, DotMenu, EmptyState, GhostButton, Modal, PrimaryButton } from "../content/ui";

type DialogKind = "install" | "update" | "revoke" | null;

export function MiniAppsPage({ ports = DEFAULT_PORTS }: { ports?: MiniPorts }) {
  const [apps, setApps] = useState<MiniAppRecord[] | null>(null);
  const [upstream, setUpstream] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [dialog, setDialog] = useState<{ kind: DialogKind; app: MiniAppRecord } | null>(null);
  const [hosting, setHosting] = useState<MiniAppRecord | null>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await ports.list();
      setApps(list);
      setUpstream(false);
    } catch (err) {
      if (isUpstreamMissing(err)) setUpstream(true);
      else setError(errorText(err));
      setApps([]);
    }
  }, [ports]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className="mx-auto flex h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto p-4" data-testid="miniapps-page">
      <Banner tone="error" text={error} />
      <Banner tone="info" text={notice} />
      <Banner tone="info" text={upstream ? "小程序目录（G08 MiniAppPort）尚未开通；以下界面在端点就绪后即为真实数据。" : ""} />

      {apps !== null && apps.length === 0 && !upstream ? <EmptyState title="暂无小程序" hint="目录由管理后台发布" /> : null}

      <div className="grid grid-cols-3 gap-2">
        {(apps ?? []).map((app) => (
          <MiniAppCard
            key={app.app_id}
            app={app}
            onOpen={() => setDialog({ kind: app.installed_version ? "update" : "install", app })}
            onHost={() => setHosting(app)}
            onRevoke={() => setDialog({ kind: "revoke", app })}
          />
        ))}
      </div>

      {dialog?.kind === "install" || dialog?.kind === "update" ? (
        <InstallDialog
          ports={ports}
          app={dialog.app}
          mode={dialog.kind}
          onClose={() => setDialog(null)}
          onDone={(message) => {
            setDialog(null);
            setNotice(message);
            void refresh();
          }}
          onError={setError}
        />
      ) : null}

      {dialog?.kind === "revoke" ? (
        <RevokeDialog
          ports={ports}
          app={dialog.app}
          onClose={() => setDialog(null)}
          onDone={() => {
            setDialog(null);
            setNotice("已撤回授权");
            void refresh();
          }}
          onError={setError}
        />
      ) : null}

      {hosting ? (
        <Modal title={`${hosting.name}（沙箱宿主）`} onClose={() => setHosting(null)}>
          <SandboxHost app={hosting} onError={setError} />
        </Modal>
      ) : null}
    </div>
  );
}

function MiniAppCard({
  app,
  onOpen,
  onHost,
  onRevoke,
}: {
  app: MiniAppRecord;
  onOpen: () => void;
  onHost: () => void;
  onRevoke: () => void;
}) {
  const menus = [
    { label: app.installed_version ? "更新" : "安装", onSelect: onOpen },
    { label: "打开（沙箱）", onSelect: onHost },
  ];
  if (app.installed_version) menus.push({ label: "撤回授权", onSelect: onRevoke });
  return (
    <div className="rounded-xl border border-zinc-200 bg-white p-3" data-testid={`miniapp-${app.app_id}`}>
      <button type="button" onClick={onOpen} className="w-full text-left">
        <p className="truncate text-sm font-medium text-zinc-800">{app.name}</p>
        <p className="mt-0.5 truncate font-mono text-[10px] text-zinc-400">{app.app_id}</p>
        <p className="mt-0.5 text-[11px] text-zinc-400">
          {app.installed_version ? `已安装 v${app.installed_version}` : app.latest ? `可安装 v${app.latest.version}` : "无版本"}
        </p>
      </button>
      <div className="mt-1 flex justify-end">
        <DotMenu items={menus} />
      </div>
    </div>
  );
}

function InstallDialog({
  ports,
  app,
  mode,
  onClose,
  onDone,
  onError,
}: {
  ports: MiniPorts;
  app: MiniAppRecord;
  mode: "install" | "update";
  onClose: () => void;
  onDone: (message: string) => void;
  onError: (text: string) => void;
}) {
  const [stage, setStage] = useState<"idle" | "verifying" | "installing">("idle");
  const target = app.latest;
  const go = async () => {
    if (!target) {
      onError("没有可用版本");
      return;
    }
    onError("");
    setStage("verifying");
    try {
      // install() 内部先下载并校验 manifest digest，再提交安装
      setStage("installing");
      const reply = await ports.install(app.app_id, target.version, target.manifest_digest);
      onDone(`已安装 v${reply.installed_version}`);
    } catch (err) {
      if (isUpstreamMissing(err)) onError("小程序服务（G08 MiniAppPort）尚未开通，无法安装");
      else onError(errorText(err));
      setStage("idle");
    }
  };

  return (
    <Modal title={mode === "install" ? "安装小程序" : "更新小程序"} onClose={onClose}>
      <div className="space-y-3 text-sm" data-testid="install-dialog">
        <p className="text-zinc-600">
          {app.name} <span className="font-mono text-xs text-zinc-400">{app.app_id}</span>
        </p>
        {target ? (
          <p className="text-xs text-zinc-400">
            版本 v{target.version} · digest {target.manifest_digest.slice(0, 12)}…（安装前本地校验）
          </p>
        ) : (
          <p className="text-xs text-zinc-400">无可用版本</p>
        )}
        <p className="text-xs text-zinc-500">安装后按 scope 申请授权；敏感调用在宿主内逐次确认。</p>
        <div className="flex justify-end gap-2">
          <GhostButton onClick={onClose}>取消</GhostButton>
          <PrimaryButton onClick={() => void go()} busy={stage !== "idle"} disabled={!target}>
            {mode === "install" ? "安装" : "更新"}
          </PrimaryButton>
        </div>
      </div>
    </Modal>
  );
}

function RevokeDialog({
  ports,
  app,
  onClose,
  onDone,
  onError,
}: {
  ports: MiniPorts;
  app: MiniAppRecord;
  onClose: () => void;
  onDone: () => void;
  onError: (text: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const go = async () => {
    setBusy(true);
    onError("");
    try {
      await ports.revoke(app.app_id);
      onDone();
    } catch (err) {
      if (isUpstreamMissing(err)) onError("小程序服务（G08 MiniAppPort）尚未开通，无法撤权");
      else onError(errorText(err));
      setBusy(false);
    }
  };
  return (
    <Modal title="撤回授权" onClose={onClose}>
      <div className="space-y-3 text-sm" data-testid="revoke-dialog">
        <p className="text-zinc-600">
          将撤回 <strong>{app.name}</strong>（<span className="font-mono text-xs">{app.app_id}</span>）的全部授权，小程序将立即失去数据访问。
        </p>
        <div className="flex justify-end gap-2">
          <GhostButton onClick={onClose}>取消</GhostButton>
          <GhostButton tone="danger" onClick={() => void go()} disabled={busy}>
            确认撤回
          </GhostButton>
        </div>
      </div>
    </Modal>
  );
}

/** Web mini-app host: only https + allowlisted origins, hard sandbox. */
function SandboxHost({ app, onError }: { app: MiniAppRecord; onError: (text: string) => void }) {
  const target = app.latest?.package_url ?? "";
  const decision = checkOrigin(target, app.origins);
  if (!decision.ok) {
    return (
      <p className="text-sm text-red-600" data-testid="sandbox-blocked">
        已拒绝打开：来源 {decision.reason === "not-https" ? "不是 https" : decision.reason === "not-allowlisted" ? "不在该小程序的可信 origin 列表" : "无法解析"}
      </p>
    );
  }
  return (
    <iframe
      title={`${app.name} sandbox`}
      src={target}
      sandbox={MINI_SANDBOX_ATTRS}
      referrerPolicy="no-referrer"
      data-testid="mini-sandbox"
      onError={() => onError("小程序页面加载失败")}
      className="h-80 w-full rounded-lg border border-zinc-200"
    />
  );
}

const DEFAULT_PORTS = new MiniAppApi();
