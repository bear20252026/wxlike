// Type declarations for @wxlike/mini-sdk (mini-host.v1).
// The runtime tables live in generated.js, generated from the JSON contract.

export type Scope = "storage" | "network" | "camera" | "location" | "file" | "payment";

export type Method =
  | "storage.get"
  | "storage.set"
  | "storage.remove"
  | "network.request"
  | "camera.capture"
  | "location.get"
  | "file.pick"
  | "pay.request";

export type ErrorCode =
  | "unknown_app"
  | "origin_mismatch"
  | "session_closed"
  | "session_mismatch"
  | "replayed"
  | "not_granted"
  | "denied"
  | "cancelled"
  | "invalid_call"
  | "budget"
  | "host"
  | "port";

export interface BridgeRequest {
  action: Method;
  app: string;
  origin: string;
  request_id: string;
  params: Record<string, unknown>;
}

export type BridgeResponse =
  | { ok: true; result: unknown }
  | { ok: false; error: { code: ErrorCode; message: string } };

/** The host shell supplies this; it adds app/origin from the session. */
export type Transport = (request: {
  action: Method;
  request_id: string;
  params: Record<string, unknown>;
}) => Promise<BridgeResponse>;

export interface NetworkResult {
  status: number;
  body: unknown;
}

export interface PhotoResult {
  reference: string;
}

export interface LocationResult {
  latitude: number;
  longitude: number;
}

export type FilePurpose = "avatar" | "attachment" | "export";

export interface FileResult {
  name: string;
  size: number;
  reference: string;
}

export interface PaymentResult {
  reference: string;
  status: "settled" | "pending";
}

export interface MiniClient {
  contractVersion: string;
  storage: {
    get(key: string): Promise<string | null>;
    set(key: string, value: string): Promise<boolean>;
    remove(key: string): Promise<boolean>;
  };
  network: {
    request(
      url: string,
      options?: { method?: "GET" | "POST"; body?: string },
    ): Promise<NetworkResult>;
  };
  camera: { capture(): Promise<PhotoResult> };
  location: { get(): Promise<LocationResult> };
  file: { pick(purpose: FilePurpose): Promise<FileResult> };
  payments: {
    pay(input: {
      amount: string;
      currency: string;
      orderId: string;
    }): Promise<PaymentResult>;
  };
}

export class MiniHostError extends Error {
  code: ErrorCode;
  method?: Method;
  requestId?: string;
}

export function createClient(
  transport: Transport,
  options?: { requestId?: () => string },
): MiniClient;

export function isMiniHostError(error: unknown): error is MiniHostError;

export const CONTRACT_VERSION: string;
export const METHODS: Readonly<Record<string, Method>>;
export const SCOPES: Readonly<Record<Method, Scope>>;
export const ERROR_CODES: readonly ErrorCode[];
