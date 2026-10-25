/**
 * Types for the mini-app feature (G11-05): mini.* — MiniAppPort contract
 * (`/miniapps`, `/miniapps/{id}/releases`, `/mini-installs`, `/mini-grants`).
 * The server has no mini-app surface yet (G08 upstream) — everything here is
 * the frozen contract shape the card prescribes.
 */

export interface MiniAppRelease {
  version: string;
  manifest_digest: string;
  signature: string;
  /** Where the signed package bytes come from (short-lived URL). */
  package_url: string;
  released_at: number;
  revoked: boolean;
}

export interface MiniAppRecord {
  /** 每张应用卡映射真实 app_id —— the card grid must render this value. */
  app_id: string;
  name: string;
  icon?: string;
  description?: string;
  /** 可信 origin（网页型小程序宿主 sandbox 检查依据）。 */
  origins: string[];
  latest?: MiniAppRelease;
  installed_version?: string;
}

export type MiniScope =
  | "profile.read"
  | "friends.read"
  | "media.upload"
  | "wallet.pay"
  | "location.read"
  | string;

export interface MiniGrant {
  app_id: string;
  scopes: MiniScope[];
  granted_at: number;
  expires_at?: number;
}

export type InstallStage = "idle" | "verifying" | "installing" | "installed" | "failed";

export interface RequestOpts {
  signal?: AbortSignal;
  idempotencyKey?: string;
}

export interface MiniPorts {
  list(opts?: RequestOpts): Promise<MiniAppRecord[]>;
  releases(appId: string, opts?: RequestOpts): Promise<MiniAppRelease[]>;
  install(appId: string, version: string, manifestDigest: string, opts?: RequestOpts): Promise<{ installed_version: string }>;
  grants(appId?: string, opts?: RequestOpts): Promise<MiniGrant[]>;
  grant(appId: string, scopes: MiniScope[], opts?: RequestOpts): Promise<MiniGrant>;
  revoke(appId: string, opts?: RequestOpts): Promise<void>;
  uninstall(appId: string, opts?: RequestOpts): Promise<void>;
}

/** Sandbox origin decision — pure and unit-testable. */
export type OriginDecision =
  | { ok: true }
  | { ok: false; reason: "not-https" | "not-allowlisted" | "unparseable" };

export function checkOrigin(rawUrl: string, allowlist: string[]): OriginDecision {
  let parsed: URL;
  try {
    parsed = new URL(rawUrl);
  } catch {
    return { ok: false, reason: "unparseable" };
  }
  if (parsed.protocol !== "https:") return { ok: false, reason: "not-https" };
  const host = parsed.host.toLowerCase();
  const allowed = allowlist.some((entry) => {
    const normalized = entry.toLowerCase();
    if (normalized.startsWith("*.")) {
      const suffix = normalized.slice(1); // "*.example.com" → ".example.com"
      return host.endsWith(suffix) || host === normalized.slice(2);
    }
    return host === normalized;
  });
  return allowed ? { ok: true } : { ok: false, reason: "not-allowlisted" };
}

/** iframe sandbox flags for a web mini-app: no same-origin, no top navigation. */
export const MINI_SANDBOX_ATTRS =
  "allow-scripts allow-forms allow-popups allow-modals";
