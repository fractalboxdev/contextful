# 0064 — An unresolvable mention or edge endpoint is dead-lettered rather than guessed

**Status:** accepted 2026-09-18
**Decides:** `memory.resolve-entity.refusal.ambiguous-mention`, `memory.resolve-entity.refusal.edge-endpoint`

## Context

Resolve maps candidate mentions onto canonical identities by alias, by embedding
similarity and by deterministic keys, then tags each candidate claim with the subject it
resolved to. Similarity is a score; aliases and deterministic keys are not. When two
canonical identities match a mention and no deterministic key separates them, the matcher
holds two candidates and a pair of numbers.

What the score means matters less than what a wrong pick does downstream. `entity_id` is
opaque and is stamped onto the claim row, so a claim resolved to the wrong identity becomes
a fact about the wrong thing. It joins that identity's knowledge card, it is returned by
recall for that subject, edges written in the same pass point at it, and a later
consolidation deduplicating on a key that includes the canonical subject folds further
candidates onto the same wrong identity. Unpicking it means finding every claim, edge and
card entry that flowed from the mistake, across passes that ran afterwards.

A missing resolution behaves differently. The candidate goes to the dead-letter table with
its mention text, and the correction is an edit to the deployment's alias sets followed by
a re-run of the same batch. The claim is late rather than wrong, and nothing downstream
consumed it in the meantime.

Edge endpoints have the same shape with a narrower failure. An edge is an explicit typed
row written only when synthesis resolves both endpoints, and nothing infers an edge while a
query runs. An edge whose source or target resolves to no identity has nowhere to point;
writing it with a staged placeholder would put a row into the graph that a traversal walks.

## Decision

A mention matching two canonical identities with no deterministic key to separate them is
recorded as an ambiguous skip, raising `MemoryEntityAmbiguous`, and the candidate claim
carrying it is dead-lettered. A candidate edge whose source or target resolves to no
identity is dead-lettered and raises `MemoryEdgeEndpointUnresolved`. Neither path writes a
row under a guessed identity, and neither path stages an identity to absorb the mention.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Dead-letter the candidate, record the skip, leave the alias sets to a human** *(chosen)* | No wrong merge ever enters the claim, edge or card population; the correction is an alias edit plus a re-run | Recall misses claims a human would have resolved, and the dead-letter table needs periodic review with alias edits to drain it |
| Resolve to the highest-scoring candidate | Every candidate lands; no operator loop; recall coverage is maximal | Lost on reversibility: a wrong merge propagates through every claim, edge and card about both things, and unpicking it is unbounded work over passes that already ran |
| Stage a new identity per ambiguous mention | Nothing is dropped and nothing is merged wrongly; the mention keeps its own row | Lost on fragmentation: the entity table fills with near-duplicates, and every one of them is a subject recall now ranks separately |
| Resolve on score above a confidence threshold, dead-letter below it | Coverage where the matcher is sure, safety where it is not | Lost on reversibility again, at a lower rate: the threshold sets how often a wrong merge happens, not whether one can, and no threshold makes the unpicking cheaper |

## Criteria

1. **Reversibility of the failure** — the work to undo a wrong outcome, against the work to
   complete a missing one.
2. **Population hygiene** — whether the entity table stays a set of things rather than a set
   of mentions.
3. **Coverage** — how many candidates reach a claim row without human involvement.
4. **Operator load** — recurring work the rule creates.

Criterion 1 decided it, and it is asymmetric in a way the other three are not. A missing
claim costs one re-run once the alias is added; a wrong merge costs a search over every
artifact derived from both identities since the merge, with no marker saying where to look.
Criterion 3 loses, and the threshold option shows why that loss cannot be bought back: a
threshold trades the rate of the irreversible failure against coverage without changing its
cost when it happens.

## Consequences

The entity population stays trustworthy enough that a knowledge card can be read as a
statement about one thing. Traversal over `memory_edges` walks rows whose endpoints were
each resolved, so a multi-hop result does not carry an invented hop.

Recall misses claims. A mention a human would have disambiguated in a second sits in the
dead-letter table until somebody adds the alias, and nothing in recall says the claim was
dropped rather than never extracted.

The dead-letter table is standing operator work. It drains only through alias edits and
re-runs, and a deployment that never reviews it silently loses a fraction of every
synthesis pass. Nothing in the rule bounds how large that fraction gets.

Loosening the rule later is cheap in code and expensive in data: a deployment that starts
resolving by score inherits every merge decision that score makes, and the claims written
before the change stay correct while the ones after it are only as good as the matcher.

## Revisit triggers

- A deterministic key with wide enough coverage that ambiguity becomes rare rather than
  routine, which changes the coverage cost without changing the reversibility argument.
- A merge-undo path that can find and retract every artifact derived from one identity,
  which is the mechanism the reversibility criterion assumes does not exist.
- Dead-letter volume per pass rising to a level where review is not performed at all, which
  means the rule is dropping claims rather than deferring them.
