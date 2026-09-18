# 0282 — Receipt version 1 attests rewrite-and-exclude over the canonical store

**Status:** accepted 2026-09-18
**Decides:** `accountability.receipt.refusal.widened-claim`

## Context

A tenant purge hands back a signed artifact. The signature makes whatever the artifact says
checkable offline against a pinned issuer key, with no access to the store — which means the
signature raises the weight of the sentence without raising its truth. A false claim with a
valid signature is worse than a false claim without one, since the holder now has grounds to
repeat it.

What the purge actually achieves is bounded by the substrate. The rewrite covers every file
the read path can reach: committed run files, compaction snapshots, each model build, and the
memory mirror. It does not cover the replication bucket or the replicas fed from it, which
hold erased rows until their next refresh. It does not destroy media: the object interface has
no delete, and snapshots superseded by the rewrite stay in the bucket until expedited
collection reaches them.

Those gaps are properties of the storage layer, not oversights in the purge. A claim of
destruction would be false on the day it was signed and would stay false until the substrate
changed underneath it.

The alternative to a narrow claim is no claim, and that is worse in a specific way. A
consumer with no artifact verifies by querying the store for a count of their rows, and zero
comes back identically from an actual rewrite, a filtering read policy, a tombstone in force,
or a grant that simply does not admit the rows. The query cannot distinguish deletion from
concealment.

## Decision

Version 1 attests rewrite-and-exclude across the canonical store: run files, snapshots, model
builds, and the memory mirror. The `coverage` block names what falls outside the claim — the
replication bucket, replicas, and physical destruction of media — in the artifact itself
rather than in surrounding prose. A holder asserts that the canonical store returns no row of
this tenant, and nothing wider. Widening what a receipt claims moves `receipt_version`, so a
holder reads the reach of the claim off one field, and a coverage block naming a scope broader
than its `receipt_version` declares raises `ReceiptClaimWidened`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A rewrite-and-exclude claim over the canonical store, with exclusions in the artifact** *(chosen)* | Every sentence in the artifact is true when signed and stays true, and the holder can assert it in their own audit without qualification. | The receipt does not answer the destruction-of-media question, and the coverage block has to be read before the counts carry meaning. |
| A broad "data destroyed" claim | Reads the way a compliance reviewer expects, with no further explanation needed. | False on signature day: the object interface has no delete and superseded snapshots stay until collection. A holder asserting it is asserting something the system cannot support, with better provenance than before. |
| Ship no receipt at all | Nothing to keep honest, and no version to manage. | The consumer verifies by querying for a count, which returns zero from a filtering read policy, a retained snapshot or a tombstone alike. Concealment and deletion become indistinguishable. |
| Close the substrate gaps first, then issue a receipt with the wide claim | One artifact, one claim, no versioning and no coverage block. | Sequencing: the narrow claim is correct today and the wide one is a version bump away, so waiting withholds a true artifact for months to avoid publishing a field. |
| Put the exclusions in documentation beside the artifact | Keeps the artifact short and the claim uncluttered. | The artifact is what travels — into a ticket, an audit file, a counterparty's records. Prose that does not travel with it is not part of the claim. |
| Let the coverage block vary freely per deployment | A deployment with stronger substrate can claim more without a version change. | The version field stops meaning anything, and a holder has to read the whole block to learn the reach, which is the comparison problem the version exists to solve. |

## Criteria

1. **Assertability** — whether a holder quoting the artifact into their own audit states
   something true. The broad claim fails this outright.
2. **Discriminating power for the consumer** — whether the artifact distinguishes deletion
   from concealment better than a query does. Shipping nothing fails this.
3. **Travel** — whether the qualification stays attached to the claim wherever the artifact
   goes. Documentation beside it fails this.
4. **Legibility of reach** — whether the holder learns how far the claim goes from one field
   rather than by reading and comparing prose. Free-form coverage fails this.
5. **Completeness of the guarantee** — whether the artifact answers the destruction question
   the reviewer actually asked. This is the criterion the chosen option loses on.

Assertability decides it. A claim wider than the store delivers would be a false assertion
with better provenance, which is a worse outcome than no artifact at all — the signature
converts a hedge into a commitment. Sequencing is what rejects the wait: the narrow claim is
correct now and widening is a version bump, so there is nothing to gain by withholding it.

## Consequences

A receipt holder knows exactly what they hold: the canonical store returns no row of this
tenant, and three named things are outside that. A downstream reviewer who needs media
destruction learns so from the artifact rather than from an assumption.

The accepted cost is that the receipt does not answer the question a compliance reviewer most
often arrives with, so a deployment using it in a regulated context has to close the gap by
other means — bucket lifecycle policy, replica inventory, disk handling — and say so. The
coverage block also has to be read first: the per-table counts of rows removed and files
rewritten are meaningless until the reader knows which stores they range over.

Reversing this is cheap by design. Widening moves `receipt_version`, artifacts already issued
keep their stated reach, and a holder comparing two artifacts reads the difference off one
field. What is expensive is retracting a widened claim later, since artifacts asserting it
are already in counterparties' hands.

## Revisit triggers

- The object interface gains a delete, or bucket lifecycle policy becomes something the engine
  drives rather than something an operator configures, which would let the claim cover
  superseded snapshots.
- Replica refresh becomes acknowledged rather than eventual, so a receipt can name the
  replicas it has confirmed.
- A regime requires an explicit destruction attestation that rewrite-and-exclude does not
  satisfy, pricing the substrate work against losing the use case.
