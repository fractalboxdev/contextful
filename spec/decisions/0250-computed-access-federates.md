# 0250 — A source that computes access federates its decisions, never its inputs

**Status:** accepted 2026-09-18
**Decides:** `visibility.declare-fidelity.refusal.mirroring-computed-inputs`

## Context

The mirror assumes that a source stores who reaches what: a list the sweep reads and the
join reproduces. A large class of sources does not work that way. They hold inputs — an
object's own access entries, its parent's entries, an inheritance-break marker, link-based
sharing settings, domain and tenant rules, sensitivity labels, per-type overrides — and a
sharing engine computes a decision from those inputs at the moment of the request.

Such a source is mirrorable in appearance. Every input is readable through an API, and a
mapping can project all of them into `access_grants` and `access_group_members` and
produce a table that looks exactly like a mirror of a list-based source. What that mapping
actually contains is a reimplementation of the engine's evaluation rules, written by
someone reading documentation, running against a copy of the inputs.

Reimplementations of a rules engine drift, and the drift is not evenly distributed. The
common paths agree — an object inheriting cleanly from a container everyone understands —
and the disagreements collect in exactly the constructs that exist to restrict: an
inheritance break, a deny entry outranking an allow, a link scoped to one tenant, a label
that narrows regardless of the entries beneath it. Each drifted edge where the copy is
more permissive than the engine is a disclosure, and the copy is more permissive whenever
it misses a restricting construct — which is the failure mode a partial reimplementation
has, not the exception to it.

The drift is also undetectable from inside. The mapping computes a decision; the source
computes a decision; nothing compares them. A deployment can run for a year with a
mirrored table that quietly disagrees with the source on the objects whose restrictions
someone deliberately set.

The alternative is to ask the engine. A federated leg queries such a source live under the
reader's own delegated credential, which means the decision comes from the party that owns
it, at the moment it is needed, with no second implementation to keep true.

## Decision

Where a source decides access with a sharing engine rather than storing it, the engine's
decisions are queried live under the reader's own delegated credential, and its inputs are
not reproduced. A mapping projecting a sharing engine's inputs into `access_grants` raises
`VisibilityComputedInputsMirrored`, naming the source.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Federate the decision; refuse to mirror the inputs** *(chosen)* | There is one implementation of the source's rules, and it is the source's. No drift is possible because no second evaluator exists. | Those sources answer per reader at live-query latency, contribute no rows to ranking over the store, and go dark for every reader when the source is unavailable. |
| Mirror the inputs and reproduce the engine's rules in the mapping | Rows in the store: ranking works across the whole corpus, answers are fast, and the source's availability does not bound reads. | Loses on drift: a faithful mirror of the inputs re-implements the engine, the disagreements collect in the restricting constructs, and each drifted edge is a disclosure that nothing in the system detects. |
| Declare such sources `excluded` outright | No federation machinery, no live dependency, no drift, and a guarantee that is trivially true. | Loses on corpus coverage: a per-reader federated leg answers the question the mirror cannot, so exclusion discards a usable path and removes some of the largest corpora an organization holds. |
| Mirror the inputs but serve only where the mapping and a live check agree | Store-side speed with a correctness backstop on every read. | Loses on cost and on drift together: the live check is the expensive part, so it is paid anyway, and where the two disagree the deployment still has to decide which to believe. |
| Mirror the inputs and mark the table `coarse` | An honest precision claim over a mirrored table, at store speed. | Loses on drift again: `coarse` claims an approximation at a grain the source enforces, while a reimplemented engine produces a claim at no grain the source recognizes — it is not coarser, it is different. |

## Criteria

1. **Where a drift between two implementations lands** — whether a disagreement is a
   narrower answer or a wider one, and whether anything detects it. *This criterion
   decides.* A reimplementation's misses concentrate in restricting constructs, so drift
   is systematically toward disclosure, and there is no in-system comparison that would
   surface it. Every other criterion here is about latency and coverage, which are visible
   costs an operator can weigh.
2. **Corpus coverage** — whether the source's content answers questions at all.
3. **Per-request cost and availability** — live latency, and a source outage becoming a
   read outage.
4. **Contribution to ranking** — whether the source's rows participate in retrieval over
   the whole store.

## Consequences

The deployment holds exactly one account of who reaches what in such a source, and it is
the source's own. Questions about a drifted edge have no place to arise, and an
investigation of a wrong answer has one implementation to examine.

The accepted cost is significant and permanent for these sources. They answer per reader
at live-query latency, so a question touching them is as slow as the upstream API.
Their rows contribute nothing to ranking over the store, so a retrieval that should have
surfaced them ranks without them and the federated leg answers only what it was asked
directly. They go dark for every reader when the source is unavailable, and the answer
says so rather than silently omitting them.

Reversing this is cheap to write and expensive to trust: a mirrored table would be fast,
complete, and wrong in an unknown set of places that no amount of testing enumerates.

## Revisit triggers

- A source exposes an effective-permissions endpoint returning the engine's own computed
  decisions in bulk, which would make a mirror a record of decisions rather than a
  reimplementation of rules.
- Federated latency on the sources in use lands high enough that readers avoid them, which
  would make the coverage criterion dominant rather than secondary.
- A drift-detection mechanism becomes available — the source publishing decision-change
  events against which a mirror could be continuously compared — which would turn an
  undetectable failure into a detectable one.
