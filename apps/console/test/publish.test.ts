import test from "node:test";
import assert from "node:assert/strict";
import { answerDelivery, scheduledAudience } from "../src/publish.ts";

test("a normal answer is delivered only to its asking operator", () => {
  assert.deepEqual(answerDelivery({ operator: "alice", answer: "grounded", accessExplanation: false }), { recipient: "alice", answer: "grounded", share: false });
});

// spec: surface.publish-answer.share-affordance@13ee63fa
test("an access explanation cannot offer a share control", () => {
  assert.throws(() => answerDelivery({ operator: "alice", answer: "why", accessExplanation: true, share: true }), /VisibilityShareAffordance/);
});

// spec: surface.publish-answer.askerless-audience@73ecc499
test("a scheduled service post refuses an audience and names its destination", () => {
  assert.throws(() => scheduledAudience({ identity: "service", destination: "#team", corpus: ["private"] }), /VisibilityAskerlessAudience.*#team/);
});

test("a human scheduled post carries only reviewed destination corpus", () => {
  assert.deepEqual(scheduledAudience({ identity: "operator", operator: "alice", destination: "team", corpus: ["shared", "private"], reviewedCorpus: ["shared"] }), { destination: "team", corpus: ["shared"] });
});
