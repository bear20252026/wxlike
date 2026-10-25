/**
 * 扫码/识物/翻译 (G11-05 requirement 3/5): real camera through
 * PlatformPort.media('camera') — no fake viewfinder. Barcode decoding uses
 * the platform BarcodeDetector when present (injectable for tests and for
 * browsers without it). 权限拒绝后能重试: a denied camera renders an explicit
 * retry path instead of a dead end. 识物/翻译 use a real image picker and a
 * real camera capture; the recognition itself is the G07 vision contract.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { errorText } from "../../lib/http";
import { platform } from "../../lib/platform";
import type { BarcodeDecoder, DiscoverPorts } from "./model";

export type CameraPhase = "idle" | "requesting" | "live" | "denied" | "failed";

interface ScanPanelDeps {
  ports: DiscoverPorts;
  /** Injectable decoder (tests) — default: the platform BarcodeDetector. */
  decoder?: () => BarcodeDecoder | null;
}

export function ScanPanel({ ports, decoder = defaultDecoder }: ScanPanelDeps) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const rafRef = useRef<number | null>(null);
  const [phase, setPhase] = useState<CameraPhase>("idle");
  const [error, setError] = useState("");
  const [scanText, setScanText] = useState("");
  const [history, setHistory] = useState<Array<{ raw: string; format: string; at: number }>>([]);
  const visionRef = useRef<HTMLInputElement>(null);
  const [visionBusy, setVisionBusy] = useState(false);
  const [visionResult, setVisionResult] = useState("");
  const [visionMode, setVisionMode] = useState<"recognition" | "translation">("recognition");

  const stopCamera = useCallback(() => {
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    const stream = streamRef.current;
    streamRef.current = null;
    if (stream) {
      for (const track of stream.getTracks()) track.stop();
    }
    if (videoRef.current) videoRef.current.srcObject = null;
  }, []);

  useEffect(() => stopCamera, [stopCamera]);

  const startCamera = async () => {
    if (phase === "requesting" || phase === "live") return;
    setError("");
    setPhase("requesting");
    try {
      const stream = await platform().media("camera");
      streamRef.current = stream;
      const video = videoRef.current;
      if (video) {
        video.srcObject = stream;
        try {
          await video.play();
        } catch {
          // autoplay refusal (or jsdom's unimplemented play) is not fatal
        }
      }
      setPhase("live");
      pumpFrames();
    } catch (err) {
      stopCamera();
      if (err instanceof DOMException && err.name === "NotAllowedError") {
        setPhase("denied");
        setError("摄像头权限被拒绝。请在系统/浏览器设置中允许后，点击“重试”重新授权。");
      } else {
        setPhase("failed");
        setError(errorText(err));
      }
    }
  };

  const decoderLoop = async (video: HTMLVideoElement) => {
    const detector = decoder();
    if (!detector) return;
    try {
      const codes = await detector.detect(video);
      for (const code of codes) {
        if (!code.rawValue) continue;
        setScanText(code.rawValue);
        setHistory((prev) =>
          prev.some((item) => item.raw === code.rawValue)
            ? prev
            : [{ raw: code.rawValue, format: code.format, at: Date.now() }, ...prev].slice(0, 10),
        );
      }
    } catch {
      // decode errors on blurry frames are routine; next frame retries
    }
  };

  const pumpFrames = () => {
    const step = () => {
      const video = videoRef.current;
      if (!video || !streamRef.current) return; // camera gone → loop ends
      void decoderLoop(video);
      schedule();
    };
    const schedule = () => {
      // rAF in browsers; jsdom/some webviews lack it — timer fallback keeps
      // the decode loop alive without burning CPU.
      if (typeof requestAnimationFrame === "function") {
        rafRef.current = requestAnimationFrame(step);
      } else {
        rafRef.current = setTimeout(step, 200) as unknown as number;
      }
    };
    if (rafRef.current === null) schedule();
  };

  const runVision = async (file: File) => {
    setVisionBusy(true);
    setError("");
    try {
      const job =
        visionMode === "recognition"
          ? await ports.recognizeImage(file)
          : await ports.translateImage(file, "zh");
      const done = job.state === "done" ? job : await ports.visionJob(job.job_id);
      setVisionResult(done.result_text ?? "（识别中，稍后在结果区查看）");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setVisionBusy(false);
    }
  };

  return (
    <section className="rounded-xl border border-zinc-200 bg-white p-4" data-testid="scan-panel">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-zinc-700">扫一扫 / 识物 / 翻译</h3>
        <div className="flex gap-2">
          {phase === "live" ? (
            <GhostButtonish onClick={stopCamera}>关闭摄像头</GhostButtonish>
          ) : (
            <GhostButtonish onClick={() => void startCamera()} testId="scan-start">
              {phase === "denied" ? "重试授权" : "打开摄像头"}
            </GhostButtonish>
          )}
        </div>
      </div>

      <div className="relative mt-2 overflow-hidden rounded-lg bg-black" data-testid="scan-viewfinder">
        <video ref={videoRef} muted playsInline className="aspect-video w-full object-cover" />
        {phase !== "live" ? (
          <div className="absolute inset-0 flex items-center justify-center text-sm text-zinc-300">
            {phase === "idle" ? "摄像头未开启" : phase === "requesting" ? "正在请求摄像头…" : phase === "denied" ? "摄像头权限被拒绝" : "摄像头不可用"}
          </div>
        ) : null}
      </div>

      {error ? (
        <p className="mt-2 text-sm text-red-600" role="alert" data-testid="scan-error">
          {error}
        </p>
      ) : null}

      <p className="mt-2 break-all font-mono text-xs text-zinc-500" data-testid="scan-result">
        {scanText ? `识别结果：${scanText}` : "对准二维码/条码"}
      </p>
      {history.length > 0 ? (
        <ul className="mt-1 space-y-0.5 text-[11px] text-zinc-400" data-testid="scan-history">
          {history.map((item) => (
            <li key={`${item.raw}-${item.at}`}>
              {new Date(item.at).toLocaleTimeString("zh-CN")} · {item.format} · {item.raw}
            </li>
          ))}
        </ul>
      ) : null}

      <div className="mt-3 flex items-center gap-2">
        <select
          value={visionMode}
          onChange={(event) => setVisionMode(event.target.value as "recognition" | "translation")}
          data-testid="vision-mode"
          className="rounded-md border border-zinc-200 px-2 py-1.5 text-sm"
        >
          <option value="recognition">识物</option>
          <option value="translation">拍照翻译</option>
        </select>
        <GhostButtonish onClick={() => visionRef.current?.click()} testId="vision-pick">
          {visionBusy ? "识别中…" : "选择图片"}
        </GhostButtonish>
        <input
          ref={visionRef}
          type="file"
          accept="image/*"
          capture="environment"
          className="hidden"
          data-testid="vision-input"
          onChange={(event) => {
            const file = event.target.files?.[0];
            event.target.value = "";
            if (file) void runVision(file);
          }}
        />
      </div>
      {visionResult ? (
        <p className="mt-2 whitespace-pre-wrap text-sm text-zinc-700" data-testid="vision-result">
          {visionResult}
        </p>
      ) : null}
    </section>
  );
}

function GhostButtonish({ children, onClick, testId }: { children: React.ReactNode; onClick?: () => void; testId?: string }) {
  return (
    <button
      type="button"
      onClick={onClick}
      data-testid={testId}
      className="rounded-lg border border-zinc-200 bg-white px-3 py-1.5 text-sm text-zinc-600 transition hover:bg-zinc-100"
    >
      {children}
    </button>
  );
}

function defaultDecoder(): BarcodeDecoder | null {
  const ctor = (window as unknown as { BarcodeDetector?: new (options?: { formats?: string[] }) => BarcodeDecoder }).BarcodeDetector;
  if (!ctor) return null;
  try {
    return new ctor({ formats: ["qr_code", "ean_13", "code_128", "upc_a", "upc_e"] });
  } catch {
    return null;
  }
}
