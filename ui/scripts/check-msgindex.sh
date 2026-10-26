#!/usr/bin/env bash
# 验证 MessageIndex（tweb SlicedArray 适配层）的合并语义。
#
# 前端没有测试框架（只有 tsc + esbuild），所以这里用 esbuild 打包成 ESM、
# 再用 node 断言：乱序页、历史页、实时消息、幂等、窗口取尾。
# 任何一条失败即非零退出，可直接接进 CI。
set -euo pipefail
cd "$(dirname "$0")/.."

out=".tmp-msgindex.mjs"
test_file=".tmp-msgindex-test.mjs"
cleanup() { rm -f "$out" "$test_file"; }
trap cleanup EXIT

npx esbuild src/msgIndex.ts --bundle --format=esm --outfile="$out" --log-level=error

cat > "$test_file" <<'EOF'
import { MessageIndex } from "./.tmp-msgindex.mjs";

const mk = (seq, text) => ({ seq, sender: "a", kind: "text", text: text ?? String(seq) });
const seqs = (a) => a.map((m) => m.seq);
const eq = (got, want, label) => {
  const g = JSON.stringify(got);
  const w = JSON.stringify(want);
  if (g !== w) {
    console.error(`FAIL ${label}: got ${g} want ${w}`);
    process.exit(1);
  }
  console.log(`ok  ${label}: ${g}`);
};

// 1) 空索引 + 乱序一页
let idx = MessageIndex.from([]);
let out = idx.merge([mk(3), mk(1), mk(2)]);
eq(seqs(out), [1, 2, 3], "乱序一页归并");

// 2) 历史页（更旧的 seq）+ 同 seq 更新版本
out = idx.merge([mk(-2), mk(0), mk(2, "v2")]);
eq(seqs(out), [-2, 0, 1, 2, 3], "历史页插入更旧区间");
eq(out.find((m) => m.seq === 2).text, "v2", "同 seq 以新版本为准");
eq(out.length, 5, "无重复");

// 3) 实时消息（更大的 seq）
out = idx.merge([mk(10), mk(4)]);
eq(seqs(out), [-2, 0, 1, 2, 3, 4, 10], "实时消息插到最新端");

// 4) 重复合并同一页（幂等）
out = idx.merge([mk(10), mk(4)]);
eq(seqs(out), [-2, 0, 1, 2, 3, 4, 10], "重复页幂等");

// 5) 窗口取尾：最近 n 条，最新在前
eq(idx.tailSeqs(3), [10, 4, 3], "tailSeqs 最近 3 条");

// 6) P1 回归：局部变异（已读回执 read:true / 撤回 → system）替换了数组里的
//    对象但不经过索引；不 refresh 的话，下一次 merge 的 toArray() 会把变异
//    前的旧对象拼回去（已读勾被"复活"成未读）。
idx = MessageIndex.from([mk(1), mk(2), mk(3)]);
const mutated = idx.toArray().map((m) => (m.seq === 2 ? { ...m, read: true } : m));
idx.refresh(mutated);
out = idx.merge([mk(4)]);
eq(seqs(out), [1, 2, 3, 4], "变异后合并顺序不变");
eq(out.find((m) => m.seq === 2).read, true, "refresh 后变异对象存活");
// 对照：不给 refresh 喂变异数组，索引里就还是旧对象（这正是修复点）
idx = MessageIndex.from([mk(1), mk(2), mk(3)]);
const drifted = idx.toArray().map((m) => (m.seq === 2 ? { ...m, read: true } : m));
idx.merge([mk(4)]); // 索引视角里没人通知它 drift
eq(idx.toArray().find((m) => m.seq === 2).read, undefined, "不 refresh 时旧对象仍在（漂移语义）");
idx.refresh(drifted);
eq(idx.toArray().find((m) => m.seq === 2).read, true, "事后 refresh 也能修正");

console.log("ALL PASS");
EOF

node "$test_file"
