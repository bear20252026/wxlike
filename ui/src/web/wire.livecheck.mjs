/**
 * `node src/web/wire.livecheck.mjs` — the codec against a running server,
 * in both directions, with the A3 latency number.
 *
 * The selftest proves the byte layout with captured fixtures; this proves the
 * live hub still accepts what we emit and that a browser-shaped client sees a
 * message promptly. It needs a server: `WXLIKE_URL=http://127.0.0.1:8099`.
 * Without it the run says so and exits non-zero — never a silent pass.
 */

import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const base = process.env.WXLIKE_URL;
if (!base) {
  console.error("需要 WXLIKE_URL（例：WXLIKE_URL=http://127.0.0.1:8099 node src/web/wire.livecheck.mjs）");
  console.error("起服务：WXLIKE_DEV_INSECURE=1 cargo run -p wxlike-server");
  process.exit(2);
}
const wsUrl = base.replace(/^http/, "ws").replace(/\/+$/, "") + "/ws";

const here = dirname(fileURLToPath(import.meta.url));
const bundled = await build({
  absWorkingDir: resolve(here, "../.."),
  entryPoints: ["src/web/wire.ts"],
  bundle: true,
  format: "esm",
  platform: "neutral",
  target: "es2022",
  write: false,
});
const wire = await import(
  `data:text/javascript;base64,${Buffer.from(bundled.outputFiles[0].text).toString("base64")}`
);
const { encodeFrame, decodeFrame, envelopeToEvent, hex, makeEnvelope } = wire;

