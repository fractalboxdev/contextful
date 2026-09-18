# 0254 — A source returning no permission data lands as an ordinary table and claims no fidelity

**Status:** accepted 2026-09-18
**Decides:** `visibility.pack.refusal.unsupported-coarse`

## Context

Sources divide by what they will tell a caller about audience. Some return per-item grant
lists. Some return only the container a row sits in. Some return neither: a public data
feed, a scraped corpus, an internal service whose API has no notion of who may read what.

The envelope a bound read returns carries a fidelity level and a resource grain. Those
fields are a claim made to whoever reads the answer — that the rows were filtered at the
stated grain against permission state observed at a stated instant. A reader downstream,
and an auditor later, weigh the answer by that claim.

The tempting move for a no-permission source is to declare it `coarse` at some nominal
grain, because `coarse` is the weakest level that still serves from the store and every
table then carries a uniform envelope. The claim would be false in a specific way: there
is no container signal behind it, so no filtering happened at that grain or any other.
The envelope would report a grain the source never sent.

The alternative pressure is to refuse such sources outright. That over-corrects. A table
with no audience data is perfectly serviceable under a credential grant, which is how
every table not on an organization-wide face is admitted anyway. What it cannot do is sit
behind a face whose entire premise is per-reader narrowing.

## Decision

A source returning no permission data lands as an ordinary table: no visibility block, no
fidelity claim on the envelope, admission by the credential grant alone, and absence from
any organization-wide face. Declaring `coarse` where the source emits no container or
workspace signal raises `VisibilityUnsupportedCoarse`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **No visibility block, no fidelity claim, credential grant alone** *(chosen)* | The envelope states nothing the source did not send, and the table still serves every reader whose grant reaches it. | The table is absent from the organization-wide face, so its content is missing from the broad questions most readers ask. |
| Declare it `coarse` at a nominal grain | One uniform envelope shape across every table; no special case in the pack schema. | Loses on claim fidelity: the envelope promises a container signal that was never received, and a reader weighing the answer weighs a fiction. |
| Refuse the source at pack review | No ambiguous tables in the deployment at all. | Loses on usefulness: the table is admissible under a credential grant like any other, and refusing it removes material for no gain in accuracy. |
| Add a fourth fidelity level meaning "no signal" | Keeps the envelope uniform while staying honest. | Loses on where the fact belongs: the level would appear on every envelope of a table that is not audience-governed, restating the absence of a visibility block that is already absent. |

## Criteria

1. **Whether the envelope's claim matches what the source sent.** *This criterion
   decides.* The fidelity fields exist to let a reader and an auditor discount an answer;
   a level asserted over no underlying signal makes every such discount wrong, and does
   so invisibly, whereas every other failure here is a visible gap.
2. **Whether the table remains reachable at all.** A rule that removes usable material to
   buy accuracy it already has is a bad trade.
3. **Uniformity of the envelope across tables.** Real, and given up.
4. **Cost of the special case in the pack schema.** One absent block.

## Consequences

Landing such a source is simple — no mapping, no sweep job, no budget — and its table is
reachable by anyone the credential grant names. What it cannot do is participate in the
answers a whole-organization reader asks, and those are the commonest questions. The
material is present in the deployment and absent from the place people look.

The accepted cost is exactly that asymmetry: an operator who ingests a no-permission
source and then wonders why it never appears in answers is looking at working behavior.
The absence is stated in the pack, not in the answer, so it is discoverable only by
reading configuration.

Reversing is cheap where a source later gains a permission API — the table takes a
mapping and moves onto the join. Reversing by relaxing the refusal is not, because every
envelope emitted under the relaxed rule carries an unverifiable claim.

## Revisit triggers

- A source adds container or workspace metadata, which moves its table onto the allowlist
  or the mirrored join.
- Operators are observed working around the refusal by manufacturing a constant container
  identifier, which would mean the refusal is being satisfied rather than respected.
- The organization-wide face gains a way to include a table under an explicit, reader-
  visible statement that it is ungoverned.
