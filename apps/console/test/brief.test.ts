import test from "node:test";
import assert from "node:assert/strict";
import { deriveBrief, topicMatch } from "../src/brief.ts";

const now = Date.parse("2026-01-08T12:00:00Z");
const conclusion = { subject: "Acme", text: "Acme filings need review", live: true };
const row = { id: "a", label: "Acme filing", topics: ["filings"], arrivedAt: "2026-01-08T11:00:00Z" };

// spec: surface.brief.topic-tier@cd81b55a
test("topic tier needs two shared tokens including the subject", () => {
  assert.equal(topicMatch(conclusion, row), true);
  assert.equal(topicMatch(conclusion, { ...row, label: "filings review", topics: [] }), false);
  assert.equal(topicMatch(conclusion, { ...row, label: "Acme", topics: [] }), false);
});

// spec: surface.brief.greeting@945ee5ee
test("brief requires a turnless present-time session and live matched arrivals", async () => {
  const input = { session: { turns: 0, vantage: "present" }, conclusions: [conclusion], arrivals: [row], now, budgetMs: 1000 };
  assert.equal((await deriveBrief(input))?.subjects[0].subject, "Acme");
  assert.equal(await deriveBrief({ ...input, session: { turns: 1, vantage: "present" } }), null);
  assert.equal(await deriveBrief({ ...input, session: { turns: 0, vantage: "2026-01-01" } }), null);
  assert.equal(await deriveBrief({ ...input, conclusions: [{ ...conclusion, live: false }] }), null);
  assert.equal(await deriveBrief({ ...input, arrivals: [] }), null);
});

function crowdedBrief(extra: typeof row[] = []) {
  const conclusions = Array.from({ length: 5 }, (_, n) => ({ subject: `Acme${n}`, text: `Acme${n} filings need review`, live: true }));
  const arrivals = conclusions.flatMap((c, n) => Array.from({ length: 5 }, (_, m) => ({ id: `${n}-${m}`, label: `${c.subject} filing`, topics: ["filings"], arrivedAt: "2026-01-08T11:00:00Z" })));
  return deriveBrief({ session: { turns: 0, vantage: "present" }, conclusions, arrivals: [...extra, ...arrivals], now, windowDays: 100, budgetMs: 1000 });
}

// spec: surface.brief.catchup-window@5445a060
test("brief caps the requested window at seven days", async () => {
  const stale = { id: "stale", label: "Acme0 filing", topics: ["filings"], arrivedAt: new Date(now - 8 * 86_400_000).toISOString() };
  const card = await crowdedBrief([stale]);
  assert.equal(card?.windowDays, 7);
  assert.ok(card?.subjects.every((subject) => subject.articles.every((article) => article.id !== "stale")));
  assert.equal(card?.subjects[0].articles.length, 3);
});

// spec: surface.brief.card-subjects@1c18a15b
test("brief carries at most three subjects", async () => {
  const card = await crowdedBrief();
  assert.equal(card?.subjects.length, 3);
});

// spec: surface.brief.articles-per-subject@89bcce7f
test("brief carries at most three articles per subject", async () => {
  const card = await crowdedBrief();
  assert.deepEqual(card?.subjects.map((subject) => subject.articles.length), [3, 3, 3]);
});

const failingArrivals = async () => { throw new Error("unavailable"); };
const stalledArrivals = () => new Promise<never>(() => {});

// spec: surface.brief.absence-is-earned@0777ef8f
test("brief reports derivation failure and timeout without a card", async () => {
  const base = { session: { turns: 0, vantage: "present" }, conclusions: [conclusion], now, budgetMs: 1000 };
  await assert.rejects(deriveBrief({ ...base, loadArrivals: failingArrivals }), /ConsoleBriefUnavailable/);
  await assert.rejects(deriveBrief({ ...base, loadArrivals: stalledArrivals, budgetMs: 5 }), /ConsoleBriefUnavailable/);
  assert.equal(await deriveBrief({ ...base, arrivals: [row], session: { turns: 1, vantage: "present" } }), null);
  assert.equal(await deriveBrief({ ...base, arrivals: [row], conclusions: [{ ...conclusion, live: false }] }), null);
});

test("brief refuses a missing derivation budget before loading arrivals", async () => {
  const pending = deriveBrief({ session: { turns: 0, vantage: "present" }, conclusions: [conclusion], now,
    loadArrivals: () => new Promise(() => {}), budgetMs: undefined as unknown as number });
  await Promise.race([
    assert.rejects(pending, /ConsoleBriefUnavailable/),
    new Promise<never>((_, reject) => setTimeout(() => reject(new Error("budget missing hung")), 50)),
  ]);
});
