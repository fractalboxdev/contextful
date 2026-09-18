# 0232 — A synthesized row declaring a zone set wider than its evidence intersection is refused and resolves to the intersection

**Status:** accepted 2026-09-18
**Decides:** `enforcement.place.refusal.declaration-above-the-evidence-floor`

## Context

The engine writes rows of its own. A synthesis step reads rows from several tables and
lands a conclusion — a summary, a derived fact, a memory row — as ordinary stored content.
That row is then subject to the same placement resolution as any other, and the fail-closed
resolution covers it: tables discovered after startup, synthesized memory among them, get
the local-and-on-premises pair until something widens them.

What makes synthesis different from ingestion is that its output is derived from inputs
whose placement constraints are already known. A conclusion drawn from a table restricted
to an on-premises environment carries that table's content in compressed form. It is
shorter, it is rephrased, and it is not less sensitive for either reason. If the
synthesized row could declare a set wider than its inputs allow, synthesis would be the
widening mechanism: a caller wanting a restricted table's content in a public cloud reads
it, summarizes it, declares the summary permissive, and the summary goes.

The inputs are available at the moment the row is written. The synthesis step knows which
tables it read, and each of those tables carries an effective allow-set. The constraint the
output must respect is therefore computable rather than declared, and the only question is
what the system does when the declaration and the computation disagree.

## Decision

A synthesized row's declared zone set is legal at or beneath the intersection of the sets
its evidence tables carry: a zone is admitted where every evidence table admits it. A row
declaring a set wider than that intersection raises `EnforceEvidenceFloorExceeded` and
resolves to the intersection. The refusal names the disagreement; the resolution is what
governs the read, so a declaration that survives the refusal path still cannot widen the
row past its evidence.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Intersect the evidence sets, refuse a wider declaration, and resolve to the intersection** *(chosen)* | Synthesis cannot launder a placement constraint, and the resulting row is still usable wherever every one of its inputs was usable. | A conclusion drawn over evidence that includes one narrowly-zoned table is as narrow as that table, however marginal that table's contribution to the conclusion was. |
| Trust the declared set | The synthesis step, which understands what it wrote, decides where the result may go. | Loses on laundering: the declaration is written by the same path that read the restricted input, so the constraint is enforced by the party it constrains. |
| Take the union of the evidence sets | A conclusion is as available as its most available input, which matches an intuition about derived material. | Loses on laundering in a weaker form — mixing one permissive table into the evidence widens the output, which is the same attack with one extra step. |
| Refuse synthesis over mixed-zone evidence entirely | No intersection to compute; the output's placement equals its inputs', unambiguously. | Loses on utility: the intersection is a usable answer, and mixed-zone evidence is the ordinary case once a store holds tables from more than one source. |
| Attach per-input provenance and resolve placement per fragment of the output | The narrow input constrains only the sentences derived from it. | Loses on decidability: fragment-level provenance through a synthesis step is not something the system observes, so the resolution would rest on a claim rather than on a computed set. |

## Criteria

1. **Whether synthesis can launder a constraint** — whether a path exists from a restricted
   table's content to a wider zone that does not go through a deliberate widening of that
   table. *This criterion decides.* Placement is only worth declaring if no cheap
   derivation escapes it, and both the trust option and the union option leave that
   derivation open.
2. **Utility of the resulting row** — whether the computed set leaves the conclusion
   readable anywhere useful.
3. **Decidability from what the system observes** — whether the constraint is computed from
   recorded inputs or from an assertion.
4. **Predictability for the operator** — whether the placement of a synthesized row can be
   reasoned about from the tables it read.

## Consequences

A conclusion the engine writes is readable exactly where all of its sources were readable,
computed rather than asserted, so widening a synthesized row means widening the evidence
tables it came from.

The cost accepted is over-narrowing. One narrowly-zoned table among a dozen inputs pins the
whole conclusion to that table's set, regardless of how little it contributed. Operators
who want a broadly readable synthesis exclude the narrow table from the evidence rather
than widening the output.

An empty evidence list intersects to everything, which is the one place this rule inverts.
A synthesized row that records no evidence is unconstrained by this mechanism, and nothing
here discharges that obligation — it rests on the synthesis step recording what it read.
That is a real gap, not a rounding error: a synthesis path that drops its evidence list
produces rows this floor does not bind.

Reversing toward a declared set is expensive because the fail-closed resolution over
discovered tables assumes synthesized rows arrive constrained; loosening this would put
memory rows back in the widest category by default.

## Revisit triggers

- A synthesis path is found landing rows with an empty evidence list, which turns the
  inversion above from a theoretical gap into an observed one.
- Over-narrowing is observed in practice — synthesized rows pinned by an input that
  contributed nothing to the conclusion — at a rate that makes operators exclude tables
  from evidence to get usable output.
- Per-fragment provenance through synthesis becomes observable rather than claimed, which
  would make the finer-grained resolution decidable.
