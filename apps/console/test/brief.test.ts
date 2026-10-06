import test from "node:test";
import assert from "node:assert/strict";
import { deriveBrief, topicMatch } from "../src/brief.ts";

const now = Date.parse("2026-01-08T12:00:00Z");
const conclusion = { subject: "Acme", text: "Acme filings need review", live: true };
const row = { id: "a", label: "Acme filing", topics: ["filings"], arrivedAt: "2026-01-08T11:00:00Z" };

test("topic tier needs two shared tokens including the subject", () => {
  assert.equal(topicMatch(conclusion, row), true);
  assert.equal(topicMatch(conclusion, { ...row, label: "filings review", topics: [] }), false);
  assert.equal(topicMatch(conclusion, { ...row, label: "Acme", topics: [] }), false);
});

test("brief requires a turnless present-time session and live matched arrivals", async () => {
  const input = { session: { turns: 0, vantage: "present" }, conclusions: [conclusion], arrivals: [row], now };
  assert.equal((await deriveBrief(input))?.subjects[0].subject, "Acme");
  assert.equal(await deriveBrief({ ...input, session: { turns: 1, vantage: "present" } }), null);
  assert.equal(await deriveBrief({ ...input, session: { turns: 0, vantage: "2026-01-01" } }), null);
  assert.equal(await deriveBrief({ ...input, conclusions: [{ ...conclusion, live: false }] }), null);
  assert.equal(await deriveBrief({ ...input, arrivals: [] }), null);
});

test("brief caps window at seven days, subjects at three, and articles at three", async () => {
  const conclusions = Array.from({ length: 5 }, (_, n) => ({ subject: `Acme${n}`, text: `Acme${n} filings need review`, live: true }));
  const arrivals = conclusions.flatMap((c, n) => Array.from({ length: 5 }, (_, m) => ({ id: `${n}-${m}`, label: `${c.subject} filing`, topics: ["filings"], arrivedAt: "2026-01-08T11:00:00Z" })));
  const card = await deriveBrief({ session: { turns: 0, vantage: "present" }, conclusions, arrivals, now, windowDays: 100 });
  assert.equal(card?.windowDays, 7);
  assert.equal(card?.subjects.length, 3);
  assert.ok(card?.subjects.every((subject) => subject.articles.length === 3));
});

test("brief reports derivation failure and timeout without a card", async () => {
  const base = { session: { turns: 0, vantage: "present" }, conclusions: [conclusion], now };
  await assert.rejects(deriveBrief({ ...base, loadArrivals: async () => { throw new Error("unavailable"); } }), /ConsoleBriefUnavailable/);
  await assert.rejects(deriveBrief({ ...base, loadArrivals: () => new Promise(() => {}), budgetMs: 5 }), /ConsoleBriefUnavailable/);
});
