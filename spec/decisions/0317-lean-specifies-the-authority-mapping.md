# 0317 — The model specifies the profile-to-effective-authority mapping, each target bound to one authenticated query path

**Status:** accepted 2026-09-18
**Decides:** `formal.model.refusal.unmodelled-constructor`, `formal.prove.refusal.sampled-inclusion`, `formal.prove.refusal.unbound-target`

## Context

A mechanized model has to be about something, and the choice of object decides how long the
proofs survive. Authority in this system arrives as a credential in a maintained delegation
profile, is verified, and is mapped to the effective authority a session runs under. That
mapping is where a restriction either survives or is lost: an inherited bound has to stay
attached, and one grant's action has to stay paired with that grant's own tenant rather
than dissolving into a pair of unrelated allowlists.

The profile's own format is maintained outside this system. Its wire encoding, its
cryptography and its parser are somebody else's artifacts under somebody else's versioning.
The mapping from a verified credential to effective authority is ours, and it changes only
when the authority contract changes. A self-owned wire protocol, by contrast, moves with
every correction to itself, and each correction re-opens whatever was proved about it.

Placement is the second half. A caller's declared placement is an inductive with one case
per category the manifest admits and one case for an absent declaration, and an allow-set
is a decidable predicate over that value built from pattern forms parsed outside the model.
Inclusion between allow-sets is the question the enforcement path actually asks, over an
identifier space that is unbounded — a category pattern subsumes every identifier under it
without enumerating any of them.

A theorem is also worth exactly as much as its connection to the running system. A constant
proved about a mapping nobody calls, or about a mapping reached by a path other than the
one a request takes, is a true statement with no reader. The tree has more than one path
that could plausibly be described by the same words.

## Decision

The model specifies the profile-to-effective-authority mapping and carries five proof
targets over it: profile meaning, restriction preservation, narrowing, inclusion, and
execution through the scoped session. Inclusion is decided symbolically, by subsumption
over patterns quantified over every constructor and every identifier; deciding it by
evaluating a fixed collection of example values raises `ZoneInclusionSampled`. A category
the manifest admits with no case in the placement inductive raises
`UnmodelledConstructor` at elaboration, naming it. Each target is tied to one authenticated
query path in the tree, recorded beside the target as a module path and a test, before the
product names it; a target named in the claim with no such binding raises
`ProofTargetUnbound`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The mapping from a maintained delegation profile to effective authority, symbolically decided, each target path-bound** *(chosen)* | A stable proof object, statements about the property that actually governs a read, and a constant a reader can follow to the code path it describes. | The delegation library, its cryptography and its parser become trusted dependencies named in the claim rather than objects of it, so the claim reaches the mapping and not the credential machinery underneath it. |
| Proving a self-owned wire protocol's attenuation rules | The credential machinery itself comes inside the proved perimeter, with nothing below it trusted. | Lost on stability of the proof object: the target moves with every protocol correction, and each correction re-opens the proofs, so the inventory churns on exactly the edits least able to afford it. |
| Proving the whole enforcement engine | The strongest statement anyone would want, and no trusted remainder to name. | Lost on scope. A mediation statement quantifies over an execution relation and proceeds by induction over reachable transitions; no filter-composition theorem supplies that, and the model holds no definition of storage, concurrency or an attacker's observations to build it from. |
| Deciding inclusion by evaluating representative probe zones | A decision procedure that is trivial to write and trivially executable. | Lost on coverage: a finite probe set decides nothing about an unbounded identifier space, so the theorem would assert over the samples and be quoted as if it asserted over the space. |
| Naming proof targets without binding each to a query path | Fewer artifacts to maintain, and targets can be stated before the paths settle. | Lost on relevance: a constant with no binding is a true statement whose subject a reader cannot locate, and the product would name it anyway. |

## Criteria

1. **Stability of the proof object** — whether the thing proved about moves under its own
   maintenance. *This criterion decided it.* Proof effort is front-loaded and the
   inventory is maintained by hand, so an object that moves converts every correction into
   proof work and eventually into a stale inventory nobody trusts. A maintained external
   profile's mapping moves only when our own authority contract does.
2. **Whether the proven statement is something downstream relies on** — whether an
   enforcement decision depends on it.
3. **Coverage of the decision procedure over its input space** — whether the statement
   quantifies over the space or over a sample of it.
4. **Locatability** — whether a reader can follow a target to the path that runs it.
5. **Reach of the claim** — how much of the credential machinery ends up inside the
   perimeter. This one was outranked by stability, and its cost is paid by naming the
   trusted remainder explicitly.

## Consequences

The proofs range over definitions written from the specification text rather than from the
engine's source, so a theorem says something about the specification instead of restating
the code. A category added to the manifest cannot be quietly outside the model — the
missing case fails at elaboration, naming the category. A product statement about a target
cannot outrun the tree, because the binding is recorded before the statement is made.

The accepted cost is the trusted remainder. The delegation library, its cryptography and
its parser sit under the theorems as assumptions, so a defect in credential parsing or
signature verification is entirely outside what any of these constants say. The claim has
to name them, and every quotation of a theorem carries them.

## Revisit triggers

- The delegation profile's maintainer changes the format in a way that alters the mapping's
  domain, which moves the supposedly stable object.
- An execution relation and an induction over reachable transitions become available, which
  makes a mediation statement provable and widens the object beyond the mapping.
- A defect is found in the trusted remainder, which is the case that would argue for
  bringing the credential machinery inside the model despite its instability.
