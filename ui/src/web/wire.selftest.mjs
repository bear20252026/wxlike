/**
 * `npm run test:wire` — the bincode codec checked against real Rust bytes.
 *
 * Every fixture below is a binary WebSocket message captured from
 * `wxlike-server` itself (`server/src/lib.rs::ws_routes` → `mq_hub` fan-out),
 * so it is `bincode::serialize` output and not something I reasoned out.
 * Three things must hold for each:
 *   1. decode produces the fields Rust put in;
 *   2. `encode(decode(bytes))` is **byte-identical** — a single byte of drift
 *      (u64 vs varint length, Option as u32 instead of u8, a missing enum
 *      index) would make the server reject or misread the frame;
 *   3. the UI-facing event mapping keeps the keys `ui/src/state.ts` expects.
 *
 * Run with: `cd ui && node src/web/wire.selftest.mjs`
 */

import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { build } from "esbuild";

const here = dirname(fileURLToPath(import.meta.url));
const uiRoot = resolve(here, "../..");

async function load(entry) {
  const built = await build({
    absWorkingDir: uiRoot,
    entryPoints: [entry],
    bundle: true,
    format: "esm",
    platform: "neutral",
    target: "es2022",
    write: false,
  });
  const src = built.outputFiles[0].text;
  return await import(`data:text/javascript;base64,${Buffer.from(src).toString("base64")}`);
}

const wire = await load("src/web/wire.ts");
// local.ts only touches indexedDB inside functions, so it loads fine in Node
const local = await load("src/web/local.ts");

const { encodeFrame, decodeFrame, envelopeToEvent, hex, unhex, makeEnvelope, kindOf } = wire;

const toHex = (bytes) => Array.from(bytes).map((b) => b.toString(16).padStart(2, "0")).join("");
const fromHex = (s) => Uint8Array.from(s.match(/../g).map((p) => Number.parseInt(p, 16)));

/** Captured from the live hub: `Msg` frames for each body variant. */
const FIXTURES = {
  text: {
    hex: "03000000022300000000000000642d633364616162626433336137623931657e64646461323939306162613034646134100000000000000063336461616262643333613762393165010000000000000010000000000000006464646132393930616261303464613401000000000000000000000000000000190000000000000068656c6c6f2062696e636f646520e4bda0e5a5bd20f09f9a8023d7e559a1010000f61ebad669958ff34aa29b40aaa370ca0000",
    user: "c3daabbd33a7b91e",
    peer: "ddda2990aba04da4",
  },
  notice: {
    hex: "03000000022300000000000000642d633364616162626433336137623931657e646464613239393061626130346461341000000000000000633364616162626433336137623931650100000000000000100000000000000064646461323939306162613034646134000000000000000002000000020000002f0000000000000068765f615f6d76677478676a6d20e8afb7e6b182e6b7bbe58aa0e4bda0e4b8bae5a5bde58f8befbc9a6e6f74696365d2d5e559a1010000d0f08c322b5117a4cf1625dcf99b6dd90000",
    user: "c3daabbd33a7b91e",
    peer: "ddda2990aba04da4",
  },
  image: {
    hex: "03000000022300000000000000642d633364616162626433336137623931657e646464613239393061626130346461341000000000000000646464613239393061626130346461340100000000000000100000000000000063336461616262643333613762393165000000000000000001000000010000004000000000000000616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161610fd9e559a1010000aa074c44e2e6419c55435d87acd67d770000",
    user: "ddda2990aba04da4",
    peer: "c3daabbd33a7b91e",
  },
  voice: {
    hex: "03000000022300000000000000642d633364616162626433336137623931657e6464646132393930616261303464613410000000000000006464646132393930616261303464613401000000000000001000000000000000633364616162626433336137623931650200000000000000020000000400000009000000000000006d73672e766f696365e7000000000000007b22617372223a6e756c6c2c226475726174696f6e5f6d73223a3930302c226d656469615f6964223a2261616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161616161222c2277617665666f726d223a5b302c312c322c332c342c352c362c372c382c392c31302c31312c31322c31332c31342c31352c31362c31372c31382c31392c32302c32312c32322c32332c32342c32352c32362c32372c32382c32392c33302c33312c33322c33332c33342c33352c33362c33372c33382c33395d7de6d9e559a1010000fd13aff010259b98fab9e3e9a9f48ad90000",
    user: "ddda2990aba04da4",
    peer: "c3daabbd33a7b91e",
  },
  receipt: {
    hex: "03000000022300000000000000642d633364616162626433336137623931657e646464613239393061626130346461341000000000000000646464613239393061626130346461340100000000000000100000000000000063336461616262643333613762393165000000000000000002000000020000000000000000000000b0dae559a1010000a390ea1c1e1fc20bd7b0a298a36dd90001f61ebad669958ff34aa29b40aaa370ca0101000000f61ebad669958ff34aa29b40aaa370ca",
    user: "ddda2990aba04da4",
    peer: "c3daabbd33a7b91e",
  },
  recall: {
    hex: "03000000022300000000000000642d633364616162626433336137623931657e64646461323939306162613034646134100000000000000064646461323939306162613034646134000000000000000000000000000000000100000003000000aa074c44e2e6419c55435d87acd67d779bdbe559a10100000a445285096ca3e22e33c2b3411572540000",
    user: "ddda2990aba04da4",
    peer: "c3daabbd33a7b91e",
  },
};

