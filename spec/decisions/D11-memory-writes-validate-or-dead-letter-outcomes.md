# D11 — Memory writes validate or dead-letter; outcomes settle under their source

**Status:** accepted

## Context

Extraction turns model output into rows. A response that does not match the schema, an edge type nobody declared, or a mention matching two identities has no correct row, and a guessed one propagates through every claim built on it. Predictions scored later need a settlement rule fixed at registration, or a claim settles under whichever evidence arrives.

## Decision

- `read.synthesize` validates every candidate against the deployment's declared output schema, feeds a failure back with its error for up to 3 attempts per batch, and on exhaustion writes the response, template hash and drop reason to the dead-letter table with the cursor held. No free-text path turns an unvalidated response into a row.
- The relation vocabulary is a reserved core plus the deployment's declared types, closed within the deployment; an undeclared edge is dead-lettered and the rest of the batch lands.
- `read.resolve-entity` dead-letters an ambiguous mention and an edge whose endpoint resolves to nothing, without guessing or staging an identity.
- `read.settle` requires exactly one resolution form and one source, `metric`, `adjudicator` or `manual`. A metric comparator is stored verbatim and evaluated outside the engine. An observation settles under its registration's source, and a judgment verdict carries an `http` or `https` citation projected onto the label view. An outcome derived from the store's own rating carries a null verdict. The scored and unresolved views partition the join: one negates the other's predicate verbatim, every clause null-guarded.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Validate, dead-letter, settle under the registered source *(chosen)* | — | Dead-lettered candidates wait for a human, and extraction spends up to 3 model calls per batch. |
| Parse a non-conforming response best-effort | Provenance | Unvalidated text becomes a row nothing can trace to a schema. |
| Resolve an ambiguous mention to the highest score | Reversibility | A wrong merge propagates through every claim about both entities. |
| Evaluate metric rules with an embedded expression engine | Attack surface | Anyone registering a prediction reaches the evaluator. |
| Exclude self-rated outcomes in each reader | Repetition | The first consumer that forgets the filter reports inflated calibration. |

## Consequences

- Every memory row traces to a schema-valid candidate or a named write.
- No outcome row is absent from both label views.
