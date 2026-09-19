# D25 — Grants narrow offline, and admission re-checks the whole chain

**Status:** accepted

## Context

A holder derives children without calling the issuer, so the deriving process is the party whose honesty is in question. Tenant scope is the one dimension whose absence widens reach, and a credential persisted in a run record is readable for the record's whole retention.

## Decision

Every derivation provably narrows its parent, the checkpoint re-proves it over every hop, and each hop is individually withdrawable.

- `authority.grant` table patterns take three forms — bare star, trailing star, exact literal — answered by one implementation for derivation, view registration, tool visibility and replica selection.
- A tenant scope against a table without a bare outermost partition column refuses at admission rather than widening.
- A tenant-scoped child is derived per query with a 15 min lifetime.
- `authority.attenuate` runs the identical narrowing comparison in the deriving process and at admission over the whole chain; a widening child names the dimension that widened. A derivation adds or repeats a tenant scope; dropping or changing it refuses.
- `authority.revoke` keys the denylist on a per-derivation revocation identifier, so withdrawing one hop withdraws its subtree and leaves root and siblings verifying.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Three-form patterns, double narrowing check, per-hop revocation id *(chosen)* | — | Admission pays chain-depth comparison per request; exclusions are written as sets of concrete patterns; the denylist grows per withdrawn hop. |
| Full glob or regular-expression patterns | Containment decidability | Derivation could not decide offline whether a child pattern is contained in its parent's. |
| Check narrowing at derivation alone, or the last hop alone | Trust | A widened intermediate hop would pass unexamined. |
| Ignore an unbindable tenant scope | Failure direction | A scoped-looking credential would read the whole table. |
| One revocation id per root | Blast radius | Stopping one sub-agent would stop every sibling. |

## Consequences

- A credential refuses after a table rebuild drops its partition column, surfacing at the read.
- The consumer's service holds the broader parent in memory and derives on the hot path.
- A scoped parent cannot mint for a sibling tenant without returning to the issuer.

## Revisit

- Admission's chain comparison becomes measurable against request latency.
- A grant dimension carrying a tenant set rather than one tenant.
