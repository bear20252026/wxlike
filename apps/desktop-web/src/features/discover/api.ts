/**
 * Discover client (G11-05): service.* / vision.* — G07 ServiceCatalogPort
 * contract. All endpoints raise until G07 lands; the page renders explicit
 * upstream states. Camera scanning itself is real (see ScanPanel).
 */

import { ApiError, blockedError, idemKey, request } from "../../lib/http";
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

  /**
   * 识图/翻译（multipart 图片 + 外部 AI 能力）没有核心提供者：显式阻塞（#294，
   * 同视频上传口径）。vision crate 的 VisionService 在无 provider 时本就拒绝，
   * 把图片字节流搬进 CoreHandle 需要独立的流式 action 契约与外部 AI 凭据，
   * 不在本控制面切片里伪造识别结果。
   */
  async recognizeImage(_image: Blob, _opts?: RequestOpts): Promise<VisionJob> {
    throw blockedError("/api/vision/recognition", "识图（multipart + 外部 AI）尚无核心提供者");
  }

  async translateImage(_image: Blob, _targetLang: string, _opts?: RequestOpts): Promise<VisionJob> {
    throw blockedError("/api/vision/translation", "图片翻译（multipart + 外部 AI）尚无核心提供者");
  }

  async visionJob(jobId: string, _opts?: RequestOpts): Promise<VisionJob> {
    throw blockedError(`/api/vision/jobs/${jobId}`, "识图/翻译作业查询尚无核心提供者");
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
