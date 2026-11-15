/**
 * Mini-app client (G11-05): mini.* — MiniAppPort contract surface.
 * Every endpoint raises until G08 lands; the page renders the explicit
 * upstream state. Manifest digest verification is client-side and real:
 * the downloaded package must hash to the release's manifest_digest before
 * an install is ever confirmed.
 */

import { ApiError, blockedError, idemKey, request } from "../../lib/http";
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

  /**
   * 安装依赖"下载签名包 → sha256 校验 → 提交"的二进制链路，没有 JSON 流式
   * 核心提供者：显式阻塞（#294，同视频上传口径）。把包体字节流搬进 CoreHandle
   * 需要独立的流式 action 契约，不在本控制面切片里伪造安装成功。
   */
  async install(
    appId: string,
    _version: string,
    _manifestDigest: string,
    _opts?: RequestOpts,
  ): Promise<{ installed_version: string }> {
    throw blockedError(`/api/miniapps/${appId}`, "小程序安装依赖包体二进制下载与 sha256 校验，尚无流式核心提供者");
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
