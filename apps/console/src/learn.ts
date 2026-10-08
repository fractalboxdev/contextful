export type Learning = { subject: string; key: string; learning: string };

export function recallSubjects(entries: Learning[], question: string, matchEntity: (question: string, subject: string) => boolean): Learning[] {
  return entries.filter((entry) => matchEntity(question, entry.subject));
}

export async function learnAfterAnswer(input: {
  scope: string;
  question: string;
  stream: AsyncIterable<string>;
  distill: (exchange: { question: string; answer: string }) => Promise<Learning[]>;
  land: (scope: string, entry: Learning) => Promise<void>;
}): Promise<string> {
  if (!input.scope.trim()) throw new Error("ConsoleLearningUnscoped");
  let answer = "";
  for await (const chunk of input.stream) answer += chunk;
  const entries = await input.distill({ question: input.question, answer });
  for (const entry of entries.slice(0, 3)) {
    if (!entry.subject || !entry.key || !entry.learning) continue;
    await input.land(input.scope, { subject: entry.subject, key: entry.key, learning: entry.learning });
  }
  return answer;
}
