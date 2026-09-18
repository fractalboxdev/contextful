# 0049 — A client's declared face requirement is refused at the handshake

**Status:** accepted 2026-09-18
**Decides:** `read.embed.refusal.required-face`

## Context

One binary links a feature set at build time and reports it at the handshake: retrieval
backends, connector families, and the binary's own faces. Two binaries built from one
source tree differ in that set, and a version string separates them not at all. A client
therefore cannot infer from the engine it is talking to which arms of the read path exist
behind it.

Most absences are degradations rather than stops. Without the lexical backend, retrieval
swaps its ranking function for a token fallback and still answers. Without the vector
backend, a supplied embedding still scores over the rows the recency window recalled. A
caller that only needs an ordered list of matching rows is served correctly by either of
those engines and would be harmed by a refusal.

A minority of callers depend on a specific arm. A product whose ranking quality rests on
approximate nearest-neighbour search reads a degraded ordering as a correct one: the rows
come back, the shape is right, and nothing in the response says the arm that was supposed
to order them contributed no candidates. That failure is silent, it is discovered in
production, and it is discovered far from its cause.

The two populations want opposite defaults. Only the caller knows which one it is in.

## Decision

A client passes `require: [...]`. Each name in that list is matched against the set the
engine reported at the handshake, and a client naming a face the engine does not report is
refused ahead of its first read with `RequiredFaceAbsent`. An engine that reports no set at
all satisfies no requirement, so an old or stripped engine fails a requirement rather than
passing it by silence. Reporting a partial set is itself never a refusal: an engine
missing a face serves every client that did not ask for it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Declared requirement, matched at the handshake** *(chosen)* | The caller that depends on an arm learns before its first read; every other caller is served by a degraded engine. | The reported set becomes a compatibility surface, so changing what a build links changes what clients accept. |
| Every absent face refuses at startup | One rule, no client-side declaration, no partial-capability matrix to reason about. | Loses on serve-through: a trimmed build serves nobody, including the majority of callers whose reads the remaining arms answer correctly. |
| Surface an absence at the failing call | Nothing to declare, no handshake contract, the error names the operation that actually needed the arm. | Loses on early surfacing: a degradation has no failing call, so a ranking arm that contributes no candidates surfaces as poor results rather than as an error. |
| Report a version string and let the client decide | No new protocol field. | Loses on early surfacing for the same reason: two binaries off one commit differ in linked faces and share a version, so the client's decision is made on a value that does not carry the fact. |

## Criteria

1. **Early surfacing** — the distance, in calls and in wall-clock, between a missing face
   and the first observable consequence of it missing. *This criterion decided the
   requirement path.* An absence that degrades rather than errors has no natural moment of
   discovery at all, so a dependent client either declares its need up front or finds out
   from a quality complaint weeks later. Nothing else on this list is as expensive to get
   wrong.
2. **Serve-through** — the fraction of callers an engine with a partial feature set can
   still answer correctly. This criterion decided the reporting path: reporting an absence
   is not a refusal, because refusing on report would take serve-through to zero for a
   population most of which needs nothing that is missing.
3. **Compatibility-surface stability** — how often a build change breaks an existing
   client. This ranked below both: it argues for reporting less, and reporting less is
   what makes early surfacing impossible.

## Consequences

A caller that depends on an arm names it once and is told immediately. A caller that
declares nothing keeps the old behavior, and meets a hard absence at the failing call
rather than at the handshake — one call late, which is the accepted cost.

The reported set is now a compatibility surface. Renaming a face, or splitting one into
two, breaks every client that named the old spelling in a requirement, so the set is
governed by the registry like any other vocabulary and a name is retired rather than
reused.

Requirement matching is exact-name matching, not capability reasoning. A client that needs
"any vector arm" names the one it knows about, and a future second vector backend under a
different name does not satisfy that requirement even where it would serve the client
correctly.

## Revisit triggers

- The reported set gains an entry that is not decided at link time — configured at runtime,
  or loaded on demand — so a set reported once at the handshake stops being true for the
  session.
- Observed `RequiredFaceAbsent` refusals are dominated by clients that would have been
  served correctly by the degraded path, which says the requirement is being declared
  defensively rather than because the caller depends on the arm.
- A second backend lands under a new name for an existing arm, making exact-name matching
  reject an engine that satisfies the caller's actual need.
