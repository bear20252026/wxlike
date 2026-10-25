/**
 * PlatformPort — the thin seam between G11 features and OS capabilities
 * (clipboard, notifications, file dialogs). The web build implements it with
 * browser APIs; the Tauri shell replaces it once G00-02's bridge lands. Code
 * below the port never calls `navigator.*` directly, so the same feature runs
 * in both shells and in tests (injected port).
 *
 * Requirement source: G11-01 requirement 4 (系统复制通过PlatformPort).
 */

export interface PlatformPort {
  /** Copy text via the OS clipboard. Resolves false when the host refuses. */
  copyText(text: string): Promise<boolean>;
  /** Open a link outside the app shell (browser/tab). */
  openExternal(url: string): void;
  /** Best-effort local save of a downloaded blob through the host. */
  saveBlob(blob: Blob, filename: string): void;
  /** Camera/mic acquisition for scan/call features (G11-03/G11-05). */
  media(kind: "camera" | "microphone"): Promise<MediaStream>;
}

async function copyTextWeb(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fall through to the textarea path
  }
  // Legacy fallback (non-secure contexts): transient textarea + execCommand.
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "true");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  try {
    return document.execCommand("copy");
  } finally {
    area.remove();
  }
}

export const webPlatform: PlatformPort = {
  copyText: copyTextWeb,
  openExternal(url: string): void {
    // Only http(s) links leave the shell; anything else is treated as text.
    if (/^https?:\/\//i.test(url)) window.open(url, "_blank", "noopener,noreferrer");
  },
  saveBlob(blob: Blob, filename: string): void {
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.rel = "noopener";
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  },
  media(kind) {
    const constraints: MediaStreamConstraints =
      kind === "camera"
        ? { video: { facingMode: "environment" }, audio: false }
        : { audio: true, video: false };
    return navigator.mediaDevices.getUserMedia(constraints);
  },
};

let active: PlatformPort = webPlatform;

export function setPlatform(port: PlatformPort): void {
  active = port;
}

export function platform(): PlatformPort {
  return active;
}
