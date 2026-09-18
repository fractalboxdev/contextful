# 0241 — Source lag is measured from a coverage watermark, and an event stream moves it only when gaps are detectable

**Status:** accepted 2026-09-18
**Decides:** `visibility.sweep.refusal.ungapped-stream`

## Context

The mirror holds one source's permission state as ordinary rows, each carrying the
instant the state was read at the source. A read against a bound table compares a single
figure — the source's lag — against that table's declared budget and refuses past it.
That comparison is cheap precisely because it consults one number per source rather than
joining every candidate row to its own observation age. The number therefore has to mean
something defensible about the whole source, not about some part of it.

Permission state arrives in three shapes with very different coverage. A full run walks
every governed resource and finishes knowing what it saw. An incremental run walks a
subset — recently changed resources, a page of an enumeration — and finishes knowing only
that those resources are current. An event stream delivers individual changes as the
source notices them, and says nothing at all about the resources it did not mention.

A stream is the tempting input to move the freshness figure with: it is continuous, it
costs almost nothing, and on a healthy day it carries every change within seconds of the
change happening. The failure it hides is silence. A dropped delivery, a subscription
that lapsed, a webhook endpoint returning errors for an hour — each of these is
indistinguishable, from the consumer's side, from a source where nothing changed. A
revocation delivered to nobody leaves the mirror serving an allow that the source
withdrew, and a freshness figure driven by stream arrivals reports that state as current
for as long as the silence lasts.

Some streams are built so that silence is detectable: deliveries carry a monotonic
sequence number, a consumer notices a missing number, and the affected resources are
re-read against the source before the freshness figure moves past the gap. That property
is a fact about a particular source's delivery guarantees, not a property of streams in
general, and the mapping that lands the source is where it is known.

## Decision

`watermark_at` advances on a sweep run that finished having read every governed resource
in the source. A partial or failed run advances nothing, so a limping sweep ages the whole
source rather than reporting the fraction it managed to read as current. An event stream
moves the watermark only where the mapping declares the stream gap-detectable —
sequenced delivery with reconciliation on a detected gap — and an undeclared stream
attempting to advance it raises `VisibilityUngappedStream`. Incremental and webhook
observations still narrow the individual rows they touch the moment they land; what they
do not do is vouch for the resources they did not mention.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Watermark advances on complete coverage; a stream advances it only when gaps are detectable** *(chosen)* | One enforced figure that bounds the worst resource in the source, computable without touching candidate rows, and defensible against the question "what does this number promise". | A source whose full run is expensive has a watermark that moves in hours, so its defensible budget is hours even while incremental observations are keeping real freshness far better than that. |
| The newest observation anywhere in the source | Tracks real-world freshness closely and moves continuously; cheap sources look as good as they are. | Loses on worst-case coverage: one freshly read resource vouches for a source whose remainder still carries grants observed days ago, and the enforced number then bounds the best resource rather than the worst. |
| Per-resource lag, compared row by row | The tightest possible statement, with no source-wide pessimism at all. | Loses on enforceability: every candidate row would join to its own observation age before the budget check, putting a per-row temporal predicate inside the hot path of every read, and the refusal could no longer name one lag figure for one source. |
| Any stream advances the watermark | Near-real-time freshness on every source that emits events, with no per-source classification work. | Loses on worst-case coverage in its worst form: an undetected gap reports as health, so the one failure the budget exists to catch is the one the mechanism cannot see. |

## Criteria

1. **Worst-case coverage** — whether the enforced number bounds the oldest governed
   resource in the source or merely the newest observation. *This criterion decides.* The
   budget's whole purpose is to refuse a read that would be decided by authorization
   older than the operator is willing to accept; a figure that can be moved by one
   resource turns the budget into a statement about sweep liveness rather than about
   authorization age, and the refusal it produces protects nothing.
2. **Enforceability at request time** — whether the comparison is one figure per source
   or a predicate per candidate row.
3. **Detectability of the failure that produces the number** — whether a broken input can
   be told apart from a quiet source.
4. **Real freshness delivered** — how quickly a change at the source narrows what a
   reader reaches. Incremental and webhook observations serve this criterion without
   touching the watermark.

## Consequences

The enforced figure is honest and pessimistic. An operator reading a lag of four hours
knows that every governed resource in the source was read within four hours, which is a
claim they can defend to a security reviewer without qualification.

The accepted cost is that a source whose full enumeration is slow — a per-item permission
model under request limits is the usual shape — carries a watermark that moves in hours,
so its budget is hours, even on days when the stream delivered every change within
seconds. Real freshness is better than the enforced figure says, and the deployment
cannot spend that difference. Declaring a budget tighter than the sustainable full-run
cadence is refused at diagnose rather than silently failing at request time.

Making a stream count requires establishing a property about the source's delivery
guarantees and building reconciliation for detected gaps. That is per-source engineering
work, and it is exactly the work that makes the claim true.

Reversing toward newest-observation semantics is cheap in code and expensive in meaning:
every budget already declared would quietly weaken, with no signal at any read.

## Revisit triggers

- A source's full run cannot complete within any cadence an operator will pay for, and
  the stream for that source carries sequence numbers the mapping can reconcile against.
- Measured full-run duration on the corpora in use lands close enough to the declared
  budgets that the pessimism is costing availability rather than buying assurance.
- A gap-detectable stream is observed to miss a change that its sequence numbering did
  not reveal, which would mean the declared property is not the property that matters.
