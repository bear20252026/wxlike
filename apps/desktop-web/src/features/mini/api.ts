/**
 * Mini-app client (G11-05): mini.* — MiniAppPort contract surface.
 * Every endpoint raises until G08 lands; the page renders the explicit
 * upstream state. Manifest digest verification is client-side and real:
 * the downloaded package must hash to the release's manifest_digest before
 * an install is ever confirmed.
 */

import { ApiError, idemKey, request } from "../../lib/http";
import { sha256Hex } from "../files/bytes";
import type { MiniAppRecord, MiniAppRelease, MiniGrant, MiniPorts, MiniScope, RequestOpts } from "./model";

export function isUpstreamMissing(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 404 || err.status === 501);
}

export class MiniAppApi implements MiniPorts {
  async list(_opts?: RequestOpts): Promise<MiniAppRecord[]> {
    return request<MiniAppRecord[]>("GET", "/api/miniapps");
  }

  async releases(appId: string, opts?: RequestOpts): Promise<MiniAppRelease[]> {
    return request<MiniAppRelease[]>(
      "GET",
      `/api/miniapps/${encodeURIComponent(appId)}/releases`,
      { signal: opts?.signal },
    );
  }

  async install(
    appId: string,
    version: string,
    manifestDigest: string,
    opts?: RequestOpts,
  ): Promise<{ installed_version: string }> {
    // Verify before telling the server anything: fetch the signed package,
    // hash it, compare to the release record. Tampered package → refuse.
    const bytes = await this.fetchPackage(appId, version, opts);
    const digest = await sha256Hex(bytes);
    if (digest !== manifestDigest.toLowerCase()) {
      throw new ApiError(0, "小程序包校验失败：manifest digest 与发布记录不一致", `/api/miniapps/${appId}`, "server");
    }
    return request<{ installed_version: string }>(
      "POST",
      `/api/mini-installs`,
      {
        body: { app_id: appId, version, manifest_digest: manifestDigest },
        idempotencyKey: opts?.idempotencyKey ?? idemKey("mini-install"),
        signal: opts?.signal,
      },
    );
  }

  private async fetchPackage(appId: string, version: string, opts?: RequestOpts): Promise<Uint8Array> {
    const res = await fetch(`/api/miniapps/${encodeURIComponent(appId)}/releases/${encodeURIComponent(version)}/package`, {
      signal: opts?.signal,
    });
    if (!res.ok) throw new ApiError(res.status, `小程序包下载失败 (${res.status})`, `/api/miniapps/${appId}`, "server");
    return new Uint8Array(await res.arrayBuffer());
  }

  async grants(appId?: string, opts?: RequestOpts): Promise<MiniGrant[]> {
    const path = appId ? `/api/mini-grants?app_id=${encodeURIComponent(appId)}` : "/api/mini-grants";
    const body = await request<MiniGrant[] | { items?: MiniGrant[] }>("GET", path, { signal: opts?.signal });
    return Array.isArray(body) ? body : (body.items ?? []);
  }

  async grant(appId: string, scopes: MiniScope[], opts?: RequestOpts): Promise<MiniGrant> {
    return request<MiniGrant>("POST", "/api/mini-grants", {
      body: { app_id: appId, scopes },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("mini-grant"),
      signal: opts?.signal,
    });
  }

  async revoke(appId: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("DELETE", `/api/mini-grants/${encodeURIComponent(appId)}`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("mini-revoke"),
      signal: opts?.signal,
    });
  }

  async uninstall(appId: string, opts?: RequestOpts): Promise<void> {
    await request<unknown>("DELETE", `/api/mini-installs/${encodeURIComponent(appId)}`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("mini-uninstall"),
      signal: opts?.signal,
    });
  }
}
