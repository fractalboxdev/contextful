# 0067 — A prediction declares how it settles, and the engine evaluates no stored rule

**Status:** accepted 2026-09-18
**Decides:** `memory.settle.refusal.unknown-source`, `memory.settle.refusal.comparator-missing`, `memory.settle.refusal.comparator-on-judgment`

## Context

A prediction settles by one of three mechanisms. A `metric` source compares an ingested
observable against a comparator. An `adjudicator` source is a resolver pass fed raw source
rows. A `manual` source is a human queue. The three differ in what they owe: a metric rule
owes a comparator, and a judgment source owes a settling citation instead.

The comparator is operator-supplied text. Something has to turn it into a verdict, and
where that something lives is the decision. Running it inside the engine means an
expression language: a parser, an evaluation environment, a set of functions the expression
may call, a resource bound on evaluation, and a standing question about what an expression
can reach — the store's own tables, the filesystem, the clock. That surface would be
reachable by anyone who can register a prediction, which is a wider set than the operators
who configure the deployment. It also travels: the stored corpus is meant to be portable
across SQL engines, and a stored expression is only portable to a reader that implements the
same language.

Keeping evaluation outside the engine removes all of that. The comparator is stored verbatim
and a component the engine does not schedule reads it, evaluates it against the ingested
observable, and writes an observation row through the ordinary door.

A second question sits beside it: when a registration is malformed, does the row still land.
The tempting answer is yes — store it, interpret it later, let the settling component decide
what an unknown source or a comparator-less metric rule means. What that produces is a row
asserting a settlement contract nothing can enforce. Every later reader, including the
scheduler and the label view, reads it as an enforced contract because there is no column
saying otherwise.

Inferring the source from which fields were supplied is the third temptation, and it fails
on a smaller scale: a typo in a field name reclassifies how a claim settles with no error at
any point.

## Decision

Every registration names `metric`, `adjudicator` or `manual`. A registration naming a
resolution source outside that set raises `OutcomeSourceUnknown` at parse and is stored not
at all. A `metric` registration carrying no comparator raises `OutcomeComparatorMissing`. A
comparator supplied alongside `adjudicator` or `manual` raises `OutcomeComparatorOnJudgment`.
A metric comparator is stored verbatim and evaluated outside the engine; the engine runs no
expression language over a stored rule.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A closed source set, refused at parse; comparators stored verbatim and evaluated outside the engine** *(chosen)* | Zero expression surface in the engine; the stored corpus stays portable; an unenforceable contract never reaches storage | An application wanting a new settlement mechanism declares it through the source set rather than supplying a rule, and metric evaluation lives in a component the engine does not schedule |
| An embedded expression evaluator for metric rules | Settlement is self-contained: register a rule and the engine settles it | Lost on attack surface and on portability: the evaluator is reachable by anyone who can register, and a stored expression only travels to a reader implementing the same language |
| Infer the source from the shape of the supplied fields | No source field to fill; fewer refusals; the registration reads as data rather than as a declaration | Lost on discoverability: a typo would silently reclassify how a claim settles, and nothing surfaces the reclassification |
| Store an unknown source for later interpretation | Forward compatibility — a source the engine does not know yet still round-trips | Lost on parse-time refusal: an unenforceable contract in storage reads as an enforced one to the scheduler and the label view alike |

## Criteria

1. **Expression surface exposed to a registrant** — what a caller can cause the engine to
   evaluate.
2. **Portability of the stored corpus** — whether a reader on another SQL engine can read
   what is stored.
3. **Whether storage can hold an unenforceable contract** — the gap between what a row
   asserts and what any component will do.
4. **Extensibility** — the cost of adding a settlement mechanism.

Criterion 1 decided where evaluation lives, and criterion 3 decided what happens to a
malformed registration. They are separate halves and the record keeps them apart. Criterion
1 outranks criterion 4 because the surface is permanent and the extensibility cost is a
declaration: adding a source is a change to a closed set, which is a reviewable edit, while
an evaluator is a capability that cannot be narrowed after registrants start depending on
it. Criterion 3 rejects the store-and-interpret option even though it costs nothing at
write time, because the cost lands on every reader afterwards and on none of them visibly.

## Consequences

The engine's settlement path is a join and three field checks. Nothing it schedules
evaluates operator text, so the resource and reachability questions an expression language
would raise do not arise.

Metric evaluation lives outside the engine, in a component with its own deployment, its own
cadence and its own failure modes. A deployment that runs no such component registers metric
predictions that nothing settles, and the engine reports that as unsettled rather than as a
configuration error.

A new settlement mechanism is a change to the source set — code, review, release — rather
than a rule an application supplies at registration. That is slower than supplying a rule
and is the cost accepted here.

Loosening the parse-time refusals later is cheap; tightening them is not. Once rows carrying
an unknown source exist, every reader needs an arm for them, and the scored population spans
both eras.

## Revisit triggers

- Applications needing settlement mechanisms faster than the source set can be changed,
  which is the extensibility cost coming due.
- A sandboxed evaluation environment with a stated resource bound and a stated reachability
  set, which changes what criterion 1 is measuring.
- A stored-rule format that is data rather than an expression — a comparison operator, a
  threshold and a column — which would sit inside the engine without an expression language.
