import assert from "node:assert/strict";
import { test } from "node:test";
import {
  ConsoleError,
  OverlayCache,
  StreamingRedactor,
  createTurn,
  parseVantage,
  parseWebBound,
  resolveReaderCredential,
  sampleArrivals,
  validatePack,
} from "../src/turn.ts";

const readTool = { name: "query", pack: "data", kind: "read" as const, table: "events" };

function harness(overrides: Record<string, unknown> = {}) {
  const calls: string[] = [];
  const turn = createTurn({
    tools: [readTool],
    tables: [{ name: "events", kind: "data" }],
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
  const { turn, calls } = harness({ synthesize: async function* () { yield "Three rows [row-1]."; } });
  const answer = await turn.ask({ question: "How many?", packs: ["data"] });
  assert.deepEqual(calls, ["query"]);
  assert.match(answer.text, /Three rows/);
  assert.match(answer.text, /\[source-1\]/);
  assert.match(answer.text, /Sources\n- Count/);
  assert.deepEqual(answer.sources.map((source: { id: string }) => source.id), ["source-1"]);
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

// spec: surface.ground.org-face-read-only@030074cb
test("organization packs refuse writes at startup", () => {
  assert.throws(() => validatePack({ name: "shared", face: "organization", tools: [{ name: "erase", kind: "write" }] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleWriteToolOnOrgFace");
});

// spec: surface.ground.direct-file-read@4edf9c49
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

test("source identifiers returned to Query never expose denylisted material", async () => {
  const { turn } = harness({
    denylist: ["SECRET-ID"],
    transport: { call: async () => ({ rows: [{ value: 1 }], sources: [{ id: "SECRET-ID", label: "Public filing" }] }) },
    synthesize: async function* () { yield "Answer [SECRET-ID]."; },
  });
  const answer = await turn.ask({ question: "Explain", packs: ["data"] });
  assert.doesNotMatch(JSON.stringify(answer), /SECRET-ID/);
  assert.match(answer.sources[0].id, /^source-\d+$/);
});

test("rows without governed provenance cannot become answer prose", async () => {
  const { turn } = harness({ transport: { call: async () => ({ rows: [{ value: 1 }], sources: [] }) } });
  await assert.rejects(turn.ask({ question: "Explain", packs: ["data"] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleUngroundedAnswer");
});

test("a web leg with an invalid publication bound fails before dispatch", async () => {
  const { turn, calls } = harness({
    tools: [{ name: "web", pack: "web", kind: "read", leg: "web" }],
    planner: async () => [{ tool: "web", arguments: {}, publicationBound: "no-date" }],
  });
  await assert.rejects(turn.ask({ question: "Search", packs: ["web"] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleWebBoundUnparseable");
  assert.deepEqual(calls, []);
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
  assert.throws(() => parseVantage("2026-02-30T12:30:00Z"),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleVantageUnparseable");
  assert.throws(() => parseWebBound("unknown"),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleWebBoundUnparseable");
});

test("refused per-reader mint falls back only to an installed shared credential", async () => {
  const mint = async () => { throw new ConsoleError("ConsoleTokenExchangeRefused", "denied", 403); };
  assert.equal(await resolveReaderCredential({ mint, shared: "shared-token" }), "shared-token");
  await assert.rejects(resolveReaderCredential({ mint }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleTokenExchangeRefused");
});

test("reader credential resolution preserves transport and service failures", async () => {
  const outage = new ConsoleError("ConsoleTokenExchangeUnavailable", "outage", 503);
  await assert.rejects(resolveReaderCredential({ mint: async () => { throw outage; }, shared: "shared-token" }),
    (error: unknown) => error === outage);
  const network = new TypeError("network offline");
  await assert.rejects(resolveReaderCredential({ mint: async () => { throw network; }, shared: "shared-token" }),
    (error: unknown) => error === network);
});

test("planner scaffolding lists data tables while memory relations remain outside it", async () => {
  let plannerTables: unknown[] = [];
  const { turn } = harness({
    tables: [{ name: "events", kind: "data" }, { name: "memory_entries", kind: "memory" }],
    planner: async (input: { tables: unknown[] }) => { plannerTables = input.tables; return [{ tool: "query", arguments: {} }]; },
  });
  await turn.ask({ question: "Count", packs: ["data"] });
  assert.deepEqual(plannerTables, [{ name: "events", kind: "data" }]);
});

// spec: surface.plan-turn.planner-reached-memory@a6f057a5
test("a planner call targeting a memory relation dispatches nothing", async () => {
  const { turn, calls } = harness({
    tables: [{ name: "memory_entries", kind: "memory" }],
    tools: [{ ...readTool, table: "memory_entries" }],
  });
  await assert.rejects(turn.ask({ question: "Recall", packs: ["data"] }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsolePlannerReachedMemory");
  assert.deepEqual(calls, []);
});

test("the code path refuses an unresponsive transport within ten seconds", async () => {
  const { turn } = harness({
    transport: { call: async () => new Promise(() => {}) },
    timeoutMs: 5,
  });
  await assert.rejects(turn.ask({ question: "Slow", packs: ["data"] }),
    /code path exceeded 10 s/);
});

test("the transport receives a bounded read and cannot return more than 5000 rows", async () => {
  let maxRows = 0;
  const { turn } = harness({ transport: { call: async (call: { maxRows: number }) => {
    maxRows = call.maxRows;
    return { rows: Array.from({ length: 5001 }, () => ({})), sources: [] };
  } } });
  await assert.rejects(turn.ask({ question: "Many", packs: ["data"] }), RangeError);
  assert.equal(maxRows, 5000);
});

test("one table cannot exceed the row budget through repeated calls", async () => {
  const limits: number[] = [];
  const { turn } = harness({
    tools: [{ ...readTool, table: "events" }],
    planner: async () => [{ tool: "query", arguments: {} }, { tool: "query", arguments: {} }],
    transport: { call: async (call: { maxRows: number }) => {
      limits.push(call.maxRows);
      return { rows: Array.from({ length: 3000 }, () => ({})), sources: [{ id: "events", label: "Events" }] };
    } },
  });
  await assert.rejects(turn.ask({ question: "Many", packs: ["data"] }), RangeError);
  assert.deepEqual(limits, [5000, 2000]);
});

test("unbound store tools cannot claim a per-table row budget", async () => {
  const dispatched: string[] = [];
  const { turn } = harness({
    tools: [{ name: "query-a", pack: "data", kind: "read" }, { name: "query-b", pack: "data", kind: "read" }],
    planner: async () => [
      { tool: "query-a", arguments: { sql: "SELECT * FROM events" } },
      { tool: "query-b", arguments: { sql: "SELECT * FROM events" } },
    ],
    transport: { call: async (call: { tool: string }) => {
      dispatched.push(call.tool);
      return { rows: Array.from({ length: 5000 }, () => ({})), sources: [{ id: "events", label: "Events" }] };
    } },
  });
  await assert.rejects(turn.ask({ question: "Many", packs: ["data"] }), /registered data table/);
  assert.deepEqual(dispatched, []);
});

test("distinct registered tools share one data table's row budget", async () => {
  const limits: number[] = [];
  const { turn } = harness({
    tools: [
      { name: "query-a", pack: "data", kind: "read", table: "events" },
      { name: "query-b", pack: "data", kind: "read", table: "events" },
    ],
    planner: async () => [{ tool: "query-a", arguments: {} }, { tool: "query-b", arguments: {} }],
    transport: { call: async (call: { maxRows: number }) => {
      limits.push(call.maxRows);
      return { rows: Array.from({ length: 3000 }, () => ({})), sources: [{ id: "events", label: "Events" }] };
    } },
  });
  await assert.rejects(turn.ask({ question: "Many", packs: ["data"] }), RangeError);
  assert.deepEqual(limits, [5000, 2000]);
});

// spec: surface.set-vantage.sample-labels@95b1fd75
test("a table contributes at most three sampled arrival labels", () => {
  assert.deepEqual(sampleArrivals([{ table: "events", labels: ["a", "b", "c", "d"] }]),
    [{ table: "events", labels: ["a", "b", "c"] }]);
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

// spec: surface.speak.redactor-lookahead@2d3a1762
test("streaming redactor masks identifiers split across chunks", () => {
  const redactor = new StreamingRedactor(["SECRET-ID"]);
  const output = redactor.push("before SEC") + redactor.push("RET-ID after") + redactor.finish();
  assert.equal(output, "before [redacted] after");
  assert.throws(() => new StreamingRedactor(["x".repeat(129)]), RangeError);
});
