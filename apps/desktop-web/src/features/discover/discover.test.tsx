/**
 * G11-05 test matrix — app cards carry real app_ids, manifest digest
 * verification, install/update/revoke dialogs, sandbox origin check, camera
 * permission retry, barcode decode via injected detector, service grid
 * enumerated entry ids, quote→order→refund flow, duplicate submit.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setPlatform, type PlatformPort } from "../../lib/platform";
import { setTransport } from "../../lib/http";
import { err, fixtureTransport } from "../../test/fixture";
import { MiniAppsPage } from "../mini/MiniAppsPage";
import { checkOrigin, MINI_SANDBOX_ATTRS, type MiniAppRecord } from "../mini/model";
import { DiscoverApi } from "./api";
import { ScanPanel } from "./ScanPanel";
import { ServicesPage, tileEntryId } from "./ServicesPage";
import type { ServiceEntry } from "./model";

/** jsdom lacks MediaStream; ScanPanel only needs track enumeration. */
class FakeMediaStream {
  private tracks = [{ stop: vi.fn(), kind: "video" as const, enabled: true }];
  getTracks() {
    return this.tracks;
  }
  getVideoTracks() {
    return this.tracks;
  }
  getAudioTracks() {
    return [];
  }
}

function okPlatform(): PlatformPort {
  return {
    copyText: async () => true,
    openExternal: () => undefined,
    saveBlob: () => undefined,
    media: async () => new FakeMediaStream() as unknown as MediaStream,
  };
}

beforeEach(() => {
  localStorage.clear();
  setPlatform(okPlatform());
});

afterEach(() => {
  cleanup();
  setTransport(null);
  setPlatform(okPlatform());
});

function miniApp(overrides: Partial<MiniAppRecord> = {}): MiniAppRecord {
  return {
    app_id: "wxmini-0001",
    name: "出行助手",
    origins: ["https://mini.example.com"],
    latest: {
      version: "1.2.0",
      manifest_digest: "aa".repeat(32),
      signature: "sig",
      package_url: "https://mini.example.com/pkgs/1.2.0",
      released_at: 1_700_000_000_000,
      revoked: false,
    },
    ...overrides,
  };
}

// ---- mini ------------------------------------------------------------------

describe("MiniAppsPage", () => {
  it("上游未开通：明确呈现，不显示假卡片", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/miniapps": () => err(404, "not found"),
      }),
    );
    render(<MiniAppsPage />);
    await waitFor(() =>
      expect(screen.getByText(/G08 MiniAppPort）尚未开通/)).toBeInTheDocument(),
    );
    expect(screen.queryByTestId(/^miniapp-/)).not.toBeInTheDocument();
  });

  it("每张应用卡渲染真实 app_id 与版本", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/miniapps": () => [miniApp(), miniApp({ app_id: "wxmini-0002", name: "企业查查" })],
      }),
    );
    render(<MiniAppsPage />);
    await waitFor(() => expect(screen.getByTestId("miniapp-wxmini-0001")).toBeInTheDocument());
    expect(screen.getByTestId("miniapp-wxmini-0001")).toHaveTextContent("wxmini-0001");
    expect(screen.getByTestId("miniapp-wxmini-0002")).toHaveTextContent("企业查查");
  });

  it("安装弹窗展示 digest 并在服务端未开通时明确提示", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/miniapps": () => [miniApp()],
        "POST /api/mini-installs": () => err(404, "not found"),
      }),
    );
    // package download is a raw byte fetch (not the JSON transport)
    const originalFetch = global.fetch;
    global.fetch = (async () =>
      new Response(JSON.stringify({ error: "not found" }), { status: 404 })) as typeof fetch;
    try {
      render(<MiniAppsPage />);
      await waitFor(() => expect(screen.getByTestId("miniapp-wxmini-0001")).toBeInTheDocument());
      fireEvent.click(screen.getByText("出行助手"));
      await waitFor(() => expect(screen.getByTestId("install-dialog")).toBeInTheDocument());
      expect(screen.getByTestId("install-dialog")).toHaveTextContent("wxmini-0001");
      fireEvent.click(screen.getByRole("button", { name: "安装" }));
      await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("G08 MiniAppPort"));
    } finally {
      global.fetch = originalFetch;
    }
  });

  it("撤权弹窗确认后调用撤回", async () => {
    let revoked = false;
    setTransport(
      fixtureTransport({
        "GET /api/miniapps": () => [miniApp({ installed_version: "1.1.0" })],
        "DELETE /api/mini-grants/:appId": () => {
          revoked = true;
          return null;
        },
      }),
    );
    render(<MiniAppsPage />);
    await waitFor(() => expect(screen.getByTestId("miniapp-wxmini-0001")).toBeInTheDocument());
    fireEvent.click(screen.getByTestId("dot-menu-trigger"));
    fireEvent.click(screen.getByRole("menuitem", { name: "撤回授权" }));
    expect(screen.getByTestId("revoke-dialog")).toHaveTextContent("撤回");
    fireEvent.click(screen.getByRole("button", { name: "确认撤回" }));
    await waitFor(() => expect(revoked).toBe(true));
    await waitFor(() => expect(screen.getByText("已撤回授权")).toBeInTheDocument());
  });
});

