/**
 * 域注册表（issue #294 / G10）：七域各自的会话来源，一处说清，不许各画一张。
 *
 * 卡面要的是"七域逐项迁移为 CoreHandle/action 适配"，而 #293 只交付了钱包这一格
 * 的 Rust 提供者。所以本表把**事实**写下来，而不是把愿望写下来：
 *
 * - `state: "core"`  —— 有已合入 main 的提供者，请求一律经 `CoreHandle.dispatch`；
 * - `state: "blocked"` —— 没有提供者，界面必须显示"阻塞/未接入"，**不能**因为
 *   页面能点开、能导航就算完成，更不能返回假成功。
 *
 * 三处消费这份表，保证不会漂移：
 *   1. `app.tsx` 的组合根横幅（用户看得见的阻塞状态）；
 *   2. `lib/core-transport.ts` 的路由与拒绝；
 *   3. `scripts/check-core-transport.mjs` 的静态允许名单（只放 `state === "core"`）。
 */

export type DomainState = "core" | "blocked";

export interface DomainStatus {
  /** 路由 id（`shell/router.tsx` 里的 Route["id"]，钱包页所在域）。 */
  id: string;
  /** 对应的业务模块目录：`src/features/<featureDir>/`。 */
  featureDir: string;
  /** 中文界面名，直接进横幅文案。 */
  label: string;
  state: DomainState;
  /** 提供者出处：#293 合入 main 的 SHA；没有提供者就是 null（如实标空，不编）。 */
  providerSha: string | null;
  /** 阻塞原因/依据，写给人看，也写给断言比对。 */
  reason: string;
  /** 该域已注册的桥 action（`state === "core"` 才有意义）。 */
  actions: readonly string[];
}

import { ACTIONS } from "../shell/core-contract";

/** #293 的钱包竖切片：`origin/main` 上的提交。消费者（本卡）按卡面要求记录此 SHA。 */
export const WALLET_PROVIDER_SHA = "71cc90c5";

/** id 字符串只在 `shell/core-contract.ts` 的 `ACTIONS` 里有一份，这里只做聚合。 */
const WALLET_ACTIONS = [
  ACTIONS.walletBalanceGet,
  ACTIONS.walletLedgerPage,
  ACTIONS.walletTransfer,
  ACTIONS.walletRedpacketCreate,
  ACTIONS.walletRedpacketGet,
  ACTIONS.walletRedpacketClaim,
  ACTIONS.walletRedpacketList,
] as const;

/**
 * 六域阻塞名单。**这张表就是"不许用页面能点当验收"的机器可读版本**：
 * 谁给某个域补上提供者，就把那条改成 `state: "core"` + 记录 SHA，静态检查与
 * 组合根横幅会跟着一起换；反过来，没提供者却把域标成 core，静态检查直接红。
 */
export const DOMAIN_REGISTRY: readonly DomainStatus[] = [
  {
    id: "wallet",
    featureDir: "commerce",
    label: "钱包",
    state: "core",
    providerSha: WALLET_PROVIDER_SHA,
    reason: `余额/流水/转账/红包的读写全部经 #293 的认证桥 action（${WALLET_ACTIONS.length} 条），Bearer 不进 webview`,
    actions: WALLET_ACTIONS,
  },
  {
    id: "moments",
    featureDir: "content",
    label: "内容（朋友圈/公众号/收藏/表情）",
    state: "blocked",
    providerSha: null,
    reason: "#293 只交付钱包竖切片，内容域没有 Rust 提供者；遗留 HTTP 出口拿不到认证会话",
    actions: [],
  },
  {
    id: "video",
    featureDir: "video",
    label: "视频号",
    state: "blocked",
    providerSha: null,
    reason: "#293 只交付钱包竖切片，视频域没有 Rust 提供者；遗留 HTTP 出口拿不到认证会话",
    actions: [],
  },
  {
    id: "calls",
    featureDir: "calls",
    label: "通话",
    state: "blocked",
    providerSha: null,
    reason: "#293 只交付钱包竖切片，通话域没有 Rust 提供者；信令与 RTC 凭据都不能由界面自己发 HTTP 获取",
    actions: [],
  },
  {
    id: "files",
    featureDir: "files",
    label: "文件",
    state: "blocked",
    providerSha: null,
    reason: "#293 只交付钱包竖切片，文件域没有 Rust 提供者；上传下载需走 media 桥（#295 尚未合入）",
    actions: [],
  },
  {
    id: "mini",
    featureDir: "mini",
    label: "小程序",
    state: "blocked",
    providerSha: null,
    reason: "#293 只交付钱包竖切片，小程序域没有 Rust 提供者；包体拉取与 iframe 来源还需宿主精确策略",
    actions: [],
  },
  {
    id: "discover",
    featureDir: "discover",
    label: "发现",
    state: "blocked",
    providerSha: null,
    reason: "#293 只交付钱包竖切片，发现域没有 Rust 提供者；识图/翻译不能由界面自己带 bearer",
    actions: [],
  },
];

/** 路由 id → 域状态（`app.tsx` 按当前路由查这份表画横幅）。 */
export const DOMAIN_BY_ROUTE: Record<string, DomainStatus> = Object.fromEntries(
  DOMAIN_REGISTRY.flatMap((domain) => routeIdsFor(domain).map((route) => [route, domain] as const)),
);

/**
 * 一个域可能占多个路由（内容域占朋友圈/公众号/收藏/表情）。
 * 这份映射与 `shell/router.tsx` 的 Route id 对齐；写进函数里是为了让静态检查能
 * 只看 `src/features/**` 的目录名，不必解析路由表。
 */
function routeIdsFor(domain: DomainStatus): readonly string[] {
  switch (domain.featureDir) {
    case "content":
      return ["moments", "articles", "favorites", "stickers"];
    case "commerce":
      return ["wallet"];
    default:
      return [domain.id];
  }
}

export function domainsInState(state: DomainState): DomainStatus[] {
  return DOMAIN_REGISTRY.filter((domain) => domain.state === state);
}

/** 已迁移域的 action 全集：静态检查据此核对路由表与 capabilities 口径。 */
export function coreActionCensus(): string[] {
  return domainsInState("core").flatMap((domain) => [...domain.actions]);
}

export function blockedDomainCensus(): string[] {
  return domainsInState("blocked").map((domain) => `${domain.featureDir}:${domain.label}`);
}
