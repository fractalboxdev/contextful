# 0218 — Subject claims are bound parameters, and splicing one into SQL text is refused by the gate

**Status:** accepted 2026-09-18
**Decides:** `enforcement.filter-rows.refusal.interpolated-claim`

## Context

A row predicate is written against the calling subject: a principal identifier, a tenant, a
group membership, an attribute carried on the credential. Those values are attacker-adjacent.
A principal identifier is frequently a display name, an email address or an externally-issued
identifier, and the party who controls it is not always the party the predicate protects
against. If such a value reaches the engine as SQL text, it reaches the parser, and a value
that reaches the parser can rewrite the predicate that was supposed to constrain it.

The failure mode here is not one call site. A predicate is assembled wherever a session builds
its relations, wherever a mask exception is compiled, wherever a template binds. Each of those
is a place where a future change could produce the expression by concatenation rather than by
binding, and the resulting defect is silent: the query runs, returns rows, and nothing
distinguishes the widened predicate from the intended one until someone reads the tree.

Sanitizing the value at issuance is the other obvious position, and it fails on legitimacy
rather than on security. Names carry apostrophes. Identifiers carry separators. Externally
issued subject identifiers carry whatever the issuer chose. A rule that refuses those values
refuses real people, and the pressure to add exceptions to it starts immediately.

## Decision

Subject claims reach a predicate through a session-scoped subject relation, bound as
prepared-statement parameters and joined against. A principal value carrying SQL syntax stays
a value: it occupies a parameter slot and reaches no parser. Splicing a subject claim into SQL
text anywhere in the runtime raises `EnforceInterpolatedSubjectClaim`, and the gate rejects
the change that introduced it. Mask exceptions and exception conditions bind through the same
mechanism, so there is one route by which a subject value becomes part of a restriction.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Bind through a subject relation; refuse interpolation tree-wide at the gate** *(chosen)* | A hostile principal value cannot become syntax — the property holds by construction, and its absence is a property of the tree rather than of a reviewer's attention. | One more check every change must satisfy, and a predicate reading naturally as a literal is written as a join instead. |
| Escape and interpolate at each site | No join to push down; the predicate text reads exactly as written. | Loses on durability of the property: correctness depends on every future call site getting escaping right, and a miss produces a widened predicate with no symptom. |
| Reject principal values containing SQL characters at issuance | One check, at one point, over values the system mints. | Loses on legitimacy: names and externally-issued identifiers carry quotes and separators, so the rule refuses real subjects and accumulates exceptions. |
| Bind parameters but police interpolation by review alone | Same runtime property, no gate to maintain. | Loses on the same criterion as escaping: the property survives only as long as every reviewer notices, and the regression is invisible at runtime. |

## Criteria

1. **Whether a hostile principal value can become SQL** — the property the layer exists to
   hold. Escaping and review-only both hold it conditionally.
2. **Where the property lives** — in the shape of the code, or in the vigilance of whoever
   touches it next. This is the criterion that decided it.
3. **Legitimacy of accepted subject values** — whether the rule refuses real identifiers.
   Issuance-time rejection fails this.
4. **Cost to the author of a predicate** — how far the written form is from the natural one.
   This is the criterion the chosen option loses on.

Where the property lives decides it. Escaping at every site and binding at every site produce
the same runtime behavior on the day they are both correct; they differ in what happens on the
day one site is written by someone who did not read this. A parameter slot admits no syntax
whatever the caller sends, and a gate check turns "no site interpolates" from a claim about
current code into a condition every change is measured against.

## Consequences

The subject join pushes down into the scan, so the restricted read costs what the unrestricted
one costs in the common case, and the same mechanism serves predicates, mask exceptions and
exception conditions rather than three separately-argued paths.

The accepted cost is expressive and procedural. A predicate that would read as a literal
comparison against a claim is written as a join against the subject relation, which is longer
and less obvious to someone reading the manifest. The gate check is one more condition a
contributor's change must satisfy, and it will occasionally fire on a construction that is
harmless — a claim used in a log line, a diagnostic string — which costs a rewrite to satisfy
a rule that does not distinguish those cases.

Reversing this is expensive: manifests, mask exceptions and template bindings are all written
against parameter binding, and admitting interpolation anywhere re-opens the property
everywhere, since the guarantee is stated tree-wide rather than per site.

## Revisit triggers

- The gate check is found to fire on constructions that cannot reach a parser, at a rate that
  makes contributors route around it.
- A subject-derived value is needed in a position that admits no parameter slot — an
  identifier, a relation name — which the current mechanism cannot express.
- A measured case where the pushed-down subject join, rather than the predicate itself,
  dominates scan cost.
