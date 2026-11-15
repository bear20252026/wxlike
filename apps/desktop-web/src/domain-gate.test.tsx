/**
 * 卡面第 1/4 条的可见那一半：七域里每一条活动页顶部都必须自己说清"我现在的会话
 * 来源是什么"，而且**页面能点不等于完成**——没提供者的域要显式阻塞，已迁移的域在
 * 开发夹具宿主里不许冒充"已接通"。
 *
 * `app.tsx` 的 `DomainSessionGate` 文案与 `lib/domain-registry.ts` 同源，注释里写着
 * "用例读的是 data-testid"，这条就是那份用例（原来没有，属于"注释自称有断言"的形状）。
 */

import { screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { setTransport } from "./lib/http";
import { DOMAIN_BY_ROUTE, DOMAIN_REGISTRY, domainsInState } from "./lib/domain-registry";
import { navigate, type Route } from "./shell/router";
import { loginAs, renderApp } from "./test/helpers";

afterEach(() => {
  setTransport(null);
});

describe("组合根横幅：每条域的会话来源都要看得见、说得清", () => {
  it("钱包在开发夹具宿主里不许冒充「已接通认证核心」", async () => {
    renderApp({ hash: "#/login" });
    await loginAs("user-alpha", "demo1234");
    navigate({ id: "wallet" } as Route);

    const banner = await screen.findByTestId("domain-state-wallet");
    // 夹具就是夹具：文案必须点破"不连服务端、不作为验收证据"
    expect(banner.textContent).toContain("开发夹具");
    expect(banner.textContent).toContain("不作为验收证据");
    expect(banner.textContent).not.toContain("经认证 Rust 核心会话");
  });

  it("D6 退役完成：七域全部已迁移，阻塞名单为空", () => {
    // 七个业务域都已由 Rust 提供者给出 action，不再有 blocked 域。
    expect(domainsInState("blocked")).toEqual([]);
    expect(domainsInState("core")).toHaveLength(7);
  });

  it("注册表与现实同源：7 格已迁移 + 0 格阻塞，每格都有路由归属", () => {
    expect(domainsInState("core").map((domain) => domain.id)).toEqual(["wallet", "moments", "video", "calls", "files", "mini", "discover"]);
    expect(domainsInState("blocked").map((domain) => domain.id)).toEqual([]);
    // 已迁移的每一格都必须记着提供者的 SHA（卡面：消费者记录 provider SHA）
    const coreById = Object.fromEntries(domainsInState("core").map((d) => [d.id, d]));
    expect(coreById.wallet?.providerSha).toBe("71cc90c5");
    expect(coreById.calls?.providerSha).toBe("b3fbb40");
    expect(coreById.video?.providerSha).toBeTruthy();
    expect(coreById.moments?.providerSha).toBeTruthy();
    expect(coreById.files?.providerSha).toBeTruthy();
    expect(coreById.discover?.providerSha).toBeTruthy();
    expect(coreById.mini?.providerSha).toBeTruthy();
    // 每条路由都查得到归属；内容域占四条路由（朋友圈/公众号/收藏/表情）
    expect(Object.keys(DOMAIN_BY_ROUTE).sort()).toEqual(
      ["wallet", "moments", "articles", "favorites", "stickers", "video", "calls", "files", "mini", "discover"].sort(),
    );
    expect(DOMAIN_REGISTRY).toHaveLength(7);
  });
});