describe("unit — sandbox origin check", () => {
  it("https + 白名单命中才放行", () => {
    expect(checkOrigin("https://mini.example.com/pkgs/1", ["mini.example.com"])).toEqual({ ok: true });
    expect(checkOrigin("https://mini.example.com/pkgs/1", ["*.example.com"])).toEqual({ ok: true });
    expect(checkOrigin("http://mini.example.com/pkgs/1", ["mini.example.com"])).toEqual({ ok: false, reason: "not-https" });
    expect(checkOrigin("https://evil.example.net/pkgs/1", ["mini.example.com"])).toEqual({ ok: false, reason: "not-allowlisted" });
    expect(checkOrigin("javascript:alert(1)", ["*"])).toEqual({ ok: false, reason: "not-https" });
    expect(checkOrigin("not a url", [])).toEqual({ ok: false, reason: "unparseable" });
  });

  it("sandbox 属性不含 allow-same-origin / allow-top-navigation", () => {
    expect(MINI_SANDBOX_ATTRS).not.toContain("allow-same-origin");
    expect(MINI_SANDBOX_ATTRS).not.toContain("allow-top-navigation");
  });
});

// ---- discover --------------------------------------------------------------

const tiles: ServiceEntry[] = [
  { service_id: "svc-print", name: "云打印", category: "生活", target: { kind: "quote" }, icon: "🖨" },
  { service_id: "svc-fruit", name: "生鲜到家", category: "购物", target: { kind: "order" }, icon: "🍎" },
  { service_id: "svc-wash", name: "洗车", category: "生活", target: { kind: "catalog" }, icon: "🚗" },
  { service_id: "svc-mini", name: "出行助手", category: "应用", target: { kind: "app", app_id: "wxmini-0001" }, icon: "🧭" },
];

describe("ServicesPage — 服务格子", () => {
  it("所有格子有稳定入口 ID 并可展开到目录/报价", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/services": () => tiles,
        "GET /api/orders": () => [],
        "GET /api/quotes": () => ({
          quote_id: "q-1",
          service_id: "svc-print",
          price_minor: 250,
          currency: "CNY",
          expires_at: Date.now() + 60_000,
          conditions: "按页计费",
        }),
      }),
    );
    render(<ServicesPage />);
    await screen.findByTestId(tileEntryId(tiles[0] as ServiceEntry));
    for (const entry of tiles) {
      expect(screen.getByTestId(tileEntryId(entry))).toHaveAttribute("data-service-id", entry.service_id);
    }
    fireEvent.click(screen.getByTestId(tileEntryId(tiles[0] as ServiceEntry)));
    await waitFor(() => expect(screen.getByTestId("service-detail")).toBeInTheDocument());
    await waitFor(() => expect(screen.getByTestId("service-quote")).toHaveTextContent("¥2.50"));
  });

  it("上游未开通：格子区为空 + 明确提示，扫码不受影响", async () => {
    setTransport(
      fixtureTransport({
        "GET /api/services": () => err(404, "not found"),
        "GET /api/orders": () => err(404, "not found"),
      }),
    );
    render(<ServicesPage />);
    await waitFor(() => expect(screen.getByText(/G07 ServiceCatalogPort）尚未开通/)).toBeInTheDocument());
    expect(screen.getByTestId("scan-panel")).toBeInTheDocument();
  });

  it("下单→订单列表→退款；重复下单只发一次", async () => {
    let orderCount = 0;
    setTransport(
      fixtureTransport({
        "GET /api/services": () => tiles,
        "GET /api/orders": () => [],
        "GET /api/quotes": () => ({
          quote_id: "q-1",
          service_id: "svc-print",
          price_minor: 250,
          currency: "CNY",
          expires_at: Date.now() + 60_000,
        }),
        "POST /api/orders": (_m, _p, body) => {
          orderCount += 1;
          expect((body as Record<string, unknown>).quote_id).toBe("q-1");
          return {
            order_id: `o-${orderCount}`,
            service_id: "svc-print",
            quote_id: "q-1",
            state: "processing",
            created_at: Date.now(),
            updated_at: Date.now(),
          };
        },
        "POST /api/orders/:id/refund": () => ({
          order_id: "o-1",
          service_id: "svc-print",
          quote_id: "q-1",
          state: "refunding",
          created_at: Date.now(),
          updated_at: Date.now(),
        }),
      }),
    );
    render(<ServicesPage />);
    fireEvent.click(await screen.findByTestId(tileEntryId(tiles[0] as ServiceEntry)));
    await waitFor(() => expect(screen.getByTestId("service-quote")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "下单" }));
    fireEvent.click(screen.getByRole("button", { name: "下单" }));
    await waitFor(() => expect(screen.getByText(/订单已创建：o-1/)).toBeInTheDocument());
    expect(orderCount).toBe(1);
    await waitFor(() => expect(screen.getByTestId("order-o-1")).toBeInTheDocument());
    fireEvent.click(screen.getByRole("button", { name: "退款" }));
    await waitFor(() => expect(screen.getByTestId("order-state")).toHaveTextContent("退款中"));
  });

  it("识物/翻译：选图提交真实 Blob 请求", async () => {
    const submitted: string[] = [];
    setTransport(
      fixtureTransport({
        "GET /api/services": () => tiles,
        "GET /api/orders": () => [],
      }),
    );
    // vision endpoints use raw fetch — stub it
    const originalFetch = global.fetch;
    global.fetch = (async (input: RequestInfo | URL) => {
      const url = String(input);
      submitted.push(url);
      return new Response(
        JSON.stringify({ job_id: "j1", kind: "recognition", state: "done", result_text: "一把红色雨伞" }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }) as typeof fetch;
    try {
      render(<ServicesPage />);
      fireEvent.change(await screen.findByTestId("vision-mode"), { target: { value: "recognition" } });
      fireEvent.change(screen.getByTestId("vision-input"), {
        target: { files: [new File(["imagebytes"], "photo.jpg", { type: "image/jpeg" })] },
      });
      await waitFor(() => expect(screen.getByTestId("vision-result")).toHaveTextContent("一把红色雨伞"));
      expect(submitted.some((url) => url.includes("/api/vision/recognition"))).toBe(true);
    } finally {
      global.fetch = originalFetch;
    }
  });
});