async function api(method, path, body, token) {
  const res = await fetch(base + path, {
    method,
    headers: {
      ...(body ? { "content-type": "application/json" } : {}),
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path} → ${res.status}: ${text.slice(0, 160)}`);
  return text ? JSON.parse(text) : null;
}

function connect(user, inbox) {
  return new Promise((resolveOk, reject) => {
    const ws = new WebSocket(wsUrl);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      ws.send(encodeFrame({ t: "Hello", user_id: user }));
      resolveOk({ ws });
    };
    ws.onmessage = (ev) => {
      const receivedAt = performance.now();
      const frame = decodeFrame(new Uint8Array(ev.data));
      if (frame.t === "Msg") inbox.push({ receivedAt, env: frame.env, event: envelopeToEvent(frame.env) });
    };
    ws.onerror = () => reject(new Error(`WebSocket 握手失败 (${wsUrl})`));
  });
}

const stamp = Date.now().toString(36);
const a = await api("POST", "/api/register", { name: `lc_a_${stamp}`, password: "passw0rd!Aa" });
const b = await api("POST", "/api/register", { name: `lc_b_${stamp}`, password: "passw0rd!Bb" });
const ids = [a.user_id, b.user_id].sort();
const conv = `d-${ids[0]}~${ids[1]}`;

// 直聊受投递策略约束：非好友的消息会被 hub 拒收，先建立关系
const req = await api("POST", "/api/friends/requests", { to_user_id: b.user_id, message: "live" }, a.token);
await api("POST", `/api/friends/requests/${req.id}/accept`, undefined, b.token);

const bIn = [];
const aIn = [];
const bb = await connect(b.user_id, bIn);
const aa = await connect(a.user_id, aIn);

console.log(`服务端 ${base} · hub ${wsUrl}`);
console.log(`conv ${conv}`);

// 1. HTTP 文本 → 对端 WS。A3 的验收是"另一客户端发消息，200ms 内出现"：
//    取三条的最小值，避免本机正在跑 cargo 时把抖动当成协议问题。
const latencies = [];
let firstReply = null;
for (let i = 0; i < 3; i++) {
  const text = `live check ${i} 🚀`;
  const sentAt = performance.now();
  const reply = await api("POST", `/api/conv/${conv}/send`, { text, idempotency_key: `lc-${i}` }, a.token);
  if (!firstReply) firstReply = reply;
  // 扇出发生在 submit 里，可能早于 HTTP 响应返回 —— 按正文等，不按数组长度
  const deadline = performance.now() + 2_000;
  let hit;
  while (!(hit = bIn.find((m) => m.env.body.value === text)) && performance.now() < deadline) {
    await new Promise((r) => setTimeout(r, 5));
  }
  assert.ok(hit, `第 ${i} 条没有收到`);
  assert.equal(hex(hit.env.msg_id), reply.msg_id, "msg_id 一致");
  latencies.push(Math.round(hit.receivedAt - sentAt));
}
assert.ok(
  Math.min(...latencies) < 200,
  `A3 上限 200ms，实测最小 ${Math.min(...latencies)}ms（样本 ${latencies.join("/")}ms）`,
);
console.log(
  `  ok  HTTP send → WS 收到：最小 ${Math.min(...latencies)}ms，样本 ${latencies.join("/")}ms（A3 上限 200ms）`,
);
const reply = firstReply;
const textHit = bIn.find((m) => m.env.body.value === "live check 0 🚀");
assert.equal(textHit.event.kind, "text");
assert.equal(textHit.event.text, "live check 0 🚀");

// 2. 幂等：同一个 key 重发不落两条
await api("POST", `/api/conv/${conv}/send`, { text: "live check 0 🚀", idempotency_key: "lc-0" }, a.token);
await new Promise((r) => setTimeout(r, 250));
assert.equal(bIn.filter((m) => m.env.body.value === "live check 0 🚀").length, 1, "幂等键没生效");
console.log("  ok  同一 idempotency_key 只有一条");

// 3. 客户端构造的 Custom 帧（HTTP 发不了的那些类型）服务端要能解
const custom = makeEnvelope(conv, b.user_id, [a.user_id], 0, {
  t: "Custom",
  kind: "msg.voice",
  payload: { media_id: "b".repeat(64), duration_ms: 800, waveform: Array.from({ length: 40 }, () => 50), asr: null },
});
bb.ws.send(encodeFrame({ t: "Msg", env: custom }));
await new Promise((r) => setTimeout(r, 300));
const voice = aIn.find((m) => m.env.body.t === "Custom" && m.env.body.kind === "msg.voice");
assert.ok(voice, "hub 拒绝了我方编码的 Custom 帧");
assert.equal(voice.env.sender, b.user_id);
assert.equal(voice.env.body.payload.duration_ms, 800);
assert.equal(voice.event.kind, "msg.voice");
console.log(`  ok  自造 Custom(msg.voice) 帧被 Rust 侧解出并扇出，seq=${voice.env.seq}`);

// 4. 图片引用（MsgBody::Bytes）+ 撤回
const img = makeEnvelope(conv, b.user_id, [a.user_id], 0, { t: "Bytes", value: new TextEncoder().encode("c".repeat(64)) });
bb.ws.send(encodeFrame({ t: "Msg", env: img }));
await new Promise((r) => setTimeout(r, 300));
assert.ok(aIn.some((m) => m.env.body.t === "Bytes"), "Bytes 帧没到");
await api("POST", `/api/conv/${conv}/recall`, { msg_id: hex(img.msg_id) }, b.token);
await new Promise((r) => setTimeout(r, 300));
const recalled = aIn.find((m) => m.env.body.t === "Recall");
assert.ok(recalled, "撤回帧没到");
assert.equal(hex(recalled.env.body.target), hex(img.msg_id), "撤回目标不对");
assert.equal(envelopeToEvent(recalled.env).kind, "recall");
console.log("  ok  Bytes 帧 + 撤回链路");

// 5. 已读回执：receipt 只走 WS，HTTP 里没有这个字段
const readEnv = makeEnvelope(conv, a.user_id, [], 0, { t: "Notice", value: "" }, {
  quoteOf: custom.msg_id,
  receipt: { t: "Read", up_to_msg_id: custom.msg_id },
});
aa.ws.send(encodeFrame({ t: "Msg", env: readEnv }));
await new Promise((r) => setTimeout(r, 300));
const receipt = bIn.find((m) => m.env.receipt);
assert.ok(receipt, "回执帧被丢弃");
assert.equal(receipt.event.kind, "receipt");
assert.equal(receipt.event.receipt.state, "read");
assert.equal(receipt.event.receipt.target, hex(custom.msg_id));
console.log("  ok  已读回执（Notice + quote_of + receipt.Read）");

// 6. 我方经 WS 发出的消息真的落库（历史以 msg_id 为证；哪些类型会被服务端
//    持久化/遮蔽是 Rust 的决定，这里只断言"我发的东西进去了"）
const hist = await api("GET", `/api/history/${conv}?limit=50`, undefined, a.token);
assert.ok(Array.isArray(hist.messages), "history.messages 必须是数组");
assert.ok("next_before_seq" in hist, "api.ts 的翻页游标字段缺失");
const byId = new Map(hist.messages.map((m) => [m.msg_id, m]));
assert.ok(byId.has(hex(custom.msg_id)), "WS 发出的语音消息不在历史里");
assert.equal(byId.get(hex(custom.msg_id)).kind, "msg.voice");
assert.ok(byId.has(reply.msg_id), "HTTP 发的文本不在历史里");
assert.equal(byId.get(reply.msg_id).seq, 1, "信箱 seq 从 1 开始（与未读同一套编号）");
// 撤回行的形态由服务端决定：要么整行撤下，要么 withdrawn=true（客户端两条路都能渲染）
const recalledRow = byId.get(hex(img.msg_id));
if (recalledRow) assert.equal(recalledRow.withdrawn, true, "撤回行没打 withdrawn 标记");
console.log(
  `  ok  历史页 ${hist.messages.length} 条：${hist.messages.map((m) => `${m.seq}:${m.kind}${m.withdrawn ? "(已撤回)" : ""}`).join(" ")}${recalledRow ? "" : "（撤回行已从信箱页撤下）"}`,
);

// 7. 搜索命中中文与 emoji（索引写的是 preview_text）
const found = await api("GET", `/api/search/messages?q=${encodeURIComponent("live check 0 🚀")}`, undefined, a.token);
assert.ok(found.results.length >= 1, "搜索没命中");
console.log(`  ok  搜索命中 ${found.results.length} 条`);

aa.ws.close();
bb.ws.close();
console.log("\nPASS — live check");
process.exit(0);
