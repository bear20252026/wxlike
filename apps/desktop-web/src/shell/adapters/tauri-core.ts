import {
  domainError,
  fail,
  ok,
  type ActionRequest,
  type ActionResponse,
  type CoreCapabilities,
  type CoreHandle,
  type ErrorCode,
  type EventCursor,
  type EventEnvelope,
  type Unsubscribe,
} from "../core-contract";
import { tauriBridge } from "../platform/port";

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

interface RawResponse {
  ok?: boolean;
  data?: unknown;
  error?: { code?: string; message?: string; field_errors?: Record<string, string>; retryable?: boolean; retry_after_ms?: number };
}

/**
 * Tauri adapter. Rust owns transport, auth and projection; this layer only
 * normalises envelopes and never turns a failure into a success.
 */
export function createTauriCore(invoke: Invoke): CoreHandle {
  return {
    async dispatch<T>(request: ActionRequest): Promise<ActionResponse<T>> {
      let raw: RawResponse;
      try {
        raw = (await invoke("core_dispatch", { request })) as RawResponse;
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        if (/not found|unknown command/i.test(message)) {
          return fail(
            domainError("NOT_IMPLEMENTED", "宿主未挂载 core_dispatch（等待 G01-02 集成）", request.request_id),
          );
        }
        if (/offline|network|connection|timed out|timeout/i.test(message)) {
          return fail(domainError("OFFLINE", "核心不可达", request.request_id, { retryable: true }));
        }
        return fail(domainError("INTERNAL", message.slice(0, 300), request.request_id));
      }
      if (!raw || raw.ok !== true) {
        const error = raw?.error ?? {};
        return fail({
          code: normaliseCode(error.code),
          message: error.message ?? "核心返回未知错误",
          field_errors: error.field_errors,
          retryable: Boolean(error.retryable),
          retry_after_ms: error.retry_after_ms,
          request_id: request.request_id,
        });
      }
      return ok(raw.data as T);
    },

    subscribe(cursor: EventCursor | null, handler: (event: EventEnvelope) => void): Unsubscribe {
      const listen = tauriBridge()?.event?.listen;
      if (!listen) return () => undefined;
      let disposed = false;
      let unlisten: Unsubscribe | null = null;
      listen("core://event", (event) => {
        const envelope = event.payload as EventEnvelope | undefined;
        if (!envelope || typeof envelope.event_type !== "string") return;
        handler(envelope);
      })
        .then((fn) => {
          if (disposed) fn();
          else unlisten = fn;
        })
        .catch(() => undefined);
      if (cursor) {
        invoke("core_subscribe", { cursor }).catch(() => undefined);
      }
      return () => {
        disposed = true;
        unlisten?.();
      };
    },

    async capabilities(): Promise<CoreCapabilities> {
      try {
        const raw = (await invoke("core_capabilities")) as Partial<CoreCapabilities> | undefined;
        return {
          contract_version: raw?.contract_version ?? null,
          transport: "tauri",
          actions: Array.isArray(raw?.actions) ? raw.actions : [],
          qr_login: Boolean(raw?.qr_login),
          secrets_os_backed: Boolean(raw?.secrets_os_backed),
          sync_range: Boolean(raw?.sync_range),
          notes: raw?.notes,
        };
      } catch {
        return {
          contract_version: null,
          transport: "tauri",
          actions: [],
          qr_login: false,
          secrets_os_backed: false,
          sync_range: false,
          notes: ["core_capabilities 未挂载，按全部能力未知处理"],
        };
      }
    },
  };
}

function normaliseCode(code: string | undefined): ErrorCode {
  const allowed = [
    "UNAUTHENTICATED",
    "FORBIDDEN",
    "VALIDATION",
    "CONFLICT",
    "RATE_LIMITED",
    "REQUIRES_CONFIRMATION",
    "DEPENDENCY_UNAVAILABLE",
    "NOT_IMPLEMENTED",
    "OFFLINE",
    "CANCELLED",
    "INTERNAL",
  ] as const;
  return (allowed as readonly string[]).includes(code ?? "") ? (code as (typeof allowed)[number]) : "INTERNAL";
}