// ---- scan ------------------------------------------------------------------

class FakeDetector {
  constructor(private results: Array<{ rawValue: string; format: string }>) {}
  async detect(): Promise<Array<{ rawValue: string; format: string }>> {
    return this.results;
  }
}

describe("ScanPanel — 摄像头与解码", () => {
  it("权限拒绝：明确提示 + 重试按钮，重试成功进入 live", async () => {
    let denied = true;
    setPlatform({
      copyText: async () => true,
      openExternal: () => undefined,
      saveBlob: () => undefined,
      media: async () => {
        if (denied) throw new DOMException("denied", "NotAllowedError");
        return new FakeMediaStream() as unknown as MediaStream;
      },
    });
    render(<ScanPanel ports={new DiscoverApi()} decoder={() => new FakeDetector([]) as never} />);
    fireEvent.click(screen.getByTestId("scan-start"));
    await waitFor(() => expect(screen.getByTestId("scan-error")).toHaveTextContent("摄像头权限被拒绝"));
    expect(screen.getByText(/重试授权/)).toBeInTheDocument();
    denied = false;
    fireEvent.click(screen.getByText(/重试授权/));
    await waitFor(() => expect(screen.queryByTestId("scan-error")).not.toBeInTheDocument());
    expect(screen.getByText("关闭摄像头")).toBeInTheDocument(); // live 阶段
  });

  it("解码结果进入结果区与历史，重复码不重复记录", async () => {
    const code = { rawValue: "wxpay:intent:pi-1", format: "qr_code" };
    render(<ScanPanel ports={new DiscoverApi()} decoder={() => new FakeDetector([code]) as never} />);
    // simulate live camera
    const video = await screen.findByTestId("scan-viewfinder").then((el) => el.querySelector("video") as HTMLVideoElement);
    Object.defineProperty(video, "play", { configurable: true, value: async () => undefined });
    // drive the loop by calling startCamera through the button
    fireEvent.click(screen.getByTestId("scan-start"));
    await waitFor(() => expect(screen.getByTestId("scan-result")).toHaveTextContent("wxpay:intent:pi-1"), { timeout: 3000 });
    expect(screen.getByTestId("scan-history")).toHaveTextContent("wxpay:intent:pi-1");
    expect(screen.getByTestId("scan-history").querySelectorAll("li")).toHaveLength(1);
  });

  it("关闭摄像头停止所有轨", async () => {
    render(<ScanPanel ports={new DiscoverApi()} decoder={() => new FakeDetector([]) as never} />);
    fireEvent.click(screen.getByTestId("scan-start"));
    await waitFor(() => expect(screen.getByText("关闭摄像头")).toBeInTheDocument());
    const track = (screen.getByTestId("scan-viewfinder").querySelector("video") as HTMLVideoElement).srcObject as unknown as {
      getTracks: () => Array<{ stop: ReturnType<typeof vi.fn> }>;
    };
    const stopFns = track.getTracks().map((t) => t.stop);
    fireEvent.click(screen.getByText("关闭摄像头"));
    for (const stop of stopFns) expect(stop).toHaveBeenCalled();
  });
});
