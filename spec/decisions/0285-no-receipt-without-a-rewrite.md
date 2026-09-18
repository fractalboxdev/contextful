# 0285 — A purge with no rewrite engine available is refused rather than receipted

**Status:** accepted 2026-09-18
**Decides:** `accountability.receipt.refusal.absent-rewrite-engine`

## Context

A tenant purge is a columnar rewrite. Every file the read path can reach is read, filtered
under the purge predicate and written back, and that work is done by an engine the deployment
supplies rather than by the erasure verb itself. A deployment can therefore reach the purge
path in a configuration where the rewrite cannot run at all: the engine is absent, the build
excludes it, or it fails to initialize.

What the verb produces in the normal case is a signed artifact. The signature is checkable
offline against a pinned issuer key, which means that whatever the artifact says travels with
the weight of the signing key behind it, into a counterparty's audit file, unchallenged.

That weight attaches to anything the verb prints, not only to the signed block. A consumer who
asked for erasure and received output from the erasure command reads it as evidence that
erasure happened; the distinction between a signed receipt and an unsigned report of intent is
one a careful reader draws and a filing system does not.

The ledger complicates the timing. A row opens ahead of the first tombstone, so by the time
the rewrite is attempted and found unavailable, the request already exists in the durable
record as in flight.

## Decision

Where the columnar rewrite engine is unavailable, the purge is refused with
`ReceiptWithoutRewrite` rather than printing an artifact it could not truthfully sign. No
artifact of any kind is produced — signed or otherwise — and no tombstone stands in for the
rewrite. The open ledger row records a request that did not complete, which is what an
interrupted operation looks like everywhere else in this contract.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse with `ReceiptWithoutRewrite`; produce nothing** *(chosen)* | Every artifact the verb has ever emitted corresponds to a rewrite that ran, so a holder never has to ask which kind of output they have. | A deployment without the rewrite engine has no tenant erasure path at all, and the refusal arrives after the ledger row has opened. |
| Print an unsigned report of what would have been rewritten | The operator sees the scope and can plan; nothing is falsely signed. | Lost on what any output is read as: a consumer reads any purge output as evidence. The distinction between a plan and an attestation survives one careful reader and no filing system. |
| Tombstone in place of rewriting, and receipt that | The tenant's rows stop being served, and the caller holds an artifact. | Lost on truthfulness of the signature: the coverage block would claim rewrite-and-exclude across the canonical store, which is false: the values stay in the bytes and a bucket credential reads them. A signed false claim is the outcome this contract is built to prevent. |
| Queue the purge for a later run when the engine is available | The request is not lost, and the work happens eventually. | Lost on whether the caller gets a terminal answer: the caller holds no artifact and no completion signal, so a deadline-bearing request is neither done nor refused. An unbounded queue under a legal clock is a worse position than a refusal. |
| Fall back to a row-level delete path where the format allows one | Some deployments would get a real erasure with no rewrite engine. | Lost on predicate singularity: a second implementation of the purge predicate, which is exactly the divergence the predicate's single definition exists to prevent, introduced on the least-exercised path. |

## Criteria

1. **Truthfulness of the signature** — whether a signed artifact can be issued for work the
   engine did not perform. Tombstone-and-receipt fails this outright.
2. **What any output is read as** — whether an unsigned artifact is safely distinguishable
   from a signed one in the hands of a consumer. The unsigned report fails this.
3. **Whether the caller gets a terminal answer** — done, or refused, with no third state.
   Queuing fails this.
4. **Predicate singularity** — whether the purge's filter has one definition. The fallback
   path fails this.
5. **Deployment reach** — whether every deployment has an erasure path. This is the criterion
   the chosen option loses on.

Truthfulness of the signature decides it over partial progress. The other criteria describe
how a weaker output would be misread; this one describes whether it could be issued at all.
A signing key's value is entirely in the fact that nothing false has ever been signed with it,
and a single receipt attesting a rewrite that did not run devalues every receipt the
deployment has issued, retroactively and irrecoverably.

## Consequences

The set of purge receipts in existence is exactly the set of purges that ran, so a holder
needs no side channel to establish which kind of output they have. Operators learn about a
missing rewrite engine at the point of the purge, loudly, with a named error.

The accepted cost has two parts. A deployment assembled without the rewrite engine has no
tenant erasure path — not a degraded one, none — and discovers that when a request arrives
under a deadline rather than at configuration time. And the refusal arrives after the ledger
row has opened, so the durable record carries an in-flight request that never completed and an
operator has to read the error to know why.

Reversing this toward leniency is cheap in code and costly in trust: once one deployment has
issued an artifact for work that did not run, no reader can take the claim at face value
without asking which version produced it.

## Revisit triggers

- The purge path validates the rewrite engine's availability at startup or at configuration
  time, moving the discovery off the deadline-bearing request.
- An erasure mechanism that is genuinely equivalent to the rewrite — one that leaves nothing
  readable through the bucket — becomes available for a format the rewrite engine does not
  cover.
- A receipt gains a field distinguishing the mechanism that produced it, at which point a
  weaker but honest claim becomes expressible rather than impossible.
