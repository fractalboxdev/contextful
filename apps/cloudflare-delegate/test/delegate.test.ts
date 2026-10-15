import test from "node:test";
import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import {
  ANSWER_CAP,
  DelegateAnswerInvalid,
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
  timeToDeadline,
  transient,
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
  // The engine's instant at send plus a 300 s timeout, days after the read bound: a fire
  // under an injected clock far from this side's.
  deadline: "2030-01-03T00:05:10Z",
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

/** This side's clock within the skew of the daemon's signature, past it either way, and at it. */
const withinSkew = () => NOW + 299;
const pastSkew = () => NOW + 301;
const behindSkew = () => NOW - 301;
const atNow = () => NOW;

const isRejected = (status: 401 | 422) => (e: unknown) => e instanceof DelegateRequestRejected && e.status === status;

// spec: surface.delegate.clocks@7919274f
test("signature timestamps ride this side's system clock, not the envelope's deadline", async () => {
  // The engine's deadline is years from this clock; the signature timestamp is within the skew.
  const admitted = await verifyRequest(await signed(JSON.stringify(envelope)), KEY, withinSkew);
  assert.equal(admitted.deadline, "2030-01-03T00:05:10Z");
  assert.equal(admitted.idempotency_key, "9e2b-item-key");
  await assert.rejects(verifyRequest(await signed(JSON.stringify(envelope)), KEY, pastSkew), /past 300 s/);
  await assert.rejects(verifyRequest(await signed(JSON.stringify(envelope)), KEY, behindSkew), /past 300 s/);
  const answer = await respond(envelope, ok({ screen_result: [{ entity_id: "e-1", proceed: true }] }), KEY, atNow);
  assert.equal(answer.headers.get(TIMESTAMP_HEADER), String(NOW), "the answer carries this side's clock");
  const fractional = await respond(envelope, ok({ screen_result: [] }), KEY, () => NOW + 0.75);
  assert.equal(fractional.headers.get(TIMESTAMP_HEADER), String(NOW), "a whole Unix second, whatever the clock yields");
});

test("a request signs over call, its timestamp and body, as Node's HMAC computes it", async () => {
  const body = JSON.stringify(envelope);
  assert.equal(await signRequest(KEY, NOW, new TextEncoder().encode(body)), hmac(`call\n${NOW}\n${body}`));
  const good = await signed(body);
  const tampered = new Request(good, { body: body.replace("e-1", "e-2") });
  for (const refused of [
    signed(body, NOW, "another-key"),
    Promise.resolve(tampered),
    signed(body).then((r) => new Request(r, { headers: { [TIMESTAMP_HEADER]: String(NOW) } })),
    signed(body).then((r) => new Request(r, { headers: { [TIMESTAMP_HEADER]: String(NOW), [SIGNATURE_HEADER]: "not-hex" } })),
  ]) {
    await assert.rejects(verifyRequest(await refused, KEY, atNow), isRejected(401));
  }
  for (const malformed of ["{\"job\":1}", JSON.stringify({ ...envelope, pins: [] }), JSON.stringify({ ...envelope, row: [1, 2] }), JSON.stringify({ ...envelope, pins: { input: 7 } })]) {
    await assert.rejects(verifyRequest(await signed(malformed), KEY, atNow), isRejected(422), malformed);
  }
});

test("an answer signs over answer, the request's idempotency key, its timestamp and body", async () => {
  const answer = await respond(envelope, permanent("subject withdrawn"), KEY, atNow);
  const body = await answer.text();
  assert.equal(JSON.parse(body).outcome, "permanent");
  assert.equal(await signAnswer(KEY, envelope.idempotency_key, NOW, new TextEncoder().encode(body)), hmac(`answer\n${envelope.idempotency_key}\n${NOW}\n${body}`));
  assert.equal(answer.headers.get(SIGNATURE_HEADER), hmac(`answer\n${envelope.idempotency_key}\n${NOW}\n${body}`));
  assert.notEqual(answer.headers.get(SIGNATURE_HEADER), hmac(`answer\nanother-item\n${NOW}\n${body}`), "bound to its request");
});

test("the time to the deadline is measured from the daemon's send instant and clamped to a timer", async () => {
  assert.equal(timeToDeadline(envelope, "2030-01-03T00:00:10Z"), 300_000, "deadline minus the send instant");
  assert.equal(timeToDeadline(envelope, Date.parse("2030-01-03T00:05:09Z") / 1000), 1_000, "from Unix seconds");
  assert.equal(timeToDeadline(envelope, new Date("2030-01-03T00:06:00Z")), 0, "never negative");
  assert.equal(timeToDeadline(envelope, await signed("{}", Date.parse("2030-01-03T00:04:10Z") / 1000)), 60_000, "from the request's timestamp header");
  assert.equal(timeToDeadline(envelope, await signed("{}", 86_400)), 2 ** 31 - 1, "clamped under an engine clock far from the daemon's");
  assert.equal(timeToDeadline({ ...envelope, deadline: "soon" }, NOW), 0);
});

test("respond admits the engine's instant grammar, re-emits Z, and refuses the rest before it leaves", async () => {
  for (const [spelled, emitted] of [
    ["2030-01-01T00:10:00Z", "2030-01-01T00:10:00Z"],
    ["2030-01-01T00:10:00+00:00", "2030-01-01T00:10:00Z"],
    ["2030-01-01T02:10:00+02:00", "2030-01-01T00:10:00Z"],
    ["2030-01-01T00:10:00.250Z", "2030-01-01T00:10:00.250Z"],
  ]) {
    const deferred = await respond(envelope, defer(spelled), KEY, atNow);
    assert.equal((await deferred.json() as { retry_at: string }).retry_at, emitted, spelled);
  }
  for (const spelled of ["soon", "2030-01-01T00:10:00", "2030-01-01 00:10:00Z", "2030-01-01"]) {
    assert.throws(() => defer(spelled), DelegateAnswerInvalid, spelled);
  }
  assert.throws(() => defer(new Date(Number.NaN)), DelegateAnswerInvalid, "an invalid Date is a handler bug, not a transient failure");
  await assert.rejects(respond(envelope, ok({ other: [{ x: 1 }] }), KEY, atNow), /does not declare/);
  await assert.rejects(respond(envelope, ok({ screen_result: { entity_id: "e-1" } as never }), KEY, atNow), /plain row objects/);
  await assert.rejects(respond(envelope, ok({ screen_result: [new Map([["entity_id", "e-1"]]) as never] }), KEY, atNow), /plain row objects/);
  await assert.rejects(respond(envelope, ok({ screen_result: [{ n: 10n }] }), KEY, atNow), /does not serialize/);
  await assert.rejects(respond(envelope, { outcome: "defer", retry_at: "x".repeat(2 * 1024 * 1024) } as never, KEY, atNow), /no RFC 3339 instant/);
  await assert.rejects(respond(envelope, permanent("x".repeat(ANSWER_CAP)), KEY, atNow), /cap/);
});

test("the fetch handler verifies, runs the handler and signs, and answers a refusal without running it", async () => {
  let ran = 0;
  const fetch = delegate<{ KEY?: string }, { tag: string }>(
    async (e, _env, request, ctx) => {
      ran += 1;
      assert.equal(ctx.tag, "worker-ctx", "the handler reaches the Worker context");
      // The handler reads the request too: the body is still there.
      assert.equal((await request.json() as Envelope).idempotency_key, e.idempotency_key);
      return ok({ screen_result: [{ entity_id: e.row.entity_id, proceed: true }] });
    },
    { key: (env) => env.KEY, now: atNow },
  );
  const env = { KEY };
  const ctx = { tag: "worker-ctx" };
  const good = await fetch(await signed(JSON.stringify(envelope)), env, ctx);
  assert.equal(good.status, 200);
  assert.ok(good.headers.get(SIGNATURE_HEADER));
  assert.deepEqual(await good.json(), { outcome: "ok", rows: { screen_result: [{ entity_id: "e-1", proceed: true }] } });

  const unsigned = await fetch(new Request("http://127.0.0.1:9005/stage/screen", { method: "POST", body: JSON.stringify(envelope) }), env, ctx);
  assert.equal(unsigned.status, 401);
  const wrongMethod = await fetch(new Request("http://127.0.0.1:9005/stage/screen", { method: "GET" }), env, ctx);
  assert.equal(wrongMethod.status, 405);
  const unkeyed = await fetch(await signed(JSON.stringify(envelope)), {}, ctx);
  assert.equal(unkeyed.status, 500);
  assert.match(await unkeyed.text(), /deployment key is unset/);
  assert.equal(ran, 1, "a refused request runs no handler");

  const parking = delegate(() => transient("budget"), { key: KEY, now: atNow });
  assert.deepEqual(await (await parking(await signed(JSON.stringify(envelope)), undefined, undefined)).json(), { outcome: "transient", reason: "budget" });

  const failing = delegate(() => { throw new Error("model down"); }, { key: KEY, now: atNow });
  const failed = await failing(await signed(JSON.stringify(envelope)), undefined, undefined);
  assert.equal(failed.status, 500, "a handler failure is a non-200 the engine closes Transient");
  const nullThrower = delegate(() => { throw null; }, { key: KEY, now: atNow });
  assert.equal((await nullThrower(await signed(JSON.stringify(envelope)), undefined, undefined)).status, 500);

  // A deterministic handler bug goes back as a signed permanent, so the item ends on its first attempt.
  const leaking = delegate(() => ok({ other: [] }), { key: KEY, now: atNow });
  const leaked = await leaking(await signed(JSON.stringify(envelope)), undefined, undefined);
  assert.equal(leaked.status, 200);
  assert.ok(leaked.headers.get(SIGNATURE_HEADER));
  const verdict = await leaked.json() as { outcome: string; reason: string };
  assert.equal(verdict.outcome, "permanent");
  assert.match(verdict.reason, /^DelegateAnswerInvalid: rows for `other`/);

  // The fault's reason is bounded, so a fault built from a huge handler value still leaves signed.
  const huge = delegate(() => ({ outcome: "defer", retry_at: "x".repeat(2 * 1024 * 1024) }) as never, { key: KEY, now: atNow });
  const bounded = await huge(await signed(JSON.stringify(envelope)), undefined, undefined);
  assert.equal(bounded.status, 200);
  assert.ok((await bounded.json() as { reason: string }).reason.length <= 512);
  const badDate = delegate(() => defer(new Date(Number.NaN)), { key: KEY, now: atNow });
  assert.equal((await (await badDate(await signed(JSON.stringify(envelope)), undefined, undefined)).json() as { outcome: string }).outcome, "permanent");
});
