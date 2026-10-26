---
contract: disclosure
---

# Visibility, disclosure and accountability

## The problems this contract solves

A **Contextful** workspace indexes an organization's wiki, drive and chat. Three
problems follow. A reader sees only what the source lets that reader see
(**visibility**). A figure computed from many people's rows reaches one asker without
exposing any contributor (**disclosure**). Every read leaves a verifiable trace, and a
person or a tenant is removable on demand (**accountability**).

## Visibility: mirroring source permissions

The example is a wiki page shared with the group `planning`, which contains `eng-leads`,
which contains one reader.

A **pack** is the reviewed unit that lands one source with its access mapping; a table
with no mapping fails at diagnosis ({{disclosure.pack.mapping-absent}}). A **sweep**
reads the source's permission state and commits it as one run
({{disclosure.sweep.sweep-verb}}). Grants land as ordinary rows of reserved
`_visibility` tables ({{disclosure.mirror.grants-table}}).

For the wiki, the sweep lands one resource row, one grant to `planning` and two
membership rows. A grant naming no resource fails at commit
({{disclosure.sweep.orphan-grant}}). A Drive sweep follows every permission page of each
file before its grants replace earlier ones ({{disclosure.drive-sweep.permission-pages}}).

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

At read time, a bounded group closure walks `planning`, then `eng-leads`, then the
reader ({{disclosure.reach.closure-walk}}). The registered view admits a row only when
its resource holds a grant in that reachable set ({{disclosure.reach.semi-join}}).

**Fidelity** states how closely a table follows its source, and the source family caps
the claim ({{disclosure.declare-fidelity.family-bound}}). A federated table, queried live
under a reader's delegated credential, never registers on a read face
({{disclosure.declare-fidelity.federated-registered}}).

## When permission state goes stale

An editor removes `planning` from the page. The table's staleness budget bounds how long
the old grant keeps answering. If the sweep stalls past the budget, the reader's next
read fails instead of serving a revoked grant ({{disclosure.bound-staleness.access-stale}}).

A table declared `public_only` instead narrows to public rows past its budget
({{disclosure.bound-staleness.public-only}}). No reachable set computed in either state
is retained ({{disclosure.reach.degraded-uncached}}).

## Explaining a decision

Asked why a colleague cannot see the page, `audit explain` decides from current grants
({{disclosure.explain.decision}}). The explanation names groups on the path but never
their members ({{disclosure.explain.groups-not-members}}).

## Disclosure: releasing a figure

A release computes statistics over many contributors. Before computing, it reserves
each contributor's spend against a lifetime budget, and a failed reservation publishes
nothing ({{disclosure.release.budget-reservation}}). A group key outside the permitted
columns fails ({{disclosure.release.group-key}}).

A group with too few distinct contributors is suppressed
({{disclosure.suppress.min-group-size}}). Every suppressed group together leaves one
sentinel row with no key or figure ({{disclosure.suppress.sentinel-row}}).

A cohort key resolving to one person fails at declaration
({{disclosure.bound-cohort.singleton-cohort}}). A named query template holds exactly one
statement ({{disclosure.template.single-statement}}). The offline diagnostic checks every
published model's policy from local files ({{disclosure.set-mode.check-verb}}).

## Accountability: the audit chain

Each read appends one audit entry, and rows leave only after that entry syncs
({{disclosure.record.read-entry}}). Entries hash-link into segments, each closed by a
signed root ({{disclosure.record.segment}}).

A proof for one entry verifies offline under the signer's public key alone
({{disclosure.attest.inclusion-proof}}). Verification of a damaged chain fails at the
earliest broken index ({{disclosure.attest.broken-chain}}).

## Erasure

The reader leaves the organization and is erased. Erasure needs the forget grant
({{disclosure.erase.privilege}}). The cascade invalidates every fact derived from the
erased rows in the same operation ({{disclosure.erase.cascade}}), and the files holding
them are rewritten or collected ({{disclosure.erase.physical-removal}}).

A tenant purge returns a signed receipt naming a salted pseudonym, never the tenant
identifier ({{disclosure.receipt.pseudonym}}).