const CONV = "d-c3daabbd33a7b91e~ddda2990aba04da4";
const A = "c3daabbd33a7b91e";
const B = "ddda2990aba04da4";

let checks = 0;
const ok = (label) => {
  checks += 1;
  console.log(`  ok  ${label}`);
};

console.log("bincode fixtures captured from wxlike-server");
for (const [name, fixture] of Object.entries(FIXTURES)) {
  const bytes = fromHex(fixture.hex);
  const frame = decodeFrame(bytes);
  assert.equal(frame.t, "Msg", `${name}: wire variant`);
  const env = frame.env;
  assert.equal(env.version, 2, `${name}: protocol version`);
  assert.equal(env.conv_id, CONV, `${name}: conv_id`);
  assert.equal(env.sender, fixture.user, `${name}: sender`);
  assert.ok(env.sent_at_ms > 1_700_000_000_000, `${name}: sent_at_ms is ms`);
  // the byte-for-byte test
  assert.equal(toHex(encodeFrame(frame)), fixture.hex, `${name}: re-encode differs`);
  ok(`${name} (${env.body.t}${env.body.t === "Custom" ? `/${env.body.kind}` : ""})`);
}

console.log("\nfield-by-field, the parts the UI reads");
const text = decodeFrame(fromHex(FIXTURES.text.hex)).env;
assert.equal(text.body.t, "Text");
assert.equal(text.body.value, "hello bincode 你好 🚀");
assert.equal(text.seq, 1, "mailbox seq survives the round trip");
assert.deepEqual(text.to, [B]);
assert.equal(hex(text.msg_id), "f61ebad669958ff34aa29b40aaa370ca");
assert.equal(text.quote_of, null);
assert.equal(text.receipt, null);
ok("Text: utf-8 + emoji + seq + to[]");

const image = decodeFrame(fromHex(FIXTURES.image.hex)).env;
assert.equal(image.body.t, "Bytes");
assert.equal(image.body.value.length, 64);
assert.equal(toHex(image.body.value), "61".repeat(64));
assert.equal(image.kind, "Blob", "kind_of(Bytes) == Blob");
ok("Bytes: 64-byte media reference, kind derived");

const voice = decodeFrame(fromHex(FIXTURES.voice.hex)).env;
assert.equal(voice.body.t, "Custom");
assert.equal(voice.body.kind, "msg.voice");
assert.equal(voice.body.payload.duration_ms, 900);
assert.equal(voice.body.payload.waveform.length, 40);
assert.equal(voice.body.payload.asr, null);
ok("Custom: payload rides as JSON text (json_bytes)");

const receipt = decodeFrame(fromHex(FIXTURES.receipt.hex)).env;
assert.equal(receipt.receipt.t, "Read");
assert.equal(hex(receipt.receipt.up_to_msg_id), "f61ebad669958ff34aa29b40aaa370ca");
assert.equal(hex(receipt.quote_of), "f61ebad669958ff34aa29b40aaa370ca");
assert.equal(receipt.body.value, "", "receipt body is an empty notice");
ok("Option<[u8;16]> + Option<Receipt> as one tag byte");

const recall = decodeFrame(fromHex(FIXTURES.recall.hex)).env;
assert.equal(recall.body.t, "Recall");
assert.equal(hex(recall.body.target), "aa074c44e2e6419c55435d87acd67d77");
assert.equal(recall.to.length, 0, "recall is a conversation broadcast");
ok("Recall{target}");

console.log("\nencoder side (bytes the hub must accept)");
const built = makeEnvelope(CONV, A, [B], 7, { t: "Text", value: "abc" }, { msgId: unhex("f".repeat(32)) });
assert.equal(kindOf(built.body), "Text");
const reRead = decodeFrame(encodeFrame({ t: "Msg", env: built })).env;
assert.equal(reRead.seq, 7);
assert.equal(hex(reRead.msg_id), "f".repeat(32));
assert.equal(toHex(encodeFrame({ t: "Msg", env: reRead })), toHex(encodeFrame({ t: "Msg", env: built })));
ok("makeEnvelope → bytes → same envelope");

