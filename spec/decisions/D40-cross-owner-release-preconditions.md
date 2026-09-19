# D40 — A cross-owner release runs only when the boundary is enforced below the engine

**Status:** accepted

## Context

A cross-owner setting joins or targets across tenants who do not trust each other. An engine-side check binds only the well-behaved party, and the variable deciding who is disclosed to whom is the candidate pool, not the scoring method.

## Decision

Posture follows the owner boundary a release crosses, and every protection the crossing needs is a precondition checked before the first cross-prefix write.

- `disclosure.release` against a cross-owner store requires per-tenant signed manifest entries, per-tenant write prefixes enforced by the object store's access policy, and per-tenant signing keys in each owner's key store; a store meeting fewer raises `DisclosureCleanRoomPreconditionUnmet`.
- A hashed join keys on a per-pair pepper, re-randomized for the pairing, exchanged through escrow and rotated per join; a static pepper raises `DisclosureStaticPepper`. Matches reach a consumer as an aggregate under a group-size floor.
- A segment releases an identifier, a size, coarse composition cells and an activation handle; member readback refuses, and a segment below its audience floor refuses rather than emitting a sentinel.
- A pool over other tenants' end users is off by default and requires a recorded consent contract bound to the pool.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Store-enforced preconditions, per-pair pepper, activation-only segments, pool-keyed posture *(chosen)* | — | Stores whose policy cannot express per-prefix writes are ruled out; pepper escrow is operational work; a requester cannot audit the segment it activates. |
| Document the protections as operator responsibilities | Detectability | Failure would arrive silently as corrupted data or a forged grant. |
| Accept two of three preconditions | Coverage | Each protection closes a distinct path; the third would stand fully open. |
| A static pepper shared across joins | Pepper compromise | One recovered pepper would reverse every join ever run, in both directions. |
| Posture keyed on the scoring method | Boundary crossing | Identical mathematics over a different pool changes who discloses to whom. |

## Consequences

- A segment is described as size-gated and opaque, not as differentially private.
- The engine verifies a consent contract's presence and binding, not its legal validity.
- A single-tenant pool carries no added posture.

## Revisit

- Private set intersection replaces the pepper once its cost is justified; an attested enclave join serves very-high-risk pairings.
