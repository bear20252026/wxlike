// Type declarations for @wxlike/mini-sdk (mini-host.v1).
// Method, Scope and ErrorCode are generated from the JSON contract in
// generated.d.ts; only structural result types are written by hand here.

import type { ErrorCode, Method, Scope } from "./generated.js";

export type { ErrorCode, Method, Scope };
export {
  CONTRACT_VERSION,
  CONTRACT_VERSIONS,
  ERROR_CODES,
  METHODS,
  SCOPES,
} from "./generated.js";

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
  status: "settled" | "pending" | "sandbox";
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

export const CONTRACT_VERSIONS: Readonly<{
  host: string;
  catalog: string;
  package: string;
}>;
