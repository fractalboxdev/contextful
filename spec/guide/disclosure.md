---
contract: disclosure
---

# Visibility, disclosure and accountability

## What it is for

A **Contextful** workspace that indexes an organization's wiki, drive and chat answers
everyone, so it inherits the question those sources already settled: who may see what.
Visibility mirrors source permissions into every read, so a reader sees only what
the source shows them.
Disclosure lets a derived figure cross from many contributors to one asker without
carrying any contributor's rows. Accountability leaves a verifiable trace of every read
and removes a subject or a tenant on demand.

One stance runs through all three: when the evidence behind a decision is missing, stale
or unverifiable, the operation refuses with a typed error instead of returning fewer rows.
An empty answer then means an empty corpus, never a quiet failure.

## How it works

```mermaid
flowchart LR
  PACK["connector pack"]
  subgraph VIS["visibility"]
    SWEEPER["ACL sweep job"]
    AT[("access tables")]
  end
  READER(["reader"])
  subgraph AGG["disclosure"]
    RELJOB["release job"]
    PUB[("published aggregate")]
  end
  subgraph ACC["accountability"]
    CHAINLOG[("audit chain")]
    ERASER["erasure cascade"]
    RCPT["purge receipt"]
  end
  PACK -->|"access mapping"| SWEEPER
  SWEEPER -->|"copied grants"| AT
  AT -->|"reachable rows, within budget"| READER
  READER -->|"recorded read"| CHAINLOG
  RELJOB -->|"suppressed, noised figures"| PUB
  PUB -->|"cohort answer"| READER
  ERASER -->|"signed"| RCPT
  ERASER -->|"erasure entry"| CHAINLOG
```

Visibility runs in three steps. A pack lands a source with an access mapping
({{disclosure.pack.mapping-absent}}) whose keys the mapping schema defines
({{disclosure.pack.asserts-access}}). A sweep copies grants into access tables as ordinary
store data, refusing a grant with no resource ({{disclosure.sweep.orphan-grant}}) and
advancing its coverage watermark only from streams that detect gaps
({{disclosure.sweep.ungapped-stream}}). At read time, reach resolves the admitted subject
through a bounded group closure ({{disclosure.reach.closure-walk}}) to a reachable set, and
the registered view semi-joins content against it. Every served table carries a visibility
block ({{disclosure.mirror.unbound-table}}) naming a sweep and a staleness budget
({{disclosure.mirror.incomplete-binding}}), and a read past that budget refuses
({{disclosure.bound-staleness.access-stale}}).

Fidelity states how closely a table follows its source. The source family caps the claim
({{disclosure.declare-fidelity.family-bound}}), and a source that computes access with its
own sharing engine is queried live rather than mirrored
({{disclosure.declare-fidelity.computed-inputs}}).

Disclosure acts at build time, so every read path inherits it. A
release reserves each contributing unit's budget before computing
({{disclosure.release.budget-reservation}}), groups only on permitted keys
({{disclosure.release.group-key}}), then withholds small groups
({{disclosure.suppress.min-group-size}}) and dominated ones
({{disclosure.suppress.contributor-share}}) behind one sentinel row. A cohort table never
narrows to one person ({{disclosure.bound-cohort.singleton-cohort}}).

Accountability starts at the read: rows leave once the read's own entry is durable
({{disclosure.record.read-entry}}, {{disclosure.record.unpersisted-entry}}); entries hash-link into segments closed by signed
roots ({{disclosure.record.segment}}), and verification reports the earliest break
({{disclosure.attest.broken-chain}}). A header fixes the digest
({{disclosure.record.chain-header}}), each root a Merkle hash
({{disclosure.attest.merkle-root}}), so an audit path proves membership under the
public key alone ({{disclosure.attest.inclusion-proof}}). A keyless node appends unanchored
until anchored ({{disclosure.attest.anchor-verb}}); a held chain stays held
({{disclosure.record.unanchored-over-signed}}); replicas expose truncation
({{disclosure.attest.replica-verify}}). `audit query` answers who read what
({{disclosure.record.reads-view}}), refusals too ({{disclosure.record.refused-read}}).

## Worked example

A wiki page is shared with `planning`, which contains `eng-leads`, which contains Ada.

The wiki pack maps the source's `viewer` level to read, names the `item-exception`
family, and binds the `pages` table as `mirrored` with a budget written in the grammar of
{{disclosure.bound-staleness.budget-grammar}}. The sweep lands one resource row, one grant
to `planning`, and two membership rows.

Ada's question touches `pages`. Reach walks `planning`, then `eng-leads`, then Ada,
subtracts tombstones in force, and caches the set ({{disclosure.reach.cache-capacity}}).
The source lag sits inside the budget, so the semi-join admits the page and the envelope
reports fidelity and lag. The read appends a chain entry carrying a keyed digest of the
statement, and rows return after the fsync.

An editor removes `planning` from the page. The budget bounds how long the old grant
keeps answering. If the sweep stalls past it, Ada's next read refuses instead of serving a
revoked grant, and nothing produced in that state is retained
({{disclosure.reach.degraded-uncached}}).

Asked why Bo cannot see it, explain decides from current grants
({{disclosure.explain.decision}}) along a path naming groups without members
({{disclosure.explain.path}}, {{disclosure.explain.groups-not-members}}), replays a window
({{disclosure.explain.replay}}) under coverage ({{disclosure.explain.coverage}}), lists its
audience ({{disclosure.explain.audience}}), and returns no page content
({{disclosure.explain.no-row}}).

Ada leaves and is erased. The caller holds the forget grant ({{disclosure.erase.privilege}});
the cascade invalidates facts derived from her rows ({{disclosure.erase.cascade}}) or
commits nothing ({{disclosure.erase.cascade-unbounded}}); files holding her rows are
rewritten or collected ({{disclosure.erase.physical-removal}}). A tenant purge returns a signed receipt naming a salted pseudonym ({{disclosure.receipt.pseudonym}})
and claiming no more than its version covers ({{disclosure.receipt.widened-claim}}).

## Where to look

| Question | Operation |
| --- | --- |
| Which table claims which fidelity? | `disclosure.declare-fidelity` |
| How old may permission state be? | `disclosure.bound-staleness` |
| When is a group withheld? | `disclosure.suppress` |
| Why can this person see that page? | `disclosure.explain` |
| What does a purge prove? | `disclosure.receipt` |
