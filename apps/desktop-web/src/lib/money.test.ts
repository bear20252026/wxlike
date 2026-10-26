import { describe, expect, it } from "vitest";
import { CNY, formatMoney, isJsonSafe, parseToMinor } from "./money";

describe("formatMoney — 整数最小单位格式化", () => {
  it("常规金额", () => {
    expect(formatMoney(0)).toBe("¥0.00");
    expect(formatMoney(1999)).toBe("¥19.99");
    expect(formatMoney(5)).toBe("¥0.05");
    expect(formatMoney(100000)).toBe("¥1000.00");
    expect(formatMoney(-250)).toBe("¥-2.50");
    expect(formatMoney("12345")).toBe("¥123.45");
    expect(formatMoney(1_000_000n)).toBe("¥10000.00");
  });

  it("非整数最小单位输入直接拒绝", () => {
    expect(() => formatMoney("12.34")).toThrow("金额不是整数最小单位");
    expect(() => formatMoney("abc")).toThrow("金额不是整数最小单位");
    expect(() => formatMoney("1e3")).toThrow("金额不是整数最小单位");
  });

  it("零小数位币种", () => {
    expect(formatMoney(1234, { code: "JPY", scale: 0, symbol: "¥" })).toBe("¥1234");
  });
});

describe("parseToMinor — 解析与非法输入", () => {
  it("合法输入", () => {
    expect(parseToMinor("19.99")).toBe(1999n);
    expect(parseToMinor("0.1")).toBe(10n);
    expect(parseToMinor("100")).toBe(10000n);
    expect(parseToMinor("100.5")).toBe(10050n);
    expect(parseToMinor("  8.00  ")).toBe(800n);
    expect(parseToMinor("0.01")).toBe(1n);
  });

  it("非法输入全部被拒（包括浮点陷阱形态）", () => {
    expect(() => parseToMinor("")).toThrow("请输入金额");
    expect(() => parseToMinor("   ")).toThrow("请输入金额");
    expect(() => parseToMinor("abc")).toThrow("金额格式不正确");
    expect(() => parseToMinor("1.2.3")).toThrow("金额格式不正确");
    expect(() => parseToMinor("-1")).toThrow("金额格式不正确");
    expect(() => parseToMinor("+1")).toThrow("金额格式不正确");
    expect(() => parseToMinor("1e3")).toThrow("金额格式不正确");
    expect(() => parseToMinor("0x10")).toThrow("金额格式不正确");
    expect(() => parseToMinor("1,000")).toThrow("金额格式不正确");
    expect(() => parseToMinor("0.005")).toThrow("最多 2 位小数");
    expect(() => parseToMinor("0")).toThrow("金额必须大于零");
    expect(() => parseToMinor("0.00")).toThrow("金额必须大于零");
    expect(parseToMinor("00.10")).toBe(10n); // 前导零是合法数字
  });

  it("大额仍在安全范围内解析", () => {
    expect(parseToMinor("90071992547409.91")).toBe(9007199254740991n);
  });

  it("isJsonSafe 边界", () => {
    expect(isJsonSafe(0n)).toBe(true);
    expect(isJsonSafe(2n ** 53n)).toBe(true);
    expect(isJsonSafe(2n ** 53n + 1n)).toBe(false);
    expect(isJsonSafe(-(2n ** 53n))).toBe(true);
    expect(isJsonSafe(-(2n ** 53n) - 1n)).toBe(false);
  });

  it("formatMoney 与 parseToMinor 互逆（CNY）", () => {
    for (const minor of [1n, 5n, 1999n, 123456789n]) {
      expect(parseToMinor(formatMoney(minor, CNY).slice(1))).toBe(minor);
    }
  });
});
