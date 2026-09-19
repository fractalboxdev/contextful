# D24 — Delegation is a versioned profile over an attenuable-credential library, bounded in depth and evaluation

**Status:** accepted

## Context

Delegated authority narrows offline: a service derives a per-query child, and a sub-agent derives a grandchild from that. Two wire formats — a library token and a custom envelope with room for one attenuation segment — cannot both carry that chain, and a hand-rolled chain signature is reviewed by nobody outside this project.

## Decision

The attenuable-credential library's own format is the one wire format; this engine owns a versioned profile over it and no envelope of its own.

- `authority.profile` names the exact facts, checks and restriction tuples the engine admits. The library owns serialization, signatures, block chaining and evaluation. An element the profile does not name refuses.
- A credential is an authority block followed by N attenuation blocks. A chain deeper than the profile's declared depth limit refuses before evaluation.
- Current time, audience, resolved resources and request identity are reserved facts the engine supplies on every evaluation; a block introducing a fact into that space refuses.
- The evaluator runs with no third-party block, external function, recursion or regular-expression predicate, under declared fact and iteration ceilings; input past a ceiling refuses, so every checkpoint reaches the same verdict.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Library format, N-block chain, owned profile, depth and evaluation ceilings *(chosen)* | — | The wire format tracks an upstream project; a pattern restriction is written as an enumerated allowlist. |
| A custom envelope beside the library token | Chain depth | One attenuation segment would leave grandchildren inexpressible, and two formats would each need review. |
| A hand-rolled N-block format | Review cost | Its failure mode would be silent forgery reviewed by nobody outside this project. |
| The library's full language, unprofiled | Bounding | Evaluation cost and reachable facts would be holder-controlled. |
| A bearer token plus a policy-service lookup | Read-path independence | Every admission would fail when the policy service does. |

## Consequences

- A breaking upstream format change reaches every credential in circulation; the profile version absorbs it.
- A wall-clock timeout plays no part in admission, so slow hardware refuses nothing fast hardware admits.
- A delegation wanting to carry its own resolved resource cannot express it.

## Revisit

- Real delegation trees reach the depth limit.
- The library adds a backtracking-free matcher identical across native and WebAssembly builds.
