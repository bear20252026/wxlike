import { useCallback, useEffect, useRef, useState } from "react";
import { FilePlus2, ImagePlus, Mic, Send, Smile, Square, Trash2, X } from "lucide-react";
import { ACTIONS, newIdempotencyKey } from "../../shell/core-contract";
import type { ChatMessage, MessageBody } from "../../shell/models";
import { useApp, useAppState, useCoreAction, useToast } from "../../shell/store";
import { Button, IconButton, cn } from "../../shell/ui";
import { formatBytes, uploadFile, UploadCancelled, UploadError, MAX_UPLOAD_BYTES } from "./upload";

const EMOJIS = ["😀", "😂", "🥰", "🌿", "☕", "✦", "🌷", "👍", "🎉", "🐱", "📷", "🌙", "✨", "🙌", "😭", "🤝"];

interface UploadState {
  name: string;
  sent: number;
  total: number;
  cancelled: boolean;
}

export function Composer({
  convId,
  replyTo,
  onClearReply,
}: {
  convId: string;
  replyTo: ChatMessage | null;
  onClearReply: () => void;
}) {
  const state = useAppState();
  const dispatch = useCoreAction();
  const toast = useToast();
  const { platform } = useApp();
  const [text, setText] = useState("");
  const [emojiOpen, setEmojiOpen] = useState(false);
  const [upload, setUpload] = useState<UploadState | null>(null);
  const [recording, setRecording] = useState<{ seconds: number } | null>(null);
  const [sending, setSending] = useState(false);
  const sendingRef = useRef(false);
  const abortRef = useRef<AbortController | null>(null);
  const draftLoaded = useRef("");
  const draftTicket = useRef(0);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const recordTimer = useRef<ReturnType<typeof setInterval> | null>(null);
  const recordStart = useRef(0);
  const enterToSend = state.preferences?.enter_to_send ?? true;

  useEffect(() => {
    const ticket = ++draftTicket.current;
    draftLoaded.current = "";
    setText("");
    setEmojiOpen(false);
    void dispatch<{ text: string }>(ACTIONS.draftGet, { conv_id: convId }, { silent: true }).then((response) => {
      if (ticket !== draftTicket.current || !response.ok) return;
      draftLoaded.current = response.data.text;
      setText(response.data.text);
    });
  }, [convId, dispatch]);

  useEffect(() => {
    const ticket = draftTicket.current;
    if (text === draftLoaded.current) return;
    const timer = setTimeout(() => {
      if (ticket !== draftTicket.current) return;
      draftLoaded.current = text;
      void dispatch(ACTIONS.draftSave, { conv_id: convId, text }, { silent: true, idempotencyKey: `draft:${convId}:${text.length}:${text.slice(-24)}` });
    }, 500);
    return () => clearTimeout(timer);
  }, [text, convId, dispatch]);

  useEffect(() => {
    return () => {
      abortRef.current?.abort();
      if (recordTimer.current) clearInterval(recordTimer.current);
      recorderRef.current?.stream.getTracks().forEach((track) => track.stop());
    };
  }, []);

  const autoGrow = useCallback(() => {
    const element = textareaRef.current;
    if (!element) return;
    element.style.height = "0px";
    element.style.height = `${Math.min(120, element.scrollHeight)}px`;
  }, []);

  const sendText = async () => {
    const body: MessageBody = { kind: "text", text: text.trim() };
    if (replyTo) body.quote_msg_id = replyTo.msg_id;
    if (!body.text || sendingRef.current) return;
    sendingRef.current = true;
    setSending(true);
    const clientMsgId = newIdempotencyKey("msg");
    const response = await dispatch(ACTIONS.messageSend, { conv_id: convId, body, client_msg_id: clientMsgId }, {
      idempotencyKey: clientMsgId,
    });
    sendingRef.current = false;
    setSending(false);
    if (response.ok) {
      setText("");
      draftLoaded.current = "";
      onClearReply();
      await dispatch(ACTIONS.draftClear, { conv_id: convId }, { silent: true });
      toast("消息已进入本地 outbox，等待核心投影");
    }
  };

  const pickAndSend = async (accept: string, kind: "image" | "file") => {
    if (upload) return;
    const picked = await platform.files.open({ accept, multiple: false });
    if (!picked.ok) {
      if (picked.code !== "CANCELLED") toast(picked.message, "error");
      return;
    }
    const file = picked.value[0];
    if (!file) return;
    if (file.size > MAX_UPLOAD_BYTES) {
      toast(`「${file.name}」超出 1GB 上传预算，未开始上传`, "error");
      return;
    }
    await uploadAndSend(file, kind);
  };

  const uploadAndSend = async (
    file: { name: string; size: number; type: string; lastModified: number; bytes: () => Promise<Uint8Array>; read?: (offset: number, length: number) => Promise<Uint8Array> },
    kind: "image" | "file" | "voice",
    durationMs?: number,
  ) => {
    const controller = new AbortController();
    abortRef.current = controller;
    setUpload({ name: file.name, sent: 0, total: file.size, cancelled: false });
    try {
      const media = await uploadFile({
        file,
        dispatch,
        signal: controller.signal,
        onProgress: (progress) => {
          setUpload({ name: file.name, sent: progress.sent, total: progress.total, cancelled: false });
        },
      });
      const body: MessageBody =
        kind === "voice"
          ? { kind: "voice", media_id: media.media_id, duration_ms: durationMs ?? 0, name: media.name }
          : { kind, media_id: media.media_id, name: media.name, size: media.size, mime: media.mime };
      const clientMsgId = newIdempotencyKey("msg");
      const response = await dispatch(ACTIONS.messageSend, { conv_id: convId, body, client_msg_id: clientMsgId }, {
        idempotencyKey: clientMsgId,
      });
      if (response.ok) {
        toast(`已上传 ${formatBytes(media.size)} · ${media.digest_algorithm} ${media.digest.slice(0, 16)}…`);
      }
    } catch (error) {
      if (error instanceof UploadCancelled) {
        toast("上传已取消，服务端会话已撤销");
      } else if (error instanceof UploadError) {
        toast(error.error.message, "error");
      } else {
        toast(error instanceof Error ? error.message : "上传失败", "error");
      }
    } finally {
      abortRef.current = null;
      setUpload(null);
    }
  };

  const cancelUpload = () => {
    abortRef.current?.abort();
    setUpload(null);
  };

  const startRecording = async () => {
    if (recording) return;
    const mediaDevices = navigator.mediaDevices;
    if (!mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
      toast("当前环境不支持录音（需要麦克风与 MediaRecorder）", "error");
      return;
    }
    try {
      const stream = await mediaDevices.getUserMedia({ audio: true });
      const recorder = new MediaRecorder(stream);
      const chunks: Blob[] = [];
      recorder.ondataavailable = (event) => {
        if (event.data.size) chunks.push(event.data);
      };
      recorder.onstop = async () => {
        stream.getTracks().forEach((track) => track.stop());
        if (recordTimer.current) clearInterval(recordTimer.current);
        setRecording(null);
        const durationMs = Date.now() - recordStart.current;
        const blob = new Blob(chunks, { type: recorder.mimeType || "audio/webm" });
        if (!blob.size) {
          toast("没有录到声音", "error");
          return;
        }
        const file = {
          name: `语音-${new Date().toISOString().slice(11, 19)}.webm`,
          size: blob.size,
          type: blob.type,
          lastModified: Date.now(),
          bytes: async () => new Uint8Array(await blob.arrayBuffer()),
        };
        await uploadAndSend(file, "voice", durationMs);
      };
      recorder.start();
      recorderRef.current = recorder;
      recordStart.current = Date.now();
      setRecording({ seconds: 0 });
      recordTimer.current = setInterval(() => {
        setRecording({ seconds: Math.floor((Date.now() - recordStart.current) / 1000) });
      }, 500);
    } catch (error) {
      toast(error instanceof Error && /denied|permission/i.test(error.message) ? "麦克风权限被拒绝" : "无法开始录音", "error");
    }
  };

  const stopRecording = (send: boolean) => {
    const recorder = recorderRef.current;
    if (!recorder) return;
    if (!send) recorder.onstop = null;
    recorder.stop();
    recorderRef.current = null;
    if (recordTimer.current) clearInterval(recordTimer.current);
    setRecording(null);
    if (!send) {
      recorder.stream.getTracks().forEach((track) => track.stop());
      toast("已取消录音");
    }
  };

  const percent = upload && upload.total ? Math.min(100, Math.round((upload.sent / upload.total) * 100)) : 0;

  return (
    <div className="shrink-0 border-t border-[var(--line)] bg-[var(--surface-2)] px-3 pb-2 pt-1.5">
      {replyTo ? (
        <div className="mb-1.5 flex items-center gap-2 rounded border-l-2 border-[var(--accent)] bg-[var(--surface-3)] px-2 py-1 text-[9.5px] text-[var(--ink-2)]">
          <span className="truncate">引用 {replyTo.sender.name}：{replyTo.body.text?.slice(0, 60) ?? "[消息]"}</span>
          <button type="button" aria-label="取消引用" onClick={onClearReply} className="ml-auto text-[var(--muted)] hover:text-[var(--ink)]">
            <X size={11} />
          </button>
        </div>
      ) : null}

      {upload ? (
        <div className="mb-1.5 rounded border border-[var(--line-strong)] bg-[var(--surface)] p-2" data-testid="upload-progress">
          <div className="flex items-center justify-between text-[9.5px] text-[var(--ink-2)]">
            <span className="truncate">{upload.name} · {formatBytes(upload.sent)} / {formatBytes(upload.total)}</span>
            <button type="button" onClick={cancelUpload} className="ml-2 text-[var(--danger)] hover:underline" data-testid="upload-cancel">
              取消
            </button>
          </div>
          <div className="mt-1 h-1 overflow-hidden rounded bg-[var(--surface-3)]" role="progressbar" aria-valuenow={percent} aria-valuemin={0} aria-valuemax={100}>
            <div className="h-full bg-[var(--accent)] transition-[width]" style={{ width: `${percent}%` }} />
          </div>
        </div>
      ) : null}

      {recording ? (
        <div className="mb-1.5 flex items-center gap-2 rounded border border-[var(--danger)]/40 bg-[var(--danger)]/5 px-2 py-1.5 text-[10px] text-[var(--danger)]">
          <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[var(--danger)]" />
          录音中 {recording.seconds}s
          <div className="ml-auto flex gap-1">
            <Button size="sm" variant="ghost" onClick={() => stopRecording(false)} data-testid="record-cancel">
              <Trash2 size={11} /> 取消
            </Button>
            <Button size="sm" variant="primary" onClick={() => stopRecording(true)} data-testid="record-stop">
              <Square size={11} /> 结束并发送
            </Button>
          </div>
        </div>
      ) : null}

      <div className="flex items-end gap-1.5">
        <div className="flex items-center gap-0.5">
          <IconButton label="选择图片" onClick={() => void pickAndSend("image/*", "image")} disabled={Boolean(upload) || Boolean(recording)} data-testid="composer-image">
            <ImagePlus size={16} />
          </IconButton>
          <IconButton label="发送文件（单文件上限 1GB）" onClick={() => void pickAndSend("*/*", "file")} disabled={Boolean(upload) || Boolean(recording)} data-testid="composer-file">
            <FilePlus2 size={16} />
          </IconButton>
          <IconButton
            label={recording ? "结束录音" : "录制语音"}
            onClick={() => (recording ? stopRecording(true) : void startRecording())}
            disabled={Boolean(upload)}
            data-testid="composer-voice"
          >
            <Mic size={16} className={recording ? "text-[var(--danger)]" : undefined} />
          </IconButton>
          <div className="relative">
            <IconButton label="表情" onClick={() => setEmojiOpen((open) => !open)} data-testid="composer-emoji">
              <Smile size={16} />
            </IconButton>
            {emojiOpen ? (
              <>
                <div className="fixed inset-0 z-20" onClick={() => setEmojiOpen(false)} />
                <div className="pop-in absolute bottom-9 left-0 z-30 grid w-[220px] grid-cols-8 gap-1 rounded-lg border border-[var(--line-strong)] bg-[var(--surface-2)] p-2 shadow-[var(--shadow-pop)]">
                  {EMOJIS.map((emoji) => (
                    <button
                      key={emoji}
                      type="button"
                      onClick={() => {
                        setText((value) => value + emoji);
                        textareaRef.current?.focus();
                      }}
                      className="grid h-6 w-6 place-items-center rounded text-[15px] hover:bg-[var(--surface-3)]"
                      data-testid={`emoji-${emoji}`}
                    >
                      {emoji}
                    </button>
                  ))}
                </div>
              </>
            ) : null}
          </div>
        </div>

        <div className={cn("min-w-0 flex-1 rounded-lg border border-[var(--line-strong)] bg-[var(--surface)]", "focus-within:border-[var(--accent)]")}>
          <textarea
            ref={textareaRef}
            value={text}
            rows={1}
            onChange={(event) => {
              setText(event.target.value);
              autoGrow();
            }}
            onKeyDown={(event) => {
              const accelerator = event.metaKey || event.ctrlKey;
              const wantsSend =
                event.key === "Enter" &&
                !event.shiftKey &&
                !event.nativeEvent.isComposing &&
                (enterToSend || accelerator);
              if (wantsSend) {
                event.preventDefault();
                void sendText();
              }
            }}
            placeholder={enterToSend ? "输入消息，Enter 发送，Shift+Enter 换行" : "输入消息，⌘/Ctrl+Enter 发送"}
            aria-label="消息输入"
            className="max-h-[120px] w-full resize-none bg-transparent px-2.5 py-2 text-[12px] text-[var(--ink)] outline-none placeholder:text-[var(--muted)]"
            data-testid="composer-input"
          />
        </div>

        <Button
          variant="primary"
          size="md"
          className="h-8 shrink-0"
          disabled={!text.trim()}
          pending={sending}
          onClick={() => void sendText()}
          data-testid="composer-send"
        >
          <Send size={13} /> 发送
        </Button>
      </div>
    </div>
  );
}
