/**
 * G01-03 wasm 包（`packages/wasm-client`，wasm-pack `--target web`）的类型兜底。
 *
 * 产物本身带 `.d.ts` 且不进 git；装上 `wxlike-core` 后以产物类型为准。这里
 * 只在产物缺失时兜住 `npm run typecheck`，形状按
 * `crates/client-web/src/lib.rs` 的 wasm-bindgen 出口手写，不要凭想象改。
 */
declare module "wxlike-core" {
  export interface WxlikeSubscription {
    /** 退订；幂等。 */
    unsubscribe(): void;
    isUnsubscribed(): boolean;
    free(): void;
  }

  export interface WxlikeCore {
    /** action id + params JSON 字符串 → TS 形状 ActionResponse JSON 字符串。 */
    dispatch(action: string, paramsJson: string): Promise<string>;
    /** 能力上报 JSON 字符串（CoreCapabilities 形状）。 */
    capabilities(): string;
    /** 一轮 outbox 补发 → {attempted,sent,deferred,failed} JSON 字符串。 */
    pump_once(): Promise<string>;
    retry_outbox(key: string): Promise<boolean>;
    set_token(token?: string | null): void;
    /** 每条事件以 JSON 字符串喂 `onEvent`；返回订阅句柄。 */
    subscribe(onEvent: (text: string) => void): Promise<WxlikeSubscription>;
    /** 退出：取消在飞任务、释放租约；幂等。 */
    shutdown(): void;
    free(): void;
  }

  export interface WxlikeCoreStatic {
    /** async 工厂：打开 IndexedDB / 抢跨 tab 租约，失败 reject。属性形式＝静态方法。 */
    new: (baseUrl: string) => Promise<WxlikeCore>;
  }

  export const WxlikeCore: WxlikeCoreStatic;

  /** wasm-pack `--target web` 的默认导出：实例化 wasm 后 resolve。 */
  export default function init(input?: unknown): Promise<unknown>;
  export function initSync(module: WebAssembly.Module): void;
}
