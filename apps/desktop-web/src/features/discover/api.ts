/**
 * Discover client (G11-05): service.* / vision.* — G07 ServiceCatalogPort
 * contract. All endpoints raise until G07 lands; the page renders explicit
 * upstream states. Camera scanning itself is real (see ScanPanel).
 */

import { ApiError, idemKey, request } from "../../lib/http";
import type { DiscoverPorts, OrderState, Quote, RequestOpts, ServiceEntry, ServiceOrder, VisionJob } from "./model";

export function isUpstreamMissing(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 404 || err.status === 501);
}

export class DiscoverApi implements DiscoverPorts {
  async services(_opts?: RequestOpts): Promise<ServiceEntry[]> {
    return request<ServiceEntry[]>("GET", "/api/services");
  }

  async quote(serviceId: string, opts?: RequestOpts): Promise<Quote> {
    return request<Quote>("GET", `/api/quotes?service_id=${encodeURIComponent(serviceId)}`, {
      signal: opts?.signal,
    });
  }

  async createOrder(serviceId: string, quoteId: string, opts?: RequestOpts): Promise<ServiceOrder> {
    return request<ServiceOrder>("POST", "/api/orders", {
      body: { service_id: serviceId, quote_id: quoteId },
      idempotencyKey: opts?.idempotencyKey ?? idemKey("service-order"),
      signal: opts?.signal,
    });
  }

  async order(orderId: string, opts?: RequestOpts): Promise<ServiceOrder> {
    return request<ServiceOrder>("GET", `/api/orders/${encodeURIComponent(orderId)}`, {
      signal: opts?.signal,
    });
  }

  async orders(_opts?: RequestOpts): Promise<ServiceOrder[]> {
    const body = await request<ServiceOrder[] | { items?: ServiceOrder[] }>("GET", "/api/orders");
    return Array.isArray(body) ? body : (body.items ?? []);
  }

  async refundOrder(orderId: string, opts?: RequestOpts): Promise<ServiceOrder> {
    return request<ServiceOrder>("POST", `/api/orders/${encodeURIComponent(orderId)}/refund`, {
      idempotencyKey: opts?.idempotencyKey ?? idemKey("service-refund"),
      signal: opts?.signal,
    });
  }

  async recognizeImage(image: Blob, opts?: RequestOpts): Promise<VisionJob> {
    return this.submitVision("recognition", image, undefined, opts);
  }

  async translateImage(image: Blob, targetLang: string, opts?: RequestOpts): Promise<VisionJob> {
    return this.submitVision("translation", image, targetLang, opts);
  }

  private async submitVision(
    kind: "recognition" | "translation",
    image: Blob,
    targetLang: string | undefined,
    opts?: RequestOpts,
  ): Promise<VisionJob> {
    const form = new FormData();
    form.append("image", image, "capture.jpg");
    if (targetLang) form.append("target_lang", targetLang);
    const tok = (await import("../../lib/http")).token();
    const res = await fetch(`/api/vision/${kind}`, {
      method: "POST",
      headers: tok ? { authorization: `Bearer ${tok}` } : {},
      body: form,
      signal: opts?.signal,
    });
    if (!res.ok) {
      const text = await res.text().catch(() => "");
      let message = text.trim() || `识别请求失败 (${res.status})`;
      try {
        const parsed = JSON.parse(text) as { error?: string };
        if (parsed.error) message = parsed.error;
      } catch {
        // plain-text error body
      }
      throw new ApiError(res.status, message, `/api/vision/${kind}`);
    }
    return (await res.json()) as VisionJob;
  }

  async visionJob(jobId: string, opts?: RequestOpts): Promise<VisionJob> {
    return request<VisionJob>("GET", `/api/vision/jobs/${encodeURIComponent(jobId)}`, {
      signal: opts?.signal,
    });
  }
}

export function orderStateLabel(state: OrderState): string {
  switch (state) {
    case "created": return "已创建";
    case "paying": return "支付中";
    case "processing": return "处理中";
    case "fulfilled": return "已完成";
    case "failed": return "失败";
    case "refunding": return "退款中";
    case "refunded": return "已退款";
    case "pending_reconciliation": return "对账中";
    default: return String(state);
  }
}
