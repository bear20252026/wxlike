/**
 * Money formatting over integer minor units — never floats.
 *
 * The server stores amounts as `i64` minor units (分) and sends them as JSON
 * numbers. Formatting here is pure string manipulation on the integer value;
 * `parseFloat`/`toFixed` are banned in this module (G11-04 requirement 1: 金额
 * 精确格式化不做浮点财务运算). Parsing user input goes through
 * `parseToMinor`, which rejects anything that is not a plain decimal with at
 * most the currency's scale digits.
 */

export interface CurrencySpec {
  /** ISO-like code used by the ledger ("CNY" for the virtual wallet). */
  code: string;
  /** 10^scale minor units make one major unit. */
  scale: number;
  symbol: string;
}

export const CNY: CurrencySpec = { code: "CNY", scale: 2, symbol: "¥" };

/** Fixed-width string of the integer, no `Number`→`Number` rounding anywhere. */
function split(minor: bigint | number | string, scale: number): { whole: string; frac: string } {
  const text = typeof minor === "bigint" ? minor.toString(10) : String(minor);
  if (!/^-?\d+$/.test(text)) throw new Error(`金额不是整数最小单位: ${text}`);
  const negative = text.startsWith("-");
  const digits = negative ? text.slice(1) : text;
  if (scale === 0) {
    return { whole: negative ? `-${digits}` : digits, frac: "" };
  }
  const padded = digits.padStart(scale + 1, "0");
  const cut = padded.length - scale;
  const whole = padded.slice(0, cut);
  const frac = padded.slice(cut);
  return { whole: negative ? `-${whole}` : whole, frac };
}

/** `12345` → `"¥123.45"`; `-500` → `"¥-5.00"`. */
export function formatMoney(minor: bigint | number | string, spec: CurrencySpec = CNY): string {
  const { whole, frac } = split(minor, spec.scale);
  return `${spec.symbol}${whole}${spec.scale ? `.${frac}` : ""}`;
}

/** `"123.4"` → `12340`; `"0.005"` → rejected (beyond scale). */
export function parseToMinor(input: string, spec: CurrencySpec = CNY): bigint {
  const text = input.trim();
  if (!text) throw new Error("请输入金额");
  const match = /^(\d+)(?:\.(\d*))?$/.exec(text);
  if (!match) throw new Error("金额格式不正确");
  const [, wholeRaw = "", fracRaw = ""] = match;
  if (fracRaw.length > spec.scale) throw new Error(`金额最多 ${spec.scale} 位小数`);
  const frac = (fracRaw + "0".repeat(spec.scale)).slice(0, spec.scale);
  const minor = BigInt(wholeRaw) * BigInt(10 ** spec.scale) + BigInt(frac || "0");
  if (minor <= 0n) throw new Error("金额必须大于零");
  return minor;
}

/** True when the amount can round-trip through JSON without losing precision. */
export function isJsonSafe(minor: bigint): boolean {
  return minor >= -(2n ** 53n) && minor <= 2n ** 53n;
}
