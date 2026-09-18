# 0298 — An unparsable store registry falls back while one bad entry is dropped by name

**Status:** accepted 2026-09-18
**Decides:** `control.register-store.refusal.malformed-entry`

## Context

Which stores a deployment serves resolves per request from the worker's environment: a JSON
array of entries, plus an optional allowlist that both filters and orders. Ids, hostnames
and route paths are not credentials, so the array lives in committed configuration or a
deploy-time variable and a store appears with no rebuild. That property is the point of the
design, and it is also why the payload arrives unvalidated at request time rather than at
build time.

Two failures are possible in that payload and they are not the same failure. The whole
array can fail to parse — a truncated variable, a quoting mistake in a deploy tool, an empty
string where an array was expected. Or one entry among many can be wrong while the array
itself is well-formed: a missing field, an id that is not kebab-case, a route path that is
not a path.

A uniform policy has to pick which of two bad outcomes to take on both. Strict everywhere
means one malformed entry among nine takes the whole surface down, and a deployment loses
eight working stores to a typo in the ninth. Lenient everywhere means an unparsable payload
yields an empty store list, and the surface comes up with nothing to render — which is also
what a brand-new deployment with no data looks like, so the first thing an operator sees is
ambiguous between "the product works and you have no stores" and "your configuration is
broken".

Two stores ship inside the product on the criterion that they need no configuration: a
bundled fixture with no upstream that renders the entire surface with no secrets, and a
loopback store marked development-only. That changes what an empty list costs, because
falling back to the built-ins is a working surface rather than a blank one — someone can see
what the product does while the configuration is fixed.

## Decision

A payload that does not parse falls back to the built-ins, serving a working surface while
the configuration is wrong. One malformed entry among many is dropped with a diagnostic and
raises `StoreEntryMalformed` naming that entry alone, and the remaining entries serve. Both
variables unset serve the built-ins alone. A deployment with one broken store is therefore
able to tell which store is broken, and a deployment with a broken payload is able to tell
that the payload is broken rather than that it has no stores.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Fall back on an unparsable payload, drop a bad entry by name** *(chosen)* | The blast radius matches the fault: a bad entry costs that entry, a bad payload costs the configured set and leaves a demonstrable surface standing. | Two validation behaviors sit behind one variable, so the posture is not readable from the payload alone and a reader has to know which failure they are in. |
| Uniform strict validation | One rule to state and to test; no half-configured state can be served; a bad entry can never be silently absent. | Lost on diagnosability: one malformed entry among nine takes the whole surface down, and the operator sees a dead deployment rather than a named entry. |
| Uniform lenient validation | Never takes a surface down; the maximum that can be served is always served. | Lost on the same criterion from the other side: an unparsable payload yields an empty surface with no built-in to demonstrate against, which reads exactly like a deployment that has no stores yet. |
| Strict validation at deploy time, lenient at request time | Catches both failures while an operator is present, and degrades gracefully afterwards. | Lost on timing: the array is deliberately a runtime variable so a store appears with no rebuild, so a deploy-time gate does not see the edit that introduced the fault. |

## Criteria

1. **Whether a deployment with one broken store can tell which one** — and whether a fresh
   deployment shows something before it has data. **This criterion decided.** Both
   failures here are configuration mistakes made by a person who will have to find them
   from the outside, so the value of each policy is almost entirely in how specifically it
   names the fault; availability differences between the options are secondary because
   every option leaves something serving.
2. **Match between blast radius and fault scope** — one bad entry costing one entry, one
   bad payload costing the configured set.
3. **Whether a working surface survives a configuration failure** — the built-ins make this
   possible at all.
4. **Readability of the policy** — one rule is easier to hold than two. The chosen option
   is worst here.

## Consequences

A deployment degrades along the shape of its fault. A single typo costs a single store and
names it; a broken variable costs the configured set and leaves the bundled fixture
rendering the surface, so an operator can see the product working while they fix the
configuration and cannot mistake a broken payload for an empty one. The built-ins earn
their keep beyond development, since they are what makes lenient fallback a demonstration
rather than a blank page.

The cost accepted: two validation behaviors sit behind one variable. An operator cannot
predict, from the payload alone, whether a mistake will cost one store or all of them —
that depends on whether the mistake happens to break JSON parsing or only a field, which is
not a distinction they were thinking about when they made it. A missing store is also a
quiet outcome: an entry dropped with a diagnostic leaves a surface that looks complete to
anyone not counting, so the diagnostic has to be somewhere an operator actually reads.

## Revisit triggers

- Dropped entries are observed going unnoticed, indicating the per-entry diagnostic needs a
  surface rather than a log.
- A validation step exists that sees the payload at the moment it is edited, which would
  make both failures catchable while the operator is present and collapse the asymmetry.
- The built-ins stop being a meaningful demonstration of the surface, removing the reason
  that lenient fallback beats an empty list.
