# D41 — The hash chain is the attestable record

**Status:** accepted

## Context

An auditor asks two questions of a deployment: what was read, and did anything call out. Telemetry answers neither with evidence, because a collector drops a span with nothing proving it and a payload's anonymity is a claim, not an observation.

## Decision

Evidence comes from the data and the network trace, never from a claim a third party or the operator makes.

- `disclosure.record` appends each read to a linked hash chain, genesis written only at project init. Concurrent appends share one fsync, and rows return only after the fsync covering their entry; an unpersisted entry raises `AuditEntryUnpersisted`, returns no rows, and pops the in-memory tip. The query digest is HMAC-SHA256 under the project audit key.
- A disagreeing digest, a sequence gap, or an absent chain beside a tip or signed root raises `AuditChainBroken` at the earliest bad index. Verification runs offline against a pinned issuer key whose scheme decides the signature check.
- `disclosure.attest` signs one root per 4096-entry segment and replicates roots off-node every 10 min; an external transparency log is off by default.
- Telemetry is a projection with no durability obligation, on stderr unless an endpoint is configured; the update check is opt-in.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Linked chain under group commit, refuse on unpersisted entry, per-segment signed roots *(chosen)* | — | Audit storage is an availability dependency of reads; no field data on versions or errors. |
| Telemetry spans as the record | Detectability | A dropped span would be indistinguishable from a quiet period. |
| Fail open with a gap marker, or buffer and retry | Evidential completeness | Rows would leave before their entry exists. |
| An external transparency log as the primary record | Air-gapped operability | Some deployments cannot reach a third party. |
| A plain SHA-256 over the statement | Pseudonymity | A digest over a guessable statement reverses by dictionary. |

## Consequences

- A chain gap has exactly one cause, so an auditor's verdict carries weight.
- A compromised node cannot truncate history that replicated roots already cover.
- Deleting the chain restarts nothing: an absent chain beside its tip or a root reads as a break, never a fresh genesis.

## Revisit

- Audit writes replicate to a second store, removing most of the availability cost.
- Group commit sustains less append throughput than the reference read rate.
