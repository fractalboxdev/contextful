export function answerDelivery(input: { operator: string; answer: string; accessExplanation: boolean; share?: boolean }): {
  recipient: string; answer: string; share: false;
} {
  if (!input.operator.trim()) throw new Error("VisibilityAskerlessAudience");
  if (input.accessExplanation && input.share) throw new Error("VisibilityShareAffordance");
  return { recipient: input.operator, answer: input.answer, share: false };
}

export function scheduledAudience(input: {
  identity: "service" | "operator";
  operator?: string;
  destination: string;
  corpus: string[];
  reviewedCorpus?: string[];
}): { destination: string; corpus: string[] } {
  if (input.identity === "service" || !input.operator?.trim()) {
    throw new Error(`VisibilityAskerlessAudience: ${input.destination}`);
  }
  const reviewed = new Set(input.reviewedCorpus ?? []);
  return { destination: input.destination, corpus: input.corpus.filter((entry) => reviewed.has(entry)) };
}
