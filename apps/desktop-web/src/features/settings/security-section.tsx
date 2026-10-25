import { useEffect, useState } from "react";
import { DatabaseBackup, HardDrive, KeyRound, ShieldCheck, Upload } from "lucide-react";
import { ACTIONS } from "../../shell/core-contract";
import type { BackupInfo, StorageStatus } from "../../shell/models";
import { useApp, useAppState, useCoreAction, useToast } from "../../shell/store";
import { useAsync, usePending } from "../../shell/hooks";
import { formatBytes } from "../chat/upload";
import { Button, ConfirmDialog, InlineError, Spinner, TextField } from "../../shell/ui";

interface SecretsStatus {
  locked: boolean;
  os_backed: boolean;
  passphrase_migration: string;
  note?: string;
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let index = 0; index < bytes.length; index += chunk) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunk));
  }
  return btoa(binary);
}

function base64ToBytes(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

export function SecuritySection() {
  const dispatch = useCoreAction();
  const toast = useToast();
  const { platform, store, core } = useApp();
  const state = useAppState();
  const secrets = useAsync<SecretsStatus>(() => dispatch(ACTIONS.secretsStatus, {}), []);
  const storage = useAsync<StorageStatus>(() => dispatch(ACTIONS.storageStatus, {}), []);
  const { pending, run } = usePending();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [migrateError, setMigrateError] = useState<string | null>(null);
  const [backupInfo, setBackupInfo] = useState<BackupInfo | null>(null);
  const [verify, setVerify] = useState<{ valid: boolean; digest: string; reason?: string; base64: string } | null>(null);
  const [clearOpen, setClearOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);

  useEffect(() => {
    if (state.session.phase === "authenticated") {
      secrets.reload();
      storage.reload();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.session.phase]);

  const migrate = async () => {
    const errors: Record<string, string> = {};
    if (!current) errors.current_password = "请输入当前口令";
    if (next.length < 8) errors.new_password = "新口令至少 8 位";
    if (next && next === current) errors.new_password = "新口令不能与当前口令相同";
    if (next !== confirm) errors.confirm = "两次输入的新口令不一致";
    setFieldErrors(errors);
    setMigrateError(null);
    if (Object.keys(errors).length) return;
    await run(async () => {
      const response = await dispatch(ACTIONS.secretsMigrate, { current_password: current, new_password: next }, {
        idempotencyKey: `secrets-migrate:${current.length}:${next.length}`,
      });
      if (response.ok) {
        toast("口令已更新；旧凭据由核心迁移");
        setCurrent("");
        setNext("");
        setConfirm("");
        secrets.reload();
      } else {
        setFieldErrors(response.error.field_errors ?? {});
        if (!response.error.field_errors) setMigrateError(response.error.message);
      }
    });
  };

  const createBackup = async () => {
    const response = await dispatch<{ info: BackupInfo; base64: string }>(ACTIONS.backupCreate, {}, {
      idempotencyKey: `backup-create:${new Date().toISOString().slice(0, 10)}`,
    });
    if (!response.ok) return;
    setBackupInfo(response.data.info);
    const bytes = base64ToBytes(response.data.base64);
    const saved = await platform.files.save({
      suggestedName: response.data.info.file_name,
      mime: "application/json",
      bytes,
    });
    if (saved.ok) {
      toast(saved.value.via === "dialog" ? "备份已保存到所选位置" : "备份已通过浏览器下载");
    } else {
      toast(`备份已生成但未保存：${saved.message}`, "error");
    }
  };

  const pickBackup = async () => {
    const picked = await platform.files.open({ accept: ".json,application/json" });
    if (!picked.ok) {
      if (picked.code !== "CANCELLED") toast(picked.message, "error");
      return;
    }
    const file = picked.value[0];
    if (!file) return;
    if (file.size > 256 * 1024 * 1024) {
      toast("备份文件超过 256MB 的界面校验预算；请使用核心的流式导入能力（未接入）", "error");
      return;
    }
    const bytes = await file.bytes();
    const base64 = bytesToBase64(bytes);
    const response = await dispatch<{ valid: boolean; reason?: string; digest: string }>(ACTIONS.backupVerify, { base64 }, {
      idempotencyKey: `backup-verify:${file.name}:${file.size}`,
    });
    if (!response.ok) return;
    setVerify({ ...response.data, base64 });
    if (!response.data.valid) toast(response.data.reason ?? "备份校验失败", "error");
  };

  const runImport = async () => {
    if (!verify?.valid) return;
    setImportOpen(false);
    await run(async () => {
      const response = await dispatch(ACTIONS.backupImport, { base64: verify.base64, confirmation: "IMPORT" }, {
        idempotencyKey: `backup-import:${verify.digest}`,
      });
      if (response.ok) {
        toast("备份已导入；正在重新载入核心投影");
        await store.loadWorkspace(core);
        storage.reload();
        secrets.reload();
        setVerify(null);
      }
    });
  };

  const clearCache = async () => {
    setClearOpen(false);
    const response = await dispatch<{ freed_bytes: number; kept: string[] }>(ACTIONS.cacheClear, {}, {
      idempotencyKey: `cache-clear:${new Date().toISOString().slice(0, 13)}`,
    });
    if (response.ok) {
      toast(`已清理缓存 ${formatBytes(response.data.freed_bytes)}；保留 ${response.data.kept.join("、")}`);
      storage.reload();
    }
  };

  return (
    <div className="space-y-4">
      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
        <div className="flex items-start gap-3">
          <span className="grid h-9 w-9 place-items-center rounded-lg bg-[var(--accent-soft)] text-[var(--accent-ink)]">
            <KeyRound size={16} />
          </span>
          <div className="min-w-0 flex-1">
            <h2 className="text-[11.5px] font-semibold text-[var(--ink)]">本地凭据</h2>
            {secrets.loading ? (
              <Spinner />
            ) : secrets.error ? (
              <p className="mt-1 text-[9.5px] text-[var(--danger)]">{secrets.error.message}</p>
            ) : secrets.data ? (
              <p className="mt-1 text-[9.5px] leading-4 text-[var(--muted)]">
                存储：{secrets.data.os_backed ? "系统密钥库" : "会话级 / 非 OS 密钥库"} · 状态：{secrets.data.locked ? "已锁定" : "已解锁"} · 口令迁移：
                {secrets.data.passphrase_migration === "available" ? "可用" : secrets.data.passphrase_migration}
                {secrets.data.note ? ` · ${secrets.data.note}` : ""}
              </p>
            ) : null}
            <div className="mt-2 flex items-center gap-1.5">
              <Button
                size="sm"
                onClick={async () => {
                  const response = await dispatch(ACTIONS.secretsLock, {}, { idempotencyKey: `secrets-lock:${Date.now()}` });
                  if (response.ok) {
                    toast("本地秘密已锁定");
                    secrets.reload();
                  }
                }}
                data-testid="secrets-lock"
              >
                <ShieldCheck size={12} /> 锁定本地秘密
              </Button>
            </div>
          </div>
        </div>

        <div className="mt-3 border-t border-[var(--line)] pt-3">
          <h3 className="text-[10.5px] font-medium text-[var(--ink)]">迁移本地口令</h3>
          <p className="mt-0.5 text-[9px] leading-4 text-[var(--muted)]">
            口令只提交给核心；表单不写入 localStorage，提交后立即清空输入框。
          </p>
          <div className="mt-2 grid gap-2 sm:grid-cols-3">
            <TextField label="当前口令" type="password" autoComplete="current-password" value={current} onChange={(event) => setCurrent(event.target.value)} error={fieldErrors.current_password} data-testid="secrets-current" />
            <TextField label="新口令" type="password" autoComplete="new-password" value={next} onChange={(event) => setNext(event.target.value)} error={fieldErrors.new_password} hint="至少 8 位" data-testid="secrets-next" />
            <TextField label="确认新口令" type="password" autoComplete="new-password" value={confirm} onChange={(event) => setConfirm(event.target.value)} error={fieldErrors.confirm} data-testid="secrets-confirm" />
          </div>
          <InlineError error={migrateError ? { code: "VALIDATION", message: migrateError, retryable: false, request_id: "local" } : null} />
          <Button size="sm" className="mt-2" variant="primary" pending={pending} onClick={() => void migrate()} data-testid="secrets-migrate">
            迁移口令
          </Button>
        </div>
      </section>

      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
        <div className="flex items-start gap-3">
          <span className="grid h-9 w-9 place-items-center rounded-lg bg-[var(--accent-soft)] text-[var(--accent-ink)]">
            <DatabaseBackup size={16} />
          </span>
          <div className="min-w-0 flex-1">
            <h2 className="text-[11.5px] font-semibold text-[var(--ink)]">备份与恢复</h2>
            <p className="mt-1 text-[9.5px] leading-4 text-[var(--muted)]">
              导出由核心生成校验摘要；导入前先校验格式与账号归属，再用二次确认覆盖本地数据。
            </p>
            {backupInfo ? (
              <p className="mt-1 text-[9px] text-[var(--muted)]">
                最近备份：{backupInfo.file_name} · {formatBytes(backupInfo.size)} · 摘要 {backupInfo.digest}
              </p>
            ) : null}
            {verify ? (
              <p className={`mt-1 text-[9px] ${verify.valid ? "text-[var(--accent-ink)]" : "text-[var(--danger)]"}`} data-testid="backup-verify-result">
                {verify.valid ? `校验通过 · 摘要 ${verify.digest}` : `校验失败：${verify.reason ?? "未知原因"}`}
              </p>
            ) : null}
            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              <Button size="sm" onClick={() => void createBackup()} data-testid="backup-create">
                <DatabaseBackup size={12} /> 生成并保存备份
              </Button>
              <Button size="sm" onClick={() => void pickBackup()} data-testid="backup-pick">
                <Upload size={12} /> 选择备份校验
              </Button>
              <Button size="sm" variant="primary" disabled={!verify?.valid} onClick={() => setImportOpen(true)} data-testid="backup-import">
                导入备份
              </Button>
            </div>
          </div>
        </div>
      </section>

      <section className="rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-4">
        <div className="flex items-start gap-3">
          <span className="grid h-9 w-9 place-items-center rounded-lg bg-[var(--accent-soft)] text-[var(--accent-ink)]">
            <HardDrive size={16} />
          </span>
          <div className="min-w-0 flex-1">
            <h2 className="text-[11.5px] font-semibold text-[var(--ink)]">存储与缓存</h2>
            {storage.loading ? (
              <Spinner />
            ) : storage.data ? (
              <p className="mt-1 text-[9.5px] leading-4 text-[var(--muted)]">
                缓存 {formatBytes(storage.data.cache_bytes)} · 消息 {formatBytes(storage.data.message_bytes)} · 媒体 {formatBytes(storage.data.media_bytes)}
              </p>
            ) : (
              <p className="mt-1 text-[9.5px] text-[var(--danger)]">{storage.error?.message ?? "存储状态不可用"}</p>
            )}
            <div className="mt-2 flex items-center gap-1.5">
              <Button size="sm" onClick={() => setClearOpen(true)} data-testid="cache-clear">
                清理缓存
              </Button>
              <Button size="sm" variant="ghost" onClick={() => { storage.reload(); secrets.reload(); }}>
                刷新状态
              </Button>
            </div>
            <p className="mt-2 text-[9px] leading-4 text-[var(--muted)]">清理不会删除未发送 outbox、草稿与密钥；这些由核心保证保留。</p>
          </div>
        </div>
      </section>

      <ConfirmDialog
        open={clearOpen}
        title="清理本地缓存？"
        description="缓存会重新从核心拉取；未发送消息、草稿与本地秘密不会被删除。"
        confirmLabel="清理缓存"
        pending={pending}
        onConfirm={() => void clearCache()}
        onClose={() => setClearOpen(false)}
      />
      <ConfirmDialog
        open={importOpen}
        title="导入备份并覆盖本地数据？"
        description="导入前已通过摘要与账号校验。此操作会覆盖本地会话、联系人、群组与设置。"
        confirmLabel="导入"
        danger
        pending={pending}
        onConfirm={() => void runImport()}
        onClose={() => setImportOpen(false)}
      />
    </div>
  );
}
