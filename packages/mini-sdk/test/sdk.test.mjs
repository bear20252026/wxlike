import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  CONTRACT_VERSION,
  ERROR_CODES,
  METHODS,
  MiniHostError,
  SCOPES,
  createClient,
  isMiniHostError,
} from "../src/index.js";

const contractUrl = new URL(
  "../../../contracts/domains/miniapps/mini-host.v1.json",
  import.meta.url,
);

function recorder(result = true) {
  const calls = [];
  const transport = async (request) => {
    calls.push(request);
    return { ok: true, result };
  };
  return { calls, transport };
}

function sequenceIds() {
  let next = 0;
  return () => `req-${++next}`;
}

test("generated tables match the contract", async () => {
  const contract = JSON.parse(await readFile(contractUrl, "utf8"));
  const titles = contract.oneOf.map((branch) => branch.title);
  const scopes = Object.fromEntries(
    contract.oneOf.map((branch) => [branch.title, branch["x-scope"]]),
  );

  assert.equal(CONTRACT_VERSION, contract["x-contract-version"]);
  assert.deepEqual(Object.values(METHODS).sort(), [...titles].sort());
  assert.deepEqual({ ...SCOPES }, scopes);
  assert.deepEqual([...ERROR_CODES], contract["x-errors"]);
});

test("every typed helper emits its contract action and params only", async () => {
  const { calls, transport } = recorder();
  const client = createClient(transport, { requestId: sequenceIds() });

  await client.storage.set("draft", "hello");
  await client.storage.get("draft");
  await client.storage.remove("draft");
  await client.network.request("https://api.wxlike.app/v1/quotes");
  await client.network.request("https://api.wxlike.app/v1/orders", {
    method: "POST",
    body: "{}",
  });
  await client.camera.capture();
  await client.location.get();
  await client.file.pick("attachment");
  await client.payments.pay({ amount: "12.50", currency: "CNY", orderId: "order-1" });

  assert.deepEqual(
    calls.map((call) => call.action),
    [
      "storage.set",
      "storage.get",
      "storage.remove",
      "network.request",
      "network.request",
      "camera.capture",
      "location.get",
      "file.pick",
      "pay.request",
    ],
  );
  assert.deepEqual(calls[0].params, { key: "draft", value: "hello" });
  assert.deepEqual(calls[3].params, {
    url: "https://api.wxlike.app/v1/quotes",
    method: "GET",
  });
  assert.deepEqual(calls[8].params, {
    amount: "12.50",
    currency: "CNY",
    order_id: "order-1",
  });
  assert.deepEqual(
    calls.map((call) => call.request_id),
    calls.map((_, index) => `req-${index + 1}`),
  );
  for (const call of calls) {
    assert.deepEqual(Object.keys(call).sort(), ["action", "params", "request_id"]);
    assert.equal("app" in call, false, "guest never supplies its app id");
    assert.equal("origin" in call, false, "guest never supplies its origin");
  }
});

test("host denials surface as MiniHostError with the host code", async () => {
  const transport = async () => ({
    ok: false,
    error: { code: "denied", message: "user denied Camera" },
  });
  const client = createClient(transport, { requestId: () => "req-1" });

  await assert.rejects(client.camera.capture(), (error) => {
    assert.ok(isMiniHostError(error));
    assert.equal(error.code, "denied");
    assert.equal(error.method, "camera.capture");
    assert.equal(error.requestId, "req-1");
    assert.equal(error.message, "user denied Camera");
    return true;
  });
});

test("an unknown host error code collapses to host", async () => {
  const transport = async () => ({
    ok: false,
    error: { code: "made_up", message: "?" },
  });
  const client = createClient(transport, { requestId: () => "req-1" });
  await assert.rejects(client.location.get(), { code: "host" });
});

test("an invalid request id is rejected before the transport runs", async () => {
  let called = 0;
  const client = createClient(
    async () => {
      called += 1;
      return { ok: true, result: null };
    },
    { requestId: () => "not a valid id!" },
  );

  await assert.rejects(client.storage.get("k"), (error) => {
    assert.ok(error instanceof MiniHostError);
    assert.equal(error.code, "invalid_call");
    return true;
  });
  assert.equal(called, 0);
});

test("transport faults and malformed envelopes become host errors", async () => {
  const throwing = createClient(
    async () => {
      throw new Error("iframe went away");
    },
    { requestId: () => "req-1" },
  );
  await assert.rejects(throwing.storage.get("k"), {
    code: "host",
    message: "iframe went away",
  });

  const malformed = createClient(async () => ({ nope: true }), {
    requestId: () => "req-2",
  });
  await assert.rejects(malformed.storage.get("k"), { code: "host" });
});

test("there is no generic invoke on the guest surface", () => {
  const client = createClient(async () => ({ ok: true, result: null }));
  assert.equal(client.request, undefined);
  assert.equal(client.invoke, undefined);
  assert.ok(Object.isFrozen(client));
  assert.ok(Object.isFrozen(client.storage));
  assert.ok(Object.isFrozen(client.payments));
});
