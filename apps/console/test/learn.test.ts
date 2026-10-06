import test from "node:test";
import assert from "node:assert/strict";
import { learnAfterAnswer, recallSubjects } from "../src/learn.ts";

test("distillation runs after streaming and lands at most three scoped conclusions", async () => {
  const events: string[] = [];
  const stream = async function* () { events.push("stream"); yield "One"; yield " answer"; };
  const entries = Array.from({ length: 5 }, (_, n) => ({ subject: `Acme ${n}`, key: `k${n}`, learning: `L${n}` }));
  const answer = await learnAfterAnswer({ scope: "session-1", question: "What changed?", stream: stream(),
    distill: async ({ answer }) => { events.push(`distill:${answer}`); return entries; },
    land: async (scope, entry) => { events.push(`land:${scope}:${entry.subject}`); },
  });
  assert.equal(answer, "One answer");
  assert.deepEqual(events, ["stream", "distill:One answer", "land:session-1:Acme 0", "land:session-1:Acme 1", "land:session-1:Acme 2"]);
});

// spec: surface.learn.unscoped@1334c866
test("unscoped learning refuses before distillation or landing", async () => {
  let called = false;
  await assert.rejects(learnAfterAnswer({ scope: "", question: "q", stream: (async function* () { yield "a"; })(),
    distill: async () => { called = true; return []; }, land: async () => { called = true; },
  }), /ConsoleLearningUnscoped/);
  assert.equal(called, false);
});

test("distillation preserves observed subject and permits zero entries", async () => {
  const landed: unknown[] = [];
  const base = { scope: "s", question: "q", stream: (async function* () { yield "a"; })(), land: async (_scope: string, entry: unknown) => { landed.push(entry); } };
  await learnAfterAnswer({ ...base, distill: async () => [{ subject: "Acme Incorporated", key: "priority", learning: "raised" }] });
  assert.deepEqual(landed, [{ subject: "Acme Incorporated", key: "priority", learning: "raised" }]);
  await learnAfterAnswer({ ...base, stream: (async function* () { yield "b"; })(), distill: async () => [] });
  assert.equal(landed.length, 1);
});

test("recall resolves preserved subjects through entity matching", () => {
  const entries = [{ subject: "Acme Incorporated", key: "priority", learning: "raised" }, { subject: "Beta", key: "priority", learning: "steady" }];
  const matches: string[] = [];
  const recalled = recallSubjects(entries, "Acme's priorities", (question, subject) => {
    matches.push(`${question}:${subject}`);
    return subject === "Acme Incorporated";
  });
  assert.deepEqual(recalled, [entries[0]]);
  assert.deepEqual(matches, ["Acme's priorities:Acme Incorporated", "Acme's priorities:Beta"]);
});
