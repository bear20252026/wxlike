/**
 * 卡面第 1/4 条的可见那一半：七域里每一条活动页顶部都必须自己说清"我现在的会话
 * 来源是什么"，而且**页面能点不等于完成**——没提供者的域要显式阻塞，已迁移的域在
 * 开发夹具宿主里不许冒充"已接通"。
 *
 * `app.tsx` 的 `DomainSessionGate` 文案与 `lib/domain-registry.ts` 同源，注释里写着
 * "用例读的是 data-testid"，这条就是那份用例（原来没有，属于"注释自称有断言"的形状）。
 */

import { screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { setTransport } from "./lib/http";
import { DOMAIN_BY_ROUTE, DOMAIN_REGISTRY, domainsInState } from "./lib/domain-registry";
import { navigate, type Route } from "./shell/router";
import { fixtureTransport } from "./test/fixture";
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

  it("没有提供者的域：显式阻塞（role=alert），并写清不能当验收依据", async () => {
    // 阻塞域还挂着遗留 HTTP 出口；这里把出口钉成"一律 404"，用例只查横幅，
    // 绝不让 jsdom 真的去连本机服务端。
    setTransport(fixtureTransport({}));
    renderApp({ hash: "#/login" });
    await loginAs("user-alpha", "demo1234");
    navigate({ id: "mini" } as Route);

    const banner = await screen.findByTestId("domain-blocked-mini");
    expect(banner.getAttribute("role")).toBe("alert");
    expect(banner.textContent).toContain("未接入认证 Rust 核心（阻塞，非成功）");
    expect(banner.textContent).toContain("没有 Rust 提供者");
    expect(banner.textContent).toContain("不能当验收依据");
    // 核心那一格的横幅此刻不该出现在这个页面上
    expect(screen.queryByTestId("domain-state-wallet")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("domain-blocked-mini")).toBeInTheDocument());
  });

  it("注册表与现实同源：6 格已迁移 + 1 格阻塞，每格都有路由归属", () => {
    expect(domainsInState("core").map((domain) => domain.id)).toEqual(["wallet", "moments", "video", "calls", "files", "discover"]);
    expect(domainsInState("blocked").map((domain) => domain.id).sort()).toEqual([
      "mini",
    ]);
    // 已迁移的每一格都必须记着提供者的 SHA（卡面：消费者记录 provider SHA）
    const coreById = Object.fromEntries(domainsInState("core").map((d) => [d.id, d]));
    expect(coreById.wallet?.providerSha).toBe("71cc90c5");
    expect(coreById.calls?.providerSha).toBe("b3fbb40");
    expect(coreById.video?.providerSha).toBeTruthy();
    expect(coreById.moments?.providerSha).toBeTruthy();
    expect(coreById.files?.providerSha).toBeTruthy();
    expect(coreById.discover?.providerSha).toBeTruthy();
    for (const domain of domainsInState("blocked")) {
      expect(domain.providerSha, domain.id).toBeNull();
      expect(domain.actions, domain.id).toEqual([]);
    }
    // 每条路由都查得到归属；内容域占四条路由（朋友圈/公众号/收藏/表情）
    expect(Object.keys(DOMAIN_BY_ROUTE).sort()).toEqual(
      ["wallet", "moments", "articles", "favorites", "stickers", "video", "calls", "files", "mini", "discover"].sort(),
    );
    expect(DOMAIN_REGISTRY).toHaveLength(7);
  });
});
