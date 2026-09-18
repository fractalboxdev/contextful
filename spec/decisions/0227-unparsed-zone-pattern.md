# 0227 — An allow-set entry matching no pattern form is refused, naming the entry

**Status:** accepted 2026-09-18
**Decides:** `enforcement.place.refusal.unparsed-pattern`

## Context

An inference zone names where the model consuming a result runs, and a zone string parses,
after trimming, as `local:device`, `on-prem:<id>`, `private-cloud:<id>` or
`public-cloud:<id>`. Anything else parses as undeclared. An allow-set entry is `*`,
`local:device`, `on-prem:*`, `private-cloud:*`, `public-cloud:*`, or a category prefix carrying
a concrete identifier.

The allow-set is disjunctive: a zone is admitted when any entry matches it. That structure has
a specific consequence for a malformed entry. Adding an entry can only widen the set; an entry
that matches nothing subtracts nothing and simply fails to contribute the widening it was
written for. The set is narrower than the file appears to say, and the narrowing is silent.

The direction of that error is not the dangerous one — a silently narrow allow-set withholds
rows rather than releasing them — but the belief it produces is. The operator reads the
manifest and sees a permission granted. A reader on the admitted zone finds rows missing, and
the symptom presents as a data problem, a pipeline problem, a grant problem: anything but a
typo three lines up in the same file. `on-perm:*`, `public_cloud:*`, a trailing character, a
category the deployment invented — each reads correctly to a human and matches nothing.

The other two candidate behaviors are worse in one direction each. A wildcard interpretation
makes a typo widen. A prefix match makes a misspelled category admit a real one, which turns a
near-miss into an admission of the very category the operator was distinguishing.

## Decision

An allow-set entry matching no pattern form raises `EnforceZonePatternUnparsed`, naming the
entry, rather than becoming a pattern that silently matches nothing. The refusal fires where
the entry was written, so a deployment adding a new zone category updates the pattern grammar
before an allow-set can name it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the entry, naming it** *(chosen)* | A typo is visible in the file that contains it, and an allow-set that loads means exactly what it reads as. | A deployment adding a new zone category updates the pattern grammar before an allow-set can name it. |
| Treat it as matching nothing | The manifest always loads; a malformed entry costs nothing at boot. | Loses on failure direction for a typo: the set silently narrows, the operator reads a permission they do not have, and the symptom surfaces far from the cause. |
| Treat it as a wildcard | An unrecognized entry never blocks a legitimate zone. | Loses outright on the same criterion in the other direction: a typo widens, admitting zones the operator never named. |
| Match by prefix | Tolerates minor spelling drift without refusing. | Loses on category integrity: a misspelled category admits a real one, so `public-clou` admits the public cloud the operator was trying to distinguish. |

## Criteria

1. **Failure direction for a typo** — whether a malformed entry narrows silently, widens, or
   stops. This is the criterion that decided it.
2. **Distance between cause and symptom** — whether the operator finds the mistake where they
   made it. Matching nothing fails this badly.
3. **Category integrity** — whether a near-miss spelling can admit a category it did not name.
   Prefix matching fails this.
4. **Tolerance for a deployment's own vocabulary** — whether an operator can name a zone shape
   the grammar does not know. This is the criterion the chosen option loses on.

Failure direction decides it, and it decides against two options for opposite reasons. The
zone grammar's whole purpose is to state where data may be processed; an entry that is neither
admitted nor refused leaves that statement ambiguous, and ambiguity in a placement policy is
resolved later by whoever is debugging, under time pressure, in the wrong file. Refusing makes
the ambiguity impossible to carry forward.

## Consequences

An allow-set that loads is an allow-set whose every entry contributes, so the admitted zone set
is exactly what a reader computes from the file. The refusal names the entry, which makes the
repair mechanical. Because the grammar is closed, the set of things a manifest can say about
placement is small enough to reason about symbolically — inclusion against a floor is decided
over every constructor and every identifier rather than by probing example zones.

The accepted cost: a deployment with a zone shape the grammar does not cover cannot express it,
and updating the grammar is a change to the system rather than to a manifest. That makes the
zone vocabulary centrally owned, which is deliberate for a placement policy and inconvenient for
an operator with an unusual environment. How often that arises is unmeasured; the five
constructors cover the environments the placement model was designed around, and nothing
establishes that they cover every deployment.

Reversing this toward silent tolerance is cheap and re-opens the exact belief failure the rule
exists to prevent, retroactively, across every manifest already written.

## Revisit triggers

- Deployments are found needing a zone category the five constructors cannot express, often
  enough that grammar changes become routine rather than exceptional.
- A zone identifier form appears that legitimately contains the separator the parse relies on,
  making valid entries unparseable.
- The allow-set gains a negation or exclusion form, which would change the direction a
  malformed entry fails in and require this to be re-argued.
