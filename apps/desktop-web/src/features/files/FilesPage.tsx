/**
 * 文件中心与传输管理 (G11-02).
 *
 * - 上传：选择器/拖拽（浏览器沙箱天然禁止任意本地路径输入）、进度、取消、
 *   失败重试（内容寻址去重保证同 digest 重传不产生第二个对象）。
 * - 下载：走真实 `/api/media/{id}`，按记录 digest 复核 sha256。
 * - 列表：G04 /files 契约 + 本地真实上传日志合并；重命名/分享/删除/批量/
 *   备份计划为 G04 契约界面，上游未合入时呈现明确状态。
 * - 批量操作逐项返回结果；用户取消不弹成功。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ApiError, errorText } from "../../lib/http";
import { platform } from "../../lib/platform";
import { applyFileQuery, journalAdd, journalAll, journalRemove, recordFromUpload } from "./api";
import { formatBytes } from "./bytes";
import type {
  BackupPlan,
  FileActionResult,
  FileCategory,
  FileQuery,
  FileRecord,
  FilesPorts,
  UploadReply,
} from "./model";
import { categoryOf } from "./model";
import { Banner, DotMenu, EmptyState, GhostButton, Modal, PrimaryButton, formatTime } from "../content/ui";

interface UploadTask {
  id: string;
  name: string;
  size: number;
  progress: number;
  status: "uploading" | "done" | "error" | "cancelled";
  error?: string;
  controller?: AbortController;
}

const CATEGORIES: Array<{ value: FileCategory; label: string }> = [
  { value: "all", label: "全部" },
  { value: "image", label: "图片" },
  { value: "document", label: "文档" },
  { value: "audio", label: "音频" },
  { value: "video", label: "视频" },
  { value: "other", label: "其他" },
];

const UPSTREAM_HINT = "文件目录/重命名/分享/删除与备份计划（G04 MediaPort）尚未开通";

export function FilesPage({ ports }: { ports: FilesPorts }) {
  const [remote, setRemote] = useState<FileRecord[] | null>(null);
  const [upstreamMissing, setUpstreamMissing] = useState(false);
  const [query, setQuery] = useState<FileQuery>({});
  const [tasks, setTasks] = useState<UploadTask[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [renaming, setRenaming] = useState<FileRecord | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [batch, setBatch] = useState<Set<string>>(new Set());
  const [batchResults, setBatchResults] = useState<FileActionResult[] | null>(null);
  const [plan, setPlan] = useState<BackupPlan | null>(null);
  const [planBusy, setPlanBusy] = useState(false);
  const dragDepth = useRef(0);
  const [dragging, setDragging] = useState(false);

  const refresh = useCallback(async () => {
    setError("");
    try {
      const list = await ports.list();
      setRemote(list);
      setUpstreamMissing(false);
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        setUpstreamMissing(true);
        setRemote([]);
      } else {
        setError(errorText(err));
      }
    }
  }, [ports]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const records = useMemo(() => {
    const merged = [...journalAll(), ...(remote ?? [])];
    const seen = new Set<string>();
    return merged.filter((record) => {
      if (seen.has(record.media_id)) return false;
      seen.add(record.media_id);
      return true;
    });
  }, [remote]);

  const visible = useMemo(() => applyFileQuery(records, query), [records, query]);

  const totalBytes = useMemo(() => records.reduce((sum, record) => sum + record.size, 0), [records]);

  // ---- upload -------------------------------------------------------------

  const startUpload = useCallback(
    (file: File) => {
      const idem = `up-${file.name}-${file.size}-${file.lastModified}`;
      if (tasks.some((task) => task.id === idem)) return; // 重复提交：同一文件只排一次
      const controller = new AbortController();
      const task: UploadTask = {
        id: idem,
        name: file.name,
        size: file.size,
        progress: 0,
        status: "uploading",
        controller,
      };
      setTasks((prev) => [task, ...prev]);
      void (async () => {
        try {
          const reply = await ports.upload(
            file,
            { name: file.name, content_type: file.type || "application/octet-stream" },
            {
              signal: controller.signal,
              onProgress: (fraction) =>
                setTasks((prev) =>
                  prev.map((item) => (item.id === idem ? { ...item, progress: fraction } : item)),
                ),
            },
          );
          journalAdd(
            recordFromUpload(reply, { name: file.name, content_type: file.type || "application/octet-stream" }, replySha(file, reply)),
          );
          setTasks((prev) =>
            prev.map((item) => (item.id === idem ? { ...item, status: "done", progress: 1 } : item)),
          );
          setRemote((prev) => prev); // remote listing stays authoritative when G04 lands
          setRecordsFromJournal();
          setNotice(`已上传 ${file.name}`);
        } catch (err) {
          const code = err instanceof ApiError ? err.code : "server";
          setTasks((prev) =>
            prev.map((item) =>
              item.id === idem
                ? { ...item, status: code === "cancelled" ? "cancelled" : "error", error: errorText(err) }
                : item,
            ),
          );
        }
      })();
    },
    [ports, tasks],
  );

  const setRecordsFromJournal = () => {
    // journal is merged in `records` via useMemo dependency on `remote`; nudge it
    setRemote((prev) => (prev ? [...prev] : prev));
  };

  const onDrop = (event: React.DragEvent) => {
    event.preventDefault();
    dragDepth.current = 0;
    setDragging(false);
    for (const file of Array.from(event.dataTransfer.files)) startUpload(file);
  };

  // ---- actions ------------------------------------------------------------

  const download = async (record: FileRecord) => {
    setError("");
    try {
      const blob = await ports.download(record.media_id, {
        expectedDigest: record.digest,
      });
      platform().saveBlob(blob, record.name);
      setNotice(`已下载 ${record.name}（sha256 已核对）`);
    } catch (err) {
      setError(errorText(err));
    }
  };

  const rename = async () => {
    if (!renaming) return;
    setError("");
    try {
      const updated = await ports.rename(renaming.media_id, renameValue);
      setRemote((prev) => (prev ?? []).map((item) => (item.media_id === updated.media_id ? updated : item)));
      setRenaming(null);
      setNotice(`已重命名为 ${updated.name}`);
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        setError(`${UPSTREAM_HINT}（无法重命名）`);
      } else {
        setError(errorText(err));
      }
    }
  };

  const share = async (record: FileRecord) => {
    setError("");
    try {
      const result = await ports.share(record.media_id);
      const ok = await platform().copyText(result.url);
      setNotice(ok ? "分享链接已复制（短期有效）" : "链接已生成，复制失败");
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        setError(`${UPSTREAM_HINT}（无法分享）`);
      } else {
        setError(errorText(err));
      }
    }
  };

  const remove = async (record: FileRecord) => {
    setError("");
    try {
      await ports.remove(record.media_id);
      journalRemove(record.media_id);
      setRemote((prev) => (prev ?? []).filter((item) => item.media_id !== record.media_id));
      setNotice(`已删除 ${record.name}`);
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        setError(`${UPSTREAM_HINT}（无法删除）`);
      } else {
        setError(errorText(err));
      }
    }
  };

  const runBatchRemove = async () => {
    if (batch.size === 0) return;
    setError("");
    try {
      const results = await ports.batchRemove([...batch]);
      setBatchResults(results);
      for (const result of results) {
        if (result.ok) {
          journalRemove(result.media_id);
          setRemote((prev) => (prev ?? []).filter((item) => item.media_id !== result.media_id));
        }
      }
      setBatch(new Set());
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        setError(`${UPSTREAM_HINT}（无法批量删除）`);
      } else {
        setError(errorText(err));
      }
    }
  };

  const openPlan = async () => {
    setError("");
    setPlanBusy(true);
    try {
      setPlan(await ports.backupPlan());
    } catch (err) {
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        setError(`${UPSTREAM_HINT}（备份/迁移计划不可用）`);
      } else {
        setError(errorText(err));
      }
    } finally {
      setPlanBusy(false);
    }
  };

  const executePlan = async () => {
    if (!plan) return;
    setPlanBusy(true);
    setError("");
    try {
      const done = await ports.backupExecute(plan.plan_id);
      setPlan(done);
      setNotice("清理完成，证据见各步骤完成时间与释放容量");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setPlanBusy(false);
    }
  };

  const toggleBatch = (mediaId: string) =>
    setBatch((prev) => {
      const next = new Set(prev);
      if (next.has(mediaId)) next.delete(mediaId);
      else next.add(mediaId);
      return next;
    });

  return (
    <div
      className="mx-auto flex h-full w-full max-w-3xl flex-col gap-3 overflow-y-auto p-4"
      data-testid="files-page"
      onDragEnter={(event) => {
        event.preventDefault();
        dragDepth.current += 1;
        setDragging(true);
      }}
      onDragLeave={() => {
        dragDepth.current = Math.max(0, dragDepth.current - 1);
        if (dragDepth.current === 0) setDragging(false);
      }}
      onDragOver={(event) => event.preventDefault()}
      onDrop={onDrop}
    >
      <div className="flex flex-wrap items-center gap-2">
        <input
          value={query.keyword ?? ""}
          onChange={(event) => setQuery((prev) => ({ ...prev, keyword: event.target.value }))}
          placeholder="搜索文件名或对象 ID…"
          data-testid="files-search"
          className="min-w-48 flex-1 rounded-lg border border-zinc-200 px-3 py-1.5 text-sm outline-none focus:border-emerald-500"
        />
        <select
          value={query.category ?? "all"}
          onChange={(event) => setQuery((prev) => ({ ...prev, category: event.target.value as FileCategory }))}
          data-testid="files-category"
          className="rounded-lg border border-zinc-200 px-2 py-1.5 text-sm"
        >
          {CATEGORIES.map((item) => (
            <option key={item.value} value={item.value}>
              {item.label}
            </option>
          ))}
        </select>
        <select
          value={query.sort ?? "newest"}
          onChange={(event) => setQuery((prev) => ({ ...prev, sort: event.target.value as FileQuery["sort"] }))}
          data-testid="files-sort"
          className="rounded-lg border border-zinc-200 px-2 py-1.5 text-sm"
        >
          <option value="newest">最新上传</option>
          <option value="oldest">最早上传</option>
          <option value="largest">按大小</option>
          <option value="name">按名称</option>
        </select>
        <label className="cursor-pointer rounded-lg bg-emerald-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-emerald-700">
          上传文件
          <input
            type="file"
            multiple
            className="hidden"
            data-testid="files-input"
            onChange={(event) => {
              for (const file of Array.from(event.target.files ?? [])) startUpload(file);
              event.target.value = "";
            }}
          />
        </label>
        <GhostButton onClick={() => void openPlan()} disabled={planBusy} title="迁移与清理">
          清理
        </GhostButton>
      </div>

      <p className="text-xs text-zinc-400" data-testid="files-usage">
        本端可见 {records.length} 个对象 · {formatBytes(totalBytes)}
        {upstreamMissing ? "（G04 目录未开通，以下为本端真实上传记录）" : ""}
      </p>

      {dragging ? (
        <div className="rounded-xl border-2 border-dashed border-emerald-400 bg-emerald-50 p-6 text-center text-sm text-emerald-700" data-testid="files-dropzone">
          松开鼠标开始上传
        </div>
      ) : null}

      <Banner tone="error" text={error} />
      <Banner tone="info" text={notice} />

      {tasks.length > 0 ? (
        <section aria-label="传输队列" className="space-y-2">
          {tasks.map((task) => (
            <div key={task.id} className="rounded-lg border border-zinc-200 bg-white p-2.5" data-testid={`task-${task.name}`}>
              <div className="flex items-center justify-between gap-2 text-sm">
                <span className="truncate text-zinc-700">{task.name}</span>
                {task.status === "uploading" ? (
                  <GhostButton
                    onClick={() => {
                      // 用户取消不弹成功：中断即标记 cancelled，不进入 done 分支
                      task.controller?.abort();
                    }}
                  >
                    取消
                  </GhostButton>
                ) : (
                  <span
                    className={
                      task.status === "done"
                        ? "text-emerald-600"
                        : task.status === "cancelled"
                          ? "text-zinc-400"
                          : "text-red-600"
                    }
                    data-testid={`task-status-${task.name}`}
                  >
                    {task.status === "done" ? "已完成" : task.status === "cancelled" ? "已取消" : "失败"}
                  </span>
                )}
              </div>
              {task.status === "uploading" ? (
                <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-zinc-100">
                  <div
                    className="h-full rounded-full bg-emerald-500 transition-all"
                    style={{ width: `${Math.round(task.progress * 100)}%` }}
                    data-testid={`task-progress-${task.name}`}
                  />
                </div>
              ) : null}
              {task.error ? <p className="mt-1 text-xs text-red-500">{task.error}</p> : null}
            </div>
          ))}
        </section>
      ) : null}

      {batch.size > 0 ? (
        <div className="flex items-center justify-between rounded-lg bg-amber-50 px-3 py-2 text-sm">
          <span>已选择 {batch.size} 项</span>
          <GhostButton tone="danger" onClick={() => void runBatchRemove()}>
            批量删除
          </GhostButton>
        </div>
      ) : null}

      {!upstreamMissing && remote === null ? <p className="text-sm text-zinc-400" role="status">加载中…</p> : null}
      {visible.length === 0 && !upstreamMissing && remote !== null ? (
        <EmptyState title="暂无文件" hint="拖拽或点击“上传文件”开始" />
      ) : null}

      <section className="space-y-2">
        {visible.map((record) => (
          <article key={record.media_id} className="flex items-start gap-2 rounded-xl border border-zinc-200 bg-white p-3.5" data-testid={`file-${record.media_id}`}>
            <input
              type="checkbox"
              checked={batch.has(record.media_id)}
              onChange={() => toggleBatch(record.media_id)}
              aria-label={`选择 ${record.name}`}
              className="mt-1"
            />
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium text-zinc-800">{record.name}</p>
              <p className="mt-0.5 text-xs text-zinc-400">
                {formatBytes(record.size)} · {categoryLabel(record)} · {formatTime(record.uploaded_at)} ·{" "}
                {record.visibility === "public" ? "公开" : "私有"}
              </p>
              <p className="mt-0.5 truncate font-mono text-[11px] text-zinc-300">{record.media_id}</p>
            </div>
            <DotMenu
              items={[
                { label: "下载", onSelect: () => void download(record) },
                { label: "重命名", onSelect: () => { setRenaming(record); setRenameValue(record.name); } },
                { label: "分享", onSelect: () => void share(record) },
                { label: "删除", onSelect: () => void remove(record), danger: true },
              ]}
            />
          </article>
        ))}
      </section>

      {renaming ? (
        <Modal title="重命名" onClose={() => setRenaming(null)}>
          <div className="space-y-3">
            <input
              value={renameValue}
              onChange={(event) => setRenameValue(event.target.value)}
              data-testid="rename-input"
              className="w-full rounded-lg border border-zinc-200 px-3 py-2 text-sm outline-none focus:border-emerald-500"
            />
            <div className="flex justify-end gap-2">
              <GhostButton onClick={() => setRenaming(null)}>取消</GhostButton>
              <PrimaryButton onClick={() => void rename()} disabled={!renameValue.trim()}>
                保存
              </PrimaryButton>
            </div>
          </div>
        </Modal>
      ) : null}

      {batchResults ? (
        <Modal title="批量删除结果（逐项）" onClose={() => setBatchResults(null)}>
          <ul className="max-h-72 space-y-1.5 overflow-y-auto text-sm" data-testid="batch-results">
            {batchResults.map((result) => (
              <li key={result.media_id} className="flex items-center justify-between gap-2">
                <span className="truncate font-mono text-xs text-zinc-500">{result.media_id}</span>
                <span className={result.ok ? "text-emerald-600" : "text-red-600"}>
                  {result.ok ? "成功" : (result.detail ?? "失败")}
                </span>
              </li>
            ))}
          </ul>
          <div className="mt-3 flex justify-end">
            <PrimaryButton onClick={() => setBatchResults(null)}>好的</PrimaryButton>
          </div>
        </Modal>
      ) : null}

      {plan ? (
        <Modal title="迁移与清理计划" onClose={() => setPlan(null)}>
          <div className="space-y-3 text-sm" data-testid="backup-plan">
            <p className="text-zinc-600">
              计划容量：<strong data-testid="plan-bytes">{formatBytes(plan.plannedBytes)}</strong>
              ，共 {plan.steps.length} 步
            </p>
            <ol className="space-y-2">
              {plan.steps.map((step) => {
                const evidence = plan.evidence?.find((item) => item.step_id === step.step_id);
                return (
                  <li key={step.step_id} className="rounded-lg border border-zinc-200 p-2.5">
                    <div className="flex items-center justify-between gap-2">
                      <span>{step.label}</span>
                      <span className="text-xs text-zinc-400">{formatBytes(step.target_bytes)}</span>
                    </div>
                    {evidence ? (
                      <p className="mt-1 text-xs text-emerald-600" data-testid={`evidence-${step.step_id}`}>
                        完成：释放 {formatBytes(evidence.freed_bytes)} · {new Date(evidence.done_at).toLocaleString("zh-CN")}
                      </p>
                    ) : (
                      <p className="mt-1 text-xs text-zinc-400">待执行</p>
                    )}
                  </li>
                );
              })}
            </ol>
            <div className="flex justify-end gap-2">
              <GhostButton onClick={() => setPlan(null)}>关闭</GhostButton>
              {!plan.evidence ? (
                <PrimaryButton onClick={() => void executePlan()} busy={planBusy}>
                  执行清理
                </PrimaryButton>
              ) : null}
            </div>
          </div>
        </Modal>
      ) : null}
    </div>
  );
}

function categoryLabel(record: FileRecord): string {
  const map: Record<string, string> = {
    image: "图片",
    document: "文档",
    audio: "音频",
    video: "视频",
    other: "其他",
  };
  return map[categoryOf(record.content_type, record.name)] ?? "其他";
}

/**
 * The digest of a fresh upload is the file's own sha256; recompute lazily from
 * the source when the record is created (crypto.subtle is async, so the page
 * stores the reply digest the server echoed: media_id IS the sha256).
 */
function replySha(_file: File, reply: UploadReply): string {
  return reply.media_id;
}
