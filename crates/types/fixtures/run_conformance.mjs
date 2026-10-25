// 契约 fixture 的 TS/JS 端一致性检查。
//
//   node crates/types/fixtures/run_conformance.mjs
//
// 规则与 Rust 端（tests/fixture_conformance.rs）完全一致：解析 `value`，
// 按"对象键升序、无空白、数字一律字符串"输出规范化 JSON，与文件里的
// `canonical` 逐字节比对。
//
// 为什么要专门在 JS 里跑一遍：JS 的 number 超过 2^53 会静默丢精度，
// 这是三端里唯一"看起来成功、其实错了"的失败模式。所以除了文本比对，
// 这里还显式断言大额金额/序列号**必须**走 BigInt 才能精确。

import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const fixtureDir = join(here, "contract-v1");

function canonical(value) {
  if (Array.isArray(value)) {
    return "[" + value.map(canonical).join(",") + "]";
  }
  if (value !== null && typeof value === "object") {
    const keys = Object.keys(value).sort();
    return (
      "{" +
      keys.map((key) => JSON.stringify(key) + ":" + canonical(value[key])).join(",") +
      "}"
    );
  }
  return JSON.stringify(value);
}

let checked = 0;
const failures = [];

for (const name of readdirSync(fixtureDir).sort()) {
  if (!name.endsWith(".json")) continue;
  const doc = JSON.parse(readFileSync(join(fixtureDir, name), "utf8"));
  const got = canonical(doc.value);
  if (got !== doc.canonical) {
    failures.push(`${name}: 规范化不一致\n  期望 ${doc.canonical}\n  实际 ${got}`);
    continue;
  }
  checked += 1;
}

// 大数专项：必须能用 BigInt 精确表示
const money = JSON.parse(readFileSync(join(fixtureDir, "money.big.json"), "utf8"));
const exact = BigInt(money.value.minor);
if (exact !== 9007199254740993n) {
  failures.push(`money.big: BigInt 解析丢了精度（${exact}）`);
}
// 走一遍 number：必须丢精度，否则"为什么用字符串"这个前提就不成立了
if (BigInt(Number(money.value.minor)) === exact) {
  failures.push("money.big: number 竟然能精确表示 2^53+1，测试前提不成立");
}

if (failures.length > 0) {
  console.error(failures.join("\n"));
  process.exit(1);
}
console.log(`ok: ${checked} 个 fixture 在 JS 端规范化结果与 Rust 端一致`);
