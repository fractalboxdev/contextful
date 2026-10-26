import test from "node:test";
import assert from "node:assert/strict";
import { learnAfterAnswer, recallSubjects } from "../src/learn.ts";

function distillationRun(events: string[], count = 5) {
  const stream = async function* () { events.push("stream"); yield "One"; yield " answer"; };
  const entries = Array.from({ length: count }, (_, n) => ({ subject: `Acme ${n}`, key: `k${n}`, learning: `L${n}` }));
  return learnAfterAnswer({ scope: "session-1", question: "What changed?", stream: stream(),
    distill: async ({ answer }) => { events.push(`distill:${answer}`); return entries; },
    land: async (scope, entry) => { events.push(`land:${scope}:${entry.subject}`); },
  });
}

// spec: surface.learn.distillation@5f9bfb8d
test("distillation runs after streaming and lands at most three scoped conclusions", async () => {
  const events: string[] = [];
  assert.equal(await distillationRun(events), "One answer");
  assert.deepEqual(events, ["stream", "distill:One answer", "land:session-1:Acme 0", "land:session-1:Acme 1", "land:session-1:Acme 2"]);
  const none: string[] = [];
  assert.equal(await distillationRun(none, 0), "One answer");
  assert.deepEqual(none, ["stream", "distill:One answer"]);
});

// spec: surface.learn.unscoped@1334c866
test("unscoped learning refuses before distillation or landing", async () => {
  let called = false;
  await assert.rejects(learnAfterAnswer({ scope: "", question: "q", stream: (async function* () { yield "a"; })(),
    distill: async () => { called = true; return []; }, land: async () => { called = true; },
  }), /ConsoleLearningUnscoped/);
  assert.equal(called, false);
});

function landInto(landed: unknown[], stream: string, distilled: { subject: string; key: string; learning: string }[]) {
  return learnAfterAnswer({ scope: "s", question: "q", stream: (async function* () { yield stream; })(),
    distill: async () => distilled, land: async (_scope: string, entry: unknown) => { landed.push(entry); } });
}

// spec: surface.learn.subject-resolution@d6e52751
test("distillation preserves observed subject and permits zero entries", async () => {
  const landed: unknown[] = [];
  await landInto(landed, "a", [{ subject: "Acme Incorporated", key: "priority", learning: "raised" }]);
  assert.deepEqual(landed, [{ subject: "Acme Incorporated", key: "priority", learning: "raised" }]);
  await landInto(landed, "b", []);
  assert.equal(landed.length, 1);
});

function matchAcme(matches: string[]) {
  return (question: string, subject: string) => {
    matches.push(`${question}:${subject}`);
    return subject === "Acme Incorporated";
  };
}

// spec: surface.learn.subject-resolution@d6e52751
test("recall resolves preserved subjects through entity matching", () => {
  const entries = [{ subject: "Acme Incorporated", key: "priority", learning: "raised" }, { subject: "Beta", key: "priority", learning: "steady" }];
  const matches: string[] = [];
  const recalled = recallSubjects(entries, "Acme's priorities", matchAcme(matches));
  assert.deepEqual(recalled, [entries[0]]);
  assert.deepEqual(matches, ["Acme's priorities:Acme Incorporated", "Acme's priorities:Beta"]);
});
