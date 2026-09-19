# D51 — The model specifies the profile-to-effective-authority mapping, each target bound to one query path

**Status:** accepted

## Context

A mechanized model has to be about an object that survives its own maintenance. A restriction survives or dies where a verified credential maps to the effective authority a session runs under. The credential's wire format is maintained outside this system; the mapping changes only when the authority contract does. A theorem about a mapping no request reaches is true and unread.

## Decision

`assurance.model` specifies the mapping from the maintained delegation profile to effective authority, with five targets: profile meaning, restriction preservation, narrowing, inclusion, and execution through the scoped session.

- `assurance.prove` decides placement inclusion symbolically, by subsumption over patterns quantified over every constructor and identifier; deciding it over sample values raises `ZoneInclusionSampled`.
- A manifest category with no case in the placement inductive raises `UnmodelledConstructor` at elaboration.
- Each target binds to one authenticated query path, recorded as a module path and a test before the product names it; an unbound target raises `ProofTargetUnbound`.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| The profile-to-authority mapping, symbolic, path-bound *(chosen)* | — | The delegation library, its cryptography and its parser are trusted dependencies the claim names. |
| A self-owned wire protocol's attenuation rules | Stability of the proof object | Every protocol correction would re-open the proofs. |
| The whole enforcement engine | Scope | Mediation needs an execution relation and induction over reachable transitions the model lacks. |
| Inclusion decided over probe zones | Coverage | A finite sample decides nothing over an unbounded identifier space. |
| Targets named without a path binding | Locatability | A reader could not find the code a constant describes. |

## Consequences

- Proofs range over definitions written from the specification, not restated code.
- A new manifest category cannot sit silently outside the model.
- A defect in credential parsing or signature checks lies outside every theorem.

## Revisit

- The profile's maintainer changes the mapping's domain.
- An execution relation makes a mediation statement provable.
- A defect surfaces in the trusted remainder.