// 变体序号必须与 Rust 的声明顺序一致：0 Hello / 1 HelloAck / 2 HelloReject /
// 3 Msg / 4 Ping。**改了 Rust 那边的顺序，这里也要改**——bincode 只写 index。
assert.deepEqual(decodeFrame(encodeFrame({ t: "Hello", token: A })), { t: "Hello", token: A });
assert.equal(toHex(encodeFrame({ t: "Hello", token: "a" })), "00000000" + "0100000000000000" + "61");
assert.deepEqual(
  decodeFrame(encodeFrame({ t: "HelloAck", user_id: "u1", session_id: "s1" })),
  { t: "HelloAck", user_id: "u1", session_id: "s1" },
);
assert.equal(
  toHex(encodeFrame({ t: "HelloAck", user_id: "u", session_id: "s" })),
  "01000000" + "0100000000000000" + "75" + "0100000000000000" + "73",
);
assert.deepEqual(
  decodeFrame(encodeFrame({ t: "HelloReject", reason: "no" })),
  { t: "HelloReject", reason: "no" },
);
assert.equal(toHex(encodeFrame({ t: "Ping" })), "04000000");
ok("Hello / HelloAck / HelloReject / Ping variant indices");

console.log("\nevent mapping (ui/src/state.ts InboundEvent)");
const ev = envelopeToEvent(text);
assert.deepEqual(
  Object.keys(ev).sort(),
  ["conv_id", "kind", "msg_id", "quote_of", "seq", "sender", "sent_at_ms", "text"].sort(),
);
assert.equal(ev.kind, "text");
assert.equal(ev.msg_id, "f61ebad669958ff34aa29b40aaa370ca");
assert.equal(envelopeToEvent(decodeFrame(fromHex(FIXTURES.image.hex)).env).media_id, "a".repeat(64));
const noticeEvent = envelopeToEvent(decodeFrame(fromHex(FIXTURES.notice.hex)).env);
assert.equal(noticeEvent.kind, "system");
assert.match(String(noticeEvent.text), /请求添加你为好友/);
const receiptEvent = envelopeToEvent(receipt);
assert.equal(receiptEvent.kind, "receipt");
assert.equal(receiptEvent.receipt.state, "read");
assert.equal(receiptEvent.receipt.target, "f61ebad669958ff34aa29b40aaa370ca");
ok("text/image/system/receipt events carry the keys api.ts reads");

console.log("\nmalformed input is refused, never guessed");
assert.throws(() => decodeFrame(fromHex("050000000000")), /unknown WireFrame/i);
assert.throws(() => decodeFrame(fromHex("000000000500000000000000")), /truncated/i);
assert.throws(() => decodeFrame(fromHex(FIXTURES.text.hex).slice(0, 40)), /truncated/i);
assert.throws(() => decodeFrame(fromHex("03000000ff")), /version/i); // 03 = Msg，紧跟坏 version
const padded = new Uint8Array([...fromHex(FIXTURES.text.hex), 1]);
assert.throws(() => decodeFrame(padded), /trailing/i);
assert.throws(() => unhex("deadbeef"), /16 bytes/);
ok("bad variant / short frame / bad version / trailing byte / bad hex");

console.log("\noutbox JSON round trip (IndexedDB records resend the same envelope)");
const bodies = [
  { t: "Text", value: "你好" },
  { t: "Notice", value: "" },
  // a media reference is 64 hex chars of arbitrary bytes — not a 16-byte MsgId.
  // Sending it through the strict id decoder is what used to poison the queue.
  { t: "Bytes", value: new TextEncoder().encode("365d0c68b4b24629de07c8d9d5814714539f1530188d045ee1d8f0f591c4ce11") },
  { t: "Recall", target: unhex("01".repeat(16)) },
  { t: "Custom", kind: "msg.voice", payload: { media_id: "a".repeat(64), duration_ms: 900, waveform: [1, 2], asr: null } },
];
for (const body of bodies) {
  const env = makeEnvelope(CONV, A, [B], 3, body);
  const back = local.envelopeFromJson(local.envelopeToJson(env));
  assert.equal(toHex(encodeFrame({ t: "Msg", env: back })), toHex(encodeFrame({ t: "Msg", env })), `${body.t} 变了`);
  ok(`outbox JSON keeps ${body.t}`);
}
const withReceipt = makeEnvelope(CONV, A, [], 0, { t: "Notice", value: "" }, {
  quoteOf: unhex("f".repeat(32)),
  receipt: { t: "Read", up_to_msg_id: unhex("e".repeat(32)) },
});
const backReceipt = local.envelopeFromJson(local.envelopeToJson(withReceipt));
assert.equal(hex(backReceipt.quote_of), "f".repeat(32));
assert.equal(backReceipt.receipt.t, "Read");
assert.equal(hex(backReceipt.receipt.up_to_msg_id), "e".repeat(32));
ok("outbox JSON keeps quote_of + receipt.Read");

console.log(`\nPASS — ${checks} fixture groups, 6 wire variants`);
