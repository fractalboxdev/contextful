# 0179 — Time, audience, resolved resources and request identity are engine-supplied facts

**Status:** accepted 2026-09-18
**Decides:** `authority.profile.refusal.reserved-fact`

## Context

The evaluator decides a credential against a set of facts. Some of those facts come from
the credential — what the holder was granted, what it narrowed to, which tables it named.
Others are ground truth about the request being decided: what time it is now, which
deployment is being addressed, which rows the query actually resolved to, and who the
authenticated caller is.

The credential is attacker-controlled input. It arrives from whoever is presenting it, it
can be assembled from scratch, and the attenuation model deliberately lets any holder
append blocks to it offline. The whole construction is safe because appending narrows:
a holder can add restrictions and facts, and none of it produces authority the parent did
not have.

That safety is a property of what a block can say, not of the signature over it. A block
carrying its own current time is signed just as validly as one carrying a table
restriction — and it decides its own expiry, because the expiry check is a comparison
against the clock fact the evaluator reads. The same holds for the rest: a block carrying
its own audience defeats the cross-store replay check, a block carrying its own resolved
resources answers the row-restriction check against rows it chose rather than rows the
query reached, and a block carrying its own request identity is the caller stating who the
caller is.

These are the facts every check is decided against. A holder that supplies them is a
holder deciding its own authorization.

## Decision

Current time, deployment audience, resolved resources and authenticated request identity
are reserved facts the engine supplies. A token block introducing a fact into that space
raises `ProfileReservedFact`. The evaluator reads each of them from the engine and from
nowhere else, on every evaluation, so the checks written against them compare a
credential's claims to ground truth rather than to itself.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Reserve the space; a block introducing a fact into it refuses** *(chosen)* | Every check is decided against a fact the engine produced, and the reservation is checkable against the profile without semantic analysis | A delegation pattern wanting to carry a resolved resource of its own cannot express it |
| Let a block carry its own clock or audience | Nothing to reserve, nothing to refuse, and offline testing becomes trivial | Lost outright on the threat: a holder decides its own expiry and its own audience, which removes the two bounds a stolen credential is contained by |
| Namespace token facts apart from engine facts | Both sides coexist; a block says whatever it likes in its own space | Lost on review cost: the separation holds only while no check reads across it, so every profile version pays a full audit of every predicate to confirm none does |
| Prefer the engine's fact where both are present | Ground truth always wins; nothing needs refusing | Lost on the same review cost, plus shadowing: a check the engine does not supply a fact for silently falls back to the block's, and which checks those are changes with each profile version |
| Reserve time and audience only | Covers the two clearly attacker-useful facts with a smaller rule | Lost on completeness: resolved resources and request identity are what row restrictions and identity checks are decided against, so leaving them open leaves those checks self-answered |

## Criteria

1. **Whether attacker-controlled input can assert the ground truth its own authorization
   is decided against.**
2. **Review cost per profile version** — how much analysis a new predicate requires to
   confirm it is safe.
3. **Expressiveness** — which legitimate delegation patterns lose a way to say something.
4. **Rule size** — how much of the fact space is reserved.

Criterion 1 decided it. It is the criterion the construction exists to satisfy, and the
two options that would recover expressiveness both leave it partly unmet. Criterion 2
then decides between refusing and namespacing, which are otherwise close: refusing is a
property of the profile's declared fact space and is checkable by reading the profile,
while namespacing is a property of every predicate's behavior and must be re-established
each time a predicate is added. A guarantee that has to be re-proved on each version is a
guarantee that will eventually not be.

## Consequences

Expiry, audience matching, row restrictions and identity checks each compare against a
fact the engine produced, so a credential cannot answer its own question. The reserved
space is a list in the profile, which means a new predicate is reviewed against a written
boundary rather than against an argument about which facts it happens to read.

The cost accepted is that a legitimate delegation pattern wanting to carry a resolved
resource of its own has no way to express it. A holder that knows, at derivation time,
exactly which rows a child should reach must express that as a restriction the engine
evaluates against the resources it resolves, rather than as an assertion of what those
resources are — which is more verbose and cannot express anything the engine's own
resolution does not already produce.

Reversing the reservation is not a local change: every check written against a reserved
fact assumes it is ground truth, and relaxing the space re-opens each of them.

## Revisit triggers

- A delegation pattern arrives whose requirement cannot be expressed as a restriction over
  engine-resolved resources, and which is common enough to justify the review cost.
- A new check is written that needs a fact belonging to neither side clearly, which means
  the boundary the profile declares no longer partitions the space.
- Offline evaluation without an engine becomes a requirement — a tool reasoning about a
  credential with no request to decide — which would need a supplied fact set that is not
  a running engine's.
