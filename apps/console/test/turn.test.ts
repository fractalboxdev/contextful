import assert from "node:assert/strict";
import { test } from "node:test";
import {
  ConsoleError,
  OverlayCache,
  StreamingRedactor,
  createTurn,
  parseVantage,
  validatePack,
} from "../src/turn.ts";

const readTool = { name: "query", pack: "data", kind: "read" as const };

function harness(overrides: Record<string, unknown> = {}) {
  const calls: string[] = [];
  const turn = createTurn({
    tools: [readTool],
    planner: async () => [{ tool: "query", arguments: { question: "count" } }],
    transport: {
      call: async (call: { tool: string }) => {
        calls.push(call.tool);
        return { rows: [{ count: 3 }], sources: [{ id: "row-1", label: "Count", url: "https://example.test/1" }] };
      },
    },
    synthesize: async function* () { yield "Three rows."; },
    ...overrides,
  });
  return { turn, calls };
}

test("a grounded turn cites only governed tool results", async () => {
  const { turn, calls } = harness();
  const answer = await turn.ask({ question: "How many?", packs: ["data"] });
  assert.deepEqual(calls, ["query"]);
  assert.match(answer.text, /Three rows\./);
  assert.match(answer.text, /Sources\n- Count/);
  assert.deepEqual(answer.sources.map((source: { id: string }) => source.id), ["row-1"]);
});

test("a turn with no tool result refuses model prose", async () => {
  const { turn } = harness({
    transport: { call: async () => ({ rows: [], sources: [] }) },
  });
  await assert.rejects(turn.ask({ question: "Guess", packs: ["data"] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleUngroundedAnswer");
});

test("unadmitted and mutating calls dispatch nothing", async () => {
  const { turn, calls } = harness({
    planner: async () => [{ tool: "query", arguments: {} }],
  });
  await assert.rejects(turn.ask({ question: "Read", packs: [] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleToolNotAdmitted");
  assert.deepEqual(calls, []);
  const write = harness({ tools: [{ name: "erase", pack: "data", kind: "write" }], planner: async () => [{ tool: "erase", arguments: {} }] });
  await assert.rejects(write.turn.ask({ question: "Erase", packs: ["data"] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleMutatingToolRequested");
  assert.deepEqual(write.calls, []);
});

test("organization packs refuse writes at startup", () => {
  assert.throws(() => validatePack({ name: "shared", face: "organization", tools: [{ name: "erase", kind: "write" }] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleWriteToolOnOrgFace");
});

test("direct file table functions refuse before dispatch", async () => {
  const { turn, calls } = harness({ tools: [{ name: "query", pack: "data", kind: "read", access: "direct-file" }] });
  await assert.rejects(turn.ask({ question: "Read", packs: ["data"] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleFileAccessDirect");
  assert.deepEqual(calls, []);
});

test("source list caps at eight and excludes synthetic citations", async () => {
  const { turn } = harness({
    transport: { call: async () => ({ rows: [{ value: 1 }], sources: Array.from({ length: 10 }, (_, i) => ({ id: `s${i}`, label: `Source ${i}` })) }) },
    synthesize: async function* () { yield "Answer [invented]."; },
  });
  const answer = await turn.ask({ question: "List", packs: ["data"] });
  assert.equal(answer.sources.length, 8);
  assert.doesNotMatch(answer.text, /invented/);
});

test("a failed first round replans once and the overlay reaches synthesis alone", async () => {
  const plannerInputs: unknown[] = [];
  const synthesisInputs: unknown[] = [];
  let reads = 0;
  const { turn } = harness({
    planner: async (input: unknown) => { plannerInputs.push(input); return [{ tool: "query", arguments: {} }]; },
    transport: { call: async () => (++reads === 1 ? { rows: [], sources: [] } : { rows: [{ value: 1 }], sources: [{ id: "one", label: "One" }] }) },
    overlay: async () => "Operator voice",
    synthesize: async function* (input: unknown) { synthesisInputs.push(input); yield "One."; },
  });
  await turn.ask({ question: "Find", packs: ["data"] });
  assert.equal(plannerInputs.length, 2);
  assert.equal(reads, 2);
  assert.doesNotMatch(JSON.stringify(plannerInputs), /Operator voice/);
  assert.match(JSON.stringify(synthesisInputs), /Operator voice/);
});

test("vantage accepts calendar days and instants, and rejects invalid dates", () => {
  assert.equal(parseVantage("2026-10-06"), "2026-10-06");
  assert.equal(parseVantage("2026-10-06T12:30:00Z"), "2026-10-06T12:30:00Z");
  assert.throws(() => parseVantage("2026-02-30"),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleVantageUnparseable" && error.status === 400);
});

test("overlay cache includes misses, expires after five minutes and truncates at 8000 chars", async () => {
  let now = 0;
  let reads = 0;
  const cache = new OverlayCache(async () => { reads++; return "x".repeat(9000); }, () => now);
  assert.equal((await cache.get("store")).length, 8000);
  now = 299_999;
  await cache.get("store");
  assert.equal(reads, 1);
  now = 300_000;
  await cache.get("store");
  assert.equal(reads, 2);
});

test("streaming redactor masks identifiers split across chunks", () => {
  const redactor = new StreamingRedactor(["SECRET-ID"]);
  const output = redactor.push("before SEC") + redactor.push("RET-ID after") + redactor.finish();
  assert.equal(output, "before [redacted] after");
  assert.throws(() => new StreamingRedactor(["x".repeat(129)]), RangeError);
});
