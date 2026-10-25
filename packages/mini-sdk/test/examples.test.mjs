// Examples run against the real SDK client; the transport here is the host
// boundary the shell provides in production. Enforcement itself (allowlist,
// grants, replay) is broker behavior proven in Rust (crates/container/tests).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

import { createClient, METHODS } from "../src/index.js";

const examplesDir = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "examples",
);

let sequence = 0;

function ok(result) {
  return { ok: true, result };
}

function err(code) {
  return { ok: false, error: { code, message: code } };
}

function makeTransport({ deny = [], allowHosts = ["api.wxlike.app"] } = {}) {
  const storage = new Map();
  const calls = [];
  const orders = new Map();
  const transport = async (request) => {
    const { action, params } = request;
    calls.push({ action, params, request_id: request.request_id });
    if (deny.includes("storage") && action.startsWith("storage.")) return err("denied");
    if (action === METHODS.storageGet) return ok(storage.get(params.key) ?? null);
    if (action === METHODS.storageSet) {
      storage.set(params.key, params.value);
      return ok(true);
    }
    if (action === METHODS.storageRemove) {
      storage.delete(params.key);
      return ok(true);
    }
    if (action === METHODS.networkRequest) {
      const host = new URL(params.url).host;
      if (!allowHosts.includes(host)) return err("not_granted");
      return ok({ status: 200, body: { menu: ["coffee"] } });
    }
    if (action === METHODS.payRequest) {
      if (deny.includes("payment")) return err("denied");
      // Order ids are deduplicated by the payment side (G06); the example
      // must survive submitting the same order twice.
      if (orders.has(params.order_id)) return ok(orders.get(params.order_id));
      const receipt = { reference: `sandbox-${orders.size + 1}`, status: "sandbox" };
      orders.set(params.order_id, receipt);
      return ok(receipt);
    }
    if (action === METHODS.locationGet) {
      if (deny.includes("location")) return err("cancelled");
      return ok({ latitude: 30.2741, longitude: 120.1551 });
    }
    return err("invalid_call");
  };
  return { transport, storage, calls, orders };
}

function loadExample(name, transport) {
  const code = readFileSync(path.join(examplesDir, name, "index.js"), "utf8");
  const context = vm.createContext({
    console,
    JSON,
    Number,
    String,
    Promise,
    Object,
    Array,
    Math,
    setTimeout,
  });
  context.__wxMini = createClient(transport, {
    requestId: () => `req-${(sequence += 1)}`,
  });
  vm.runInContext(code, context, { filename: `${name}/index.js` });
  return context;
}

async function boot(name, transport) {
  const context = loadExample(name, transport.transport);
  context.onLoad();
  const readyKey = `__${name}Ready`;
  assert.ok(context[readyKey], `${name} must expose its boot promise`);
  return context[readyKey];
}

test("notes persists its list and a repeated boot stays idempotent", async () => {
  const harness = makeTransport();
  const first = await boot("notes", harness);
  assert.equal(first.error, null);
  assert.equal(first.saved, true);
  assert.equal(first.items.length, 1);
  assert.equal(harness.storage.get("notes.v1"), JSON.stringify(first.items));

  const second = await boot("notes", harness);
  assert.equal(second.items.length, 1, "seeding must not duplicate on relaunch");
  assert.deepEqual(
    harness.storage.get("notes.v1"),
    JSON.stringify(second.items),
  );
});

test("notes keeps working when storage is denied", async () => {
  const harness = makeTransport({ deny: ["storage"] });
  const state = await boot("notes", harness);
  assert.equal(state.error, "denied");
  assert.equal(state.saved, false);
});

test("shop pays through the sandbox receipt and repeats stay idempotent", async () => {
  const harness = makeTransport();
  const first = await boot("shop", harness);
  assert.equal(first.error, null);
  assert.equal(first.sandbox, true);
  assert.equal(first.receipt.status, "sandbox");
  assert.match(first.receipt.reference, /^sandbox-/);
  assert.equal(first.menu.status, 200);

  const actions = harness.calls.map((call) => call.action);
  assert.ok(actions.indexOf(METHODS.networkRequest) < actions.indexOf(METHODS.payRequest));

  const second = await boot("shop", harness);
  assert.equal(second.receipt.reference, first.receipt.reference);
  assert.equal(harness.orders.size, 1, "same order id must not create a second charge");
});

test("shop stops before payment when the network host is not allowed", async () => {
  const harness = makeTransport({ allowHosts: [] });
  const state = await boot("shop", harness);
  assert.equal(state.error, "not_granted");
  assert.equal(state.receipt, null);
  assert.equal(harness.orders.size, 0);
});

test("shop keeps the cart when payment is denied", async () => {
  const harness = makeTransport({ deny: ["payment"] });
  const state = await boot("shop", harness);
  assert.equal(state.error, "denied");
  assert.ok(harness.storage.get("shop.cart.v1"));
  assert.equal(state.receipt, null);
});

test("game plays deterministically and keeps the high score across boots", async () => {
  const harness = makeTransport();
  const first = await boot("game", harness);
  assert.equal(first.played, true);
  const afterFirst = Number(harness.storage.get("game.score.v1"));

  const second = await boot("game", harness);
  assert.equal(second.result.guess, (afterFirst % 6) + 1);
  assert.equal(
    Number(harness.storage.get("game.score.v1")),
    second.result.best,
  );
});

test("weather rounds coordinates and falls back when location is cancelled", async () => {
  const harness = makeTransport();
  const state = await boot("weather", harness);
  assert.equal(state.error, null);
  assert.equal(state.coords.latitude, 30.27);
  assert.equal(state.coords.longitude, 120.16);
  assert.equal(harness.storage.get("weather.city.v1"), "杭州");

  const denied = await boot("weather", makeTransport({ deny: ["location"] }));
  assert.equal(denied.error, "cancelled");
  assert.equal(denied.fallback, "manual-city");
  assert.equal(denied.coords, null);
});
