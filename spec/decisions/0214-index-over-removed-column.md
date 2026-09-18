# 0214 — An index declared over a write-time-removed column is refused at manifest validation

**Status:** accepted 2026-09-18
**Decides:** `enforcement.redact.refusal.index-over-a-removed-column`

## Context

Write-time removal and query-time masking are two different promises about the same
column. A column protected at query time alone sits in cleartext inside the stored bytes,
and an object-store credential reveals it; the protection is a decision made per caller
when rows are served. A column handled at write time is a different claim: a stolen
object-store credential yields columnar bytes with the value already gone. Operators pick
the second for the columns whose exposure would be unrecoverable.

Sidecar structures are separate artifacts under the same prefix, declared in the manifest
beside the Parquet they accelerate, and each one is built from column content. A
full-text segment set holds the column's terms. A vector graph holds neighbour structure
over embeddings of its values, which recovers approximate similarity between rows without
holding a row. A bloom filter answers membership for a value a holder can guess. A zone
map holds per-range minimum and maximum values verbatim.

Put those two facts together and the declaration becomes a contradiction stated in one
file. The manifest says this column's values are removed from the data, and it says a
structure derived from those values sits beside the data. Whoever reads the columnar
bytes with a stolen credential reads the structure too.

The manifest is also the one place both halves are visible at once. The removal rule is
declared on the pipeline; the index is declared on the table; neither declaration, read
alone, shows the problem. A reviewer looking at either file sees a reasonable request.

## Decision

Declaring a vector graph, a full-text segment set, a bloom filter or a zone map over a
column removed at write time raises `EnforceIndexOverRemovedColumn`, at manifest
validation. An index built from the original values reconstructs what the rule took out,
so no such structure exists on disk in any form. An operator needing both ranked retrieval
and protection on one column chooses a query-time mask and accepts cleartext at rest, or
chooses removal and loses the index.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the declaration at manifest validation** *(chosen)* | The at-rest guarantee holds across every artifact under the prefix; the contradiction is caught where both halves are visible | A column needing both storage-level removal and ranked retrieval cannot have both, and the operator picks one |
| Build the index after removal, over the substituted values | Ranked retrieval survives; the structure carries no original content | Loses on utility for the vector and full-text cases, where a keyed digest or a null carries nothing to embed or to tokenize, so the index is present and answers nothing |
| Encrypt the index under the table's key and allow it | Both properties, as long as the key holds | Loses on scope: the guarantee is stated against a leaked storage credential, and encryption answers that only while the key is uncompromised, which is a weaker claim than absence |
| Warn at validation and build the index anyway | Nothing is blocked; the operator is informed | Loses on where the contradiction is visible: the manifest is the one place both declarations appear, and a warning there is the same silent defeat the removal rule was declared to prevent |

## Criteria

1. **Whether the at-rest guarantee survives a stolen object-store credential.**
   *(decided it)* The others weigh feature coverage against strictness. This one is the
   guarantee the removal rule exists to provide, stated against a named threat. A
   structure derived from pre-removal values reconstructs the content while sitting next
   to the bytes the rule cleaned, so the guarantee is void for as long as the structure
   exists — not degraded, void.
2. **Whether the surviving alternative is useful.** Post-removal indexes fail here for
   the two retrieval shapes that matter most.
3. **What the protection claim is made against.** Absence and encryption are different
   claims, and the removal rule's claim is absence.
4. **Whether the operator retains a workable path.** They do: query-time masking with an
   index, at the cost of cleartext at rest, declared knowingly.

## Consequences

The manifest becomes the place a protection contradiction is caught, before any bytes are
written, rather than during an incident review. Every artifact under the prefix carries
one story about a removed column: it is not there.

The cost accepted is an exclusion. A column that genuinely needs both storage-level
removal and ranked retrieval — a name field that must be absent at rest and searchable by
analysts — cannot have both, and the operator makes a trade they may not want to make.
There is no partial answer offered: no coarse index, no encrypted structure, no
opt-out.

Relaxing this later would not repair the tables that ran under the relaxation, because the
structures written during it persist and reconstruct the values.

## Revisit triggers

- An index construction appears that provably carries no information about the original
  values while still serving ranked retrieval, making the utility objection to
  post-removal indexes obsolete.
- Operators are observed abandoning write-time removal entirely in favor of query-time
  masking on columns that warrant removal, indicating the exclusion is pushing data the
  wrong way.
- The threat model changes so the stated guarantee is no longer against a bare storage
  credential, at which point encryption under the table key becomes a comparable claim.
