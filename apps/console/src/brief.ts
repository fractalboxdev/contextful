export type Conclusion = { subject: string; text: string; live: boolean };
export type Arrival = { id: string; label: string; topics: string[]; arrivedAt: string };
export type Brief = { windowDays: number; subjects: Array<{ subject: string; articles: Arrival[] }> };

function tokens(text: string): Set<string> {
  return new Set((text.toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? []).map((word) => word.length > 4 && word.endsWith("s") ? word.slice(0, -1) : word));
}

export function topicMatch(conclusion: Conclusion, row: Arrival): boolean {
  const subject = tokens(conclusion.subject);
  const left = tokens(`${conclusion.subject} ${conclusion.text}`);
  const right = tokens(`${row.label} ${row.topics.join(" ")}`);
  const shared = [...left].filter((word) => right.has(word));
  return shared.length >= 2 && shared.some((word) => subject.has(word));
}

export type BriefInput = {
  session: { turns: number; vantage: string };
  conclusions: Conclusion[];
  arrivals?: Arrival[];
  loadArrivals?: () => Promise<Arrival[]>;
  now: number;
  windowDays?: number;
  budgetMs?: number;
};

export async function deriveBrief(input: BriefInput): Promise<Brief | null> {
  if (input.session.turns !== 0 || input.session.vantage !== "present") return null;
  const live = input.conclusions.filter((entry) => entry.live);
  if (live.length === 0) return null;
  const windowDays = Math.max(0, Math.min(input.windowDays ?? 7, 7));
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const source = input.loadArrivals ? input.loadArrivals() : Promise.resolve(input.arrivals ?? []);
    const budgetMs = input.budgetMs;
    const arrivals = budgetMs === undefined ? await source : await Promise.race([
      source,
      new Promise<never>((_, reject) => { timer = setTimeout(() => reject(new Error("ConsoleBriefUnavailable")), Math.max(0, budgetMs)); }),
    ]);
    const since = input.now - windowDays * 86_400_000;
    const recent = arrivals.filter((row) => {
      const arrived = Date.parse(row.arrivedAt);
      return Number.isFinite(arrived) && arrived >= since && arrived <= input.now;
    });
    const subjects = live.map((entry) => ({ subject: entry.subject, articles: recent.filter((row) => topicMatch(entry, row)).slice(0, 3) }))
      .filter((entry) => entry.articles.length > 0).slice(0, 3);
    return subjects.length ? { windowDays, subjects } : null;
  } catch {
    throw new Error("ConsoleBriefUnavailable");
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}
