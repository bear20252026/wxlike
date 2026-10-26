import { describe, expect, it, vi } from "vitest";
import { newRequest, ACTIONS } from "../core-contract";
import { createTauriCore } from "./tauri-core";

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function request() {
  return newRequest(ACTIONS.sessionProjection, {});
}

describe("createTauriCore — invoke 异常映射", () => {
  it("宿主未挂载命令 → NOT_IMPLEMENTED", async () => {
    const invoke: Invoke = async () => {
      throw new Error("command core_dispatch not found");
    };
    const response = await createTauriCore(invoke).dispatch(request());
    expect(response.ok).toBe(false);
    if (!response.ok) {
      expect(response.error.code).toBe("NOT_IMPLEMENTED");
      expect(response.error.message).toContain("宿主未挂载");
    }
  });

  it("网络类异常 → OFFLINE 且可重试", async () => {
    const invoke: Invoke = async () => {
      throw new Error("network unreachable: connection refused");
    };
    const response = await createTauriCore(invoke).dispatch(request());
    expect(response.ok).toBe(false);
    if (!response.ok) {
      expect(response.error.code).toBe("OFFLINE");
      expect(response.error.retryable).toBe(true);
    }
  });

  it("其余异常 → INTERNAL（不吞错也不误报成功）", async () => {
    const invoke: Invoke = async () => {
      throw new Error("serde deserialize blew up");
    };
    const response = await createTauriCore(invoke).dispatch(request());
    expect(response.ok).toBe(false);
    if (!response.ok) expect(response.error.code).toBe("INTERNAL");
  });
});

describe("createTauriCore — 错误信封 code 归一 (normaliseCode)", () => {
  const cases: Array<[string | undefined, string]> = [
    ["UNAUTHENTICATED", "UNAUTHENTICATED"],
    ["VALIDATION", "VALIDATION"],
    ["REQUIRES_CONFIRMATION", "REQUIRES_CONFIRMATION"],
    ["OFFLINE", "OFFLINE"],
    ["weird-unknown-code", "INTERNAL"],
    [undefined, "INTERNAL"],
  ];

  it.each(cases)("code %s → %s", async (code, expected) => {
    const invoke: Invoke = async () =>
      ({
        ok: false,
        error: { code, message: "boom" },
      }) as unknown as Awaited<ReturnType<Invoke>>;
    const response = await createTauriCore(invoke).dispatch(request());
    expect(response.ok).toBe(false);
    if (!response.ok) expect(response.error.code).toBe(expected);
  });

  it("成功信封透传 data；缺失 ok 视为失败", async () => {
    const okInvoke: Invoke = async () => ({ ok: true, data: { phase: "authenticated" } });
    const good = await createTauriCore(okInvoke).dispatch<{ phase: string }>(request());
    expect(good.ok && good.data.phase).toBe("authenticated");

    const badShapeInvoke: Invoke = async () => ({}) as unknown as Awaited<ReturnType<Invoke>>;
    const bad = await createTauriCore(badShapeInvoke).dispatch(request());
    expect(bad.ok).toBe(false);
    if (!bad.ok) expect(bad.error.message).toContain("未知错误");
  });

  it("field_errors 与 retry 元数据保留", async () => {
    const invoke: Invoke = async () =>
      ({
        ok: false,
        error: { code: "VALIDATION", message: "参数不合法", field_errors: { days: "只能是 7/30/90" }, retry_after_ms: 500 },
      }) as unknown as Awaited<ReturnType<Invoke>>;
    const response = await createTauriCore(invoke).dispatch(request());
    expect(response.ok).toBe(false);
    if (!response.ok) {
      expect(response.error.field_errors?.days).toBe("只能是 7/30/90");
      expect(response.error.retry_after_ms).toBe(500);
    }
  });
});

describe("createTauriCore — capabilities 降级", () => {
  it("invoke 失败时按全部能力未知返回", async () => {
    const invoke = vi.fn(async () => {
      throw new Error("nope");
    });
    const caps = await createTauriCore(invoke).capabilities();
    expect(caps.transport).toBe("tauri");
    expect(caps.actions).toEqual([]);
    expect(caps.notes?.[0]).toContain("core_capabilities");
  });
});
