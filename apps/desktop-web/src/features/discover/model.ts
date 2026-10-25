/**
 * Types for the discover feature (G11-05): scan.* / service.* / vision.*.
 * Service catalog + vision belong to G07 (ServiceCatalogPort §3); the server
 * has neither yet — frozen contract shapes below, explicit upstream states in
 * the UI.
 */

export interface ServiceEntry {
  service_id: string;
  name: string;
  category: string;
  /** Where the tile goes: catalog detail / quote / order / refund / an app. */
  target:
    | { kind: "catalog" }
    | { kind: "quote" }
    | { kind: "order" }
    | { kind: "refund" }
    | { kind: "app"; app_id: string };
  icon?: string;
}

export interface Quote {
  quote_id: string;
  service_id: string;
  /** Minor units + currency, never floats. */
  price_minor: number;
  currency: string;
  expires_at: number;
  conditions?: string;
}

export type OrderState =
  | "created"
  | "paying"
  | "processing"
  | "fulfilled"
  | "failed"
  | "refunding"
  | "refunded"
  | "pending_reconciliation";

export interface ServiceOrder {
  order_id: string;
  service_id: string;
  quote_id: string;
  state: OrderState;
  provider_ref?: string;
  created_at: number;
  updated_at: number;
}

export interface ScanResult {
  /** Decoded payload (barcode/QR content). */
  raw: string;
  format: string;
  at: number;
}

export interface VisionJob {
  job_id: string;
  kind: "recognition" | "translation";
  state: "queued" | "running" | "done" | "failed";
  result_text?: string;
}

export interface RequestOpts {
  signal?: AbortSignal;
  idempotencyKey?: string;
}

export interface DiscoverPorts {
  services(opts?: RequestOpts): Promise<ServiceEntry[]>;
  quote(serviceId: string, opts?: RequestOpts): Promise<Quote>;
  createOrder(serviceId: string, quoteId: string, opts?: RequestOpts): Promise<ServiceOrder>;
  order(orderId: string, opts?: RequestOpts): Promise<ServiceOrder>;
  orders(opts?: RequestOpts): Promise<ServiceOrder[]>;
  refundOrder(orderId: string, opts?: RequestOpts): Promise<ServiceOrder>;
  /** 识物/翻译：图片走真实选图/拍摄，识别走 G07 vision 契约。 */
  recognizeImage(image: Blob, opts?: RequestOpts): Promise<VisionJob>;
  translateImage(image: Blob, targetLang: string, opts?: RequestOpts): Promise<VisionJob>;
  visionJob(jobId: string, opts?: RequestOpts): Promise<VisionJob>;
}

/** Injectable barcode decoder — the browser ships BarcodeDetector; tests stub it. */
export interface BarcodeDecoder {
  detect(source: CanvasImageSource): Promise<Array<{ rawValue: string; format: string }>>;
}
