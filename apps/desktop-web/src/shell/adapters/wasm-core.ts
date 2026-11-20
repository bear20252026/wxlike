import {
  domainError,
  fail,
  isErrorCode,
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

/**
 * G01-03 交付的浏览器核心适配器：动态加载 `packages/wasm-client`
 * （wasm-pack `--target web`，包名/入口 `wxlike-core`）。
 *
 * 真实 API 以 `crates/client-web/src/lib.rs` 的 wasm-bindgen 出口为准：
 *
 * - `await WxlikeCore.new(baseUrl)`：async 工厂（要开 IndexedDB、抢跨 tab 租约），
 *   失败 reject（私有模式 / 已有实例）；
 * - `dispatch(action, paramsJson): Promise<string>`：出入参都是 **JSON 字符串**；
 * - `capabilities(): string`：能力上报 JSON 字符串；
 * - `subscribe(onEvent): Promise<WxlikeSubscription>`：每条事件以 JSON 字符串喂回调；
 * - `pump_once(): Promise<string>` / `shutdown()` / `set_token()` / `retry_outbox()`。
 *
 * 这里只做 JSON ↔ 契约的搬运，不写第二套同步逻辑，也不把加载失败伪造成离线能力。
 */

/** wasm-pack 生成的模块形状（兜底类型；装上产物后以产物自带 .d.ts 为准）。 */
interface WasmModule {
  default?: (input?: unknown) => Promise<unknown>;
  init?: (input?: unknown) => Promise<unknown>;
  WxlikeCore: {
    new (baseUrl: string): Promise<WasmCoreInstance>;
  };
}

interface WasmSubscription {
  unsubscribe(): void;
}

interface WasmCoreInstance {
  dispatch(action: string, paramsJson: string): Promise<string>;
  capabilities(): string;
  pump_once(): Promise<string>;
  subscribe(onEvent: (text: string) => void): Promise<WasmSubscription>;
  shutdown(): void;
  set_token?(token?: string | null): void;
  retry_outbox?(key: string): Promise<boolean>;
}

export interface WasmCoreOptions {
  /** 服务端基址；默认 `VITE_CORE_BASE_URL`，再退回当前 origin。 */
  baseUrl?: string;
  /** 测试注入用；默认动态 `import("wxlike-core")`。 */
  loader?: () => Promise<unknown>;
}

interface RawError {
  code?: unknown;
  message?: unknown;
  field_errors?: unknown;
  retryable?: unknown;
  retry_after_ms?: unknown;
}

interface RawResponse {
  ok?: unknown;
  data?: unknown;
  error?: RawError;
}

function resolveBaseUrl(explicit?: string): string {
  if (explicit) return explicit;
  const fromEnv = import.meta.env.VITE_CORE_BASE_URL;
  if (typeof fromEnv === "string" && fromEnv) return fromEnv;
  if (typeof window !== "undefined" && window.location) return window.location.origin;
  return "";
}

function normaliseCode(code: unknown): ErrorCode {
  return typeof code === "string" && isErrorCode(code) ? code : "INTERNAL";
}

/** 把 wasm 核心返回的 JSON 文本折成 CoreHandle 的 ActionResponse。 */
function parseResponse(text: string, requestId: string): ActionResponse<unknown> {
  let raw: RawResponse = {};
  try {
    raw = JSON.parse(text) as RawResponse;
  } catch {
    return fail(domainError("INTERNAL", "WASM 核心返回了非 JSON 响应", requestId));
  }
  if (raw && raw.ok === true) return ok(raw.data);
  const error = raw?.error ?? {};
  const fieldErrors =
    error.field_errors && typeof error.field_errors === "object"
      ? (error.field_errors as Record<string, string>)
      : undefined;
  return fail({
    code: normaliseCode(error.code),
    message: typeof error.message === "string" ? error.message : "核心返回未知错误",
    field_errors: fieldErrors,
    retryable: Boolean(error.retryable),
    retry_after_ms: typeof error.retry_after_ms === "number" ? error.retry_after_ms : undefined,
    request_id: requestId,
  });
}

async function loadModule(loader?: () => Promise<unknown>): Promise<WasmModule> {
  const mod = (await (loader ? loader() : import("wxlike-core"))) as unknown as WasmModule;
  // wasm-pack `--target web` 的默认导出是 `init()`；调用后 wasm 实例才就绪。
  const init = mod.default ?? mod.init;
  if (typeof init === "function") await init();
  return mod;
}

/** 打开浏览器核心。失败（产物缺失 / IndexedDB 不可用）抛给调用方，不吞错。 */
export async function createWasmCore(options: WasmCoreOptions = {}): Promise<CoreHandle> {
  const mod = await loadModule(options.loader);
  const instance = await mod.WxlikeCore.new(resolveBaseUrl(options.baseUrl));

  return {
    async dispatch<T>(request: ActionRequest): Promise<ActionResponse<T>> {
      // Rust 侧从 params_json 里认 idempotency_key（写命令的硬要求）。
      const params: Record<string, unknown> = {
        ...((request.params ?? {}) as Record<string, unknown>),
      };
      if (request.idempotency_key) params.idempotency_key = request.idempotency_key;
      const text = await instance.dispatch(request.action_id, JSON.stringify(params));
      return parseResponse(text, request.request_id) as ActionResponse<T>;
    },

    subscribe(_cursor: EventCursor | null, handler: (event: EventEnvelope) => void): Unsubscribe {
      let disposed = false;
      let subscription: WasmSubscription | null = null;
      instance
        .subscribe((text) => {
          if (disposed) return;
          let envelope: EventEnvelope;
          try {
            envelope = JSON.parse(text) as EventEnvelope;
          } catch {
            return;
          }
          // `__wxlike` 控制消息（stream-end / error）不是事件，不能冒充事件喂给 UI。
          if (!envelope || typeof envelope.event_type !== "string") return;
          handler(envelope);
        })
        .then((handle) => {
          if (disposed) handle.unsubscribe();
          else subscription = handle;
        })
        .catch(() => {
          // 订阅失败不伪造事件：UI 侧表现为没有实时事件，而不是假在线。
        });
      return () => {
        disposed = true;
        subscription?.unsubscribe();
      };
    },

    async capabilities(): Promise<CoreCapabilities> {
      let parsed: Partial<CoreCapabilities> | null = null;
      try {
        parsed = JSON.parse(instance.capabilities()) as Partial<CoreCapabilities> | null;
      } catch {
        return {
          contract_version: null,
          transport: "wasm",
          actions: [],
          qr_login: false,
          secrets_os_backed: false,
          sync_range: false,
          notes: ["WASM 核心能力上报解析失败"],
        };
      }
      const raw = parsed ?? {};
      return {
        contract_version: typeof raw.contract_version === "string" ? raw.contract_version : null,
        transport: "wasm",
        actions: Array.isArray(raw.actions) ? raw.actions : [],
        qr_login: Boolean(raw.qr_login),
        secrets_os_backed: Boolean(raw.secrets_os_backed),
        sync_range: Boolean(raw.sync_range),
        notes: Array.isArray(raw.notes) ? raw.notes : undefined,
      };
    },
  };
}
