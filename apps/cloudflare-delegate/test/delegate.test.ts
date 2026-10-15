import test from "node:test";
import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import {
  ANSWER_CAP,
  DelegateRequestRejected,
  SIGNATURE_HEADER,
  TIMESTAMP_HEADER,
  defer,
  delegate,
  ok,
  permanent,
  respond,
  signAnswer,
  signRequest,
  verifyRequest,
  type Envelope,
} from "../src/index.ts";

const KEY = "a-deployment-worker-key";
const NOW = 1_900_000_000;

const envelope: Envelope = {
  job: "stage-screen",
  execution_id: "x-7f3a",
  step: "row/p-7/delegate/1",
  input_hash: "c1".repeat(32),
  attempt: 1,
  idempotency_key: "9e2b-item-key",
  entity: { namespace: "entity", key: "e-1", fence: 4 },
  as_of: "2030-01-01T00:00:10Z",
  pins: { input: "ab".repeat(32), body_dependency_delegate: "stages" },
  tables: ["screen_result"],
  row: { entity_id: "e-1", pass_id: "p-7" },
  // The engine's instant, far from this side's clock.
  deadline: "2030-01-01T00:05:10Z",
};

/** An independent HMAC-SHA256, hex, so the package's signatures are checked against Node's. */
const hmac = (message: string | Buffer) => createHmac("sha256", KEY).update(message).digest("hex");

/** A request as the daemon sends it, signed over `call`, its timestamp and the body. */
async function signed(body: string, timestamp = NOW, key = KEY): Promise<Request> {
  const bytes = new TextEncoder().encode(body);
  return new Request("http://127.0.0.1:9005/stage/screen", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      [TIMESTAMP_HEADER]: String(timestamp),
      [SIGNATURE_HEADER]: await signRequest(key, timestamp, bytes),
    },
    body,
  });
}

/** This side's clock within the skew of the daemon's signature, past it, and at it. */
const withinSkew = () => NOW + 299;
const pastSkew = () => NOW + 301;
const atNow = () => NOW;

// spec: surface.delegate.clocks@7919274f
test("signature timestamps ride this side's system clock, not the envelope's deadline", async () => {
  // The engine's deadline is years from this clock; the signature timestamp is within the skew.
  const admitted = await verifyRequest(await signed(JSON.stringify(envelope)), KEY, withinSkew);
  assert.equal(admitted.deadline, "2030-01-01T00:05:10Z");
  assert.equal(admitted.idempotency_key, "9e2b-item-key");
  await assert.rejects(verifyRequest(await signed(JSON.stringify(envelope)), KEY, pastSkew), /past 300 s/);
  const answer = await respond(envelope, ok({ screen_result: [{ entity_id: "e-1", proceed: true }] }), KEY, atNow);
  assert.equal(answer.headers.get(TIMESTAMP_HEADER), String(NOW), "the answer carries this side's clock");
});

test("a request signs over call, its timestamp and body, as Node's HMAC computes it", async () => {
  const body = JSON.stringify(envelope);
  assert.equal(await signRequest(KEY, NOW, new TextEncoder().encode(body)), hmac(`call\n${NOW}\n${body}`));
  for (const refused of [
    signed(body, NOW, "another-key"),
    signed(body).then((r) => new Request(r, { headers: { [TIMESTAMP_HEADER]: String(NOW) } })),
  ]) {
    await assert.rejects(verifyRequest(await refused, KEY, () => NOW), (e: unknown) => e instanceof DelegateRequestRejected && e.status === 401);
  }
  await assert.rejects(verifyRequest(await signed("{\"job\":1}"), KEY, () => NOW), (e: unknown) => e instanceof DelegateRequestRejected && e.status === 422);
});

test("an answer signs over answer, the request's idempotency key, its timestamp and body", async () => {
  const answer = await respond(envelope, permanent("subject withdrawn"), KEY, () => NOW);
  const body = await answer.text();
  assert.equal(JSON.parse(body).outcome, "permanent");
  assert.equal(await signAnswer(KEY, envelope.idempotency_key, NOW, new TextEncoder().encode(body)), hmac(`answer\n${envelope.idempotency_key}\n${NOW}\n${body}`));
  assert.equal(answer.headers.get(SIGNATURE_HEADER), hmac(`answer\n${envelope.idempotency_key}\n${NOW}\n${body}`));
  assert.notEqual(answer.headers.get(SIGNATURE_HEADER), hmac(`answer\nanother-item\n${NOW}\n${body}`), "bound to its request");
});

test("the time to the deadline is measured against the envelope and clamped to a timer", async () => {
  const { timeToDeadline } = await import("../src/index.ts");
  assert.equal(timeToDeadline(envelope), 300_000, "deadline minus as_of");
  assert.equal(timeToDeadline(envelope, "2030-01-01T00:05:09Z"), 1_000);
  assert.equal(timeToDeadline(envelope, "2030-01-01T00:06:00Z"), 0, "never negative");
  assert.equal(timeToDeadline({ ...envelope, deadline: "2099-01-01T00:00:00Z" }, new Date(0)), 2 ** 31 - 1, "clamped to a timer");
  assert.equal(timeToDeadline({ ...envelope, deadline: "soon" }), 0);
});

test("respond refuses an answer outside the contract before it leaves", async () => {
  await assert.rejects(respond(envelope, ok({ other: [{ x: 1 }] }), KEY, () => NOW), /does not declare/);
  await assert.rejects(respond(envelope, defer("soon"), KEY, () => NOW), /no instant/);
  await assert.rejects(respond(envelope, permanent("x".repeat(ANSWER_CAP)), KEY, () => NOW), /cap/);
  const deferred = await respond(envelope, defer(new Date("2030-01-01T00:10:00Z"), "budget"), KEY, () => NOW);
  assert.deepEqual(await deferred.json(), { outcome: "defer", retry_at: "2030-01-01T00:10:00Z", reason: "budget" });
});

test("the fetch handler verifies, runs the handler and signs, and answers a refusal without running it", async () => {
  let ran = 0;
  const fetch = delegate<{ KEY: string }>(
    (e) => {
      ran += 1;
      return ok({ screen_result: [{ entity_id: e.row.entity_id, proceed: true }] });
    },
    { key: (env) => env.KEY, now: () => NOW },
  );
  const env = { KEY };
  const good = await fetch(await signed(JSON.stringify(envelope)), env);
  assert.equal(good.status, 200);
  assert.ok(good.headers.get(SIGNATURE_HEADER));
  assert.deepEqual(await good.json(), { outcome: "ok", rows: { screen_result: [{ entity_id: "e-1", proceed: true }] } });

  const unsigned = await fetch(new Request("http://127.0.0.1:9005/stage/screen", { method: "POST", body: JSON.stringify(envelope) }), env);
  assert.equal(unsigned.status, 401);
  const wrongMethod = await fetch(new Request("http://127.0.0.1:9005/stage/screen", { method: "GET" }), env);
  assert.equal(wrongMethod.status, 405);
  assert.equal(ran, 1, "a refused request runs no handler");

  const failing = delegate(() => { throw new Error("model down"); }, { key: KEY, now: () => NOW });
  const failed = await failing(await signed(JSON.stringify(envelope)), undefined);
  assert.equal(failed.status, 500, "a handler failure is a non-200 the engine closes Transient");
  const leaking = delegate(() => ok({ other: [] }), { key: KEY, now: () => NOW });
  const leaked = await leaking(await signed(JSON.stringify(envelope)), undefined);
  assert.equal(leaked.status, 500, "an undeclared table never leaves the delegate");
});
