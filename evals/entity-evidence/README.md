# Entity and evidence benchmark

`v1.json` holds 72 fixed synthetic cases across eight independent entity groups. `corpus/` contains canonical identities, claims and source evidence under CC0-1.0. Each case records adjudicated canonical IDs, ordered claim tuples and evidence references independently of the read results. Alias collisions, similar names, two-hop relationships, contradictory live claims, missing evidence and absent entities have explicit truth labels. Later, expired, superseded and inaccessible distractors receive no answer credit. One claim, `7-live`, rests on two evidence rows, so two cases adjudicate a two-citation set.

```sh
contextful eval entity-evidence \
  --manifest evals/entity-evidence/v1.json \
  --baseline evals/entity-evidence/v1.baseline.json \
  --report entity-report.json \
  --public-key <issuer-public-key> \
  --audience <credential-audience>
```

`CONTEXTFUL_TOKEN` carries a read credential for `memory/*` and `evidence/*`, with agent `agent://entity-eval`. The evidence table's row policy admits that agent's source rows. The command lands and folds the corpus in a scratch store; knowledge cards resolve the structured arm's mention, and keyed recall supplies each requested relation step. The ordinary arm calls ranked lexical retrieval at the manifest's `k`, with an evidence-ID filter applied before ranking. The evidence table declares `retrieve.text = ["text"]`, so ranking scores each note's text and nothing else; an undeclared table scores only the engine's default snippet columns, which hold no note text. Both arms receive the same eligible evidence set, computed through keyed recall under the same admitted reader and case anchor.

The deterministic ordinary reader decodes subject, predicate and object tuples from retrieved source rows and composes the requested relation steps. Ranked rows that decode one tuple form one claim citing every such row in rank order. The reader receives identities whose subjects occur in those rows. The benchmark measures retrieval and relationship composition; it measures neither free-text extraction nor model synthesis. Missing steps, ambiguous identities and contradictory successors produce explicit abstention reasons and no partial answer.

The report separates entity accuracy, eligible-claim recall, ordered path correctness, citation resolution and appropriate abstention. Claims score against fixed adjudicated tuples, so a valid claim omitted by the engine reduces recall. A claim earns credit when its tuple matches an adjudicated fact and its citation set equals that fact's complete set in any order; a proper subset earns no claim or path credit, while citation resolution grades each cited reference on its own. A matching claim ID with a forged tuple, an excluded citation, or a broken path receives no answer credit. The report carries no model judgment; a model judgment occupies a separate label when supplied by a consuming experiment.

Both arms score 1.0 on every metric over v1: entity accuracy, path correctness and abstention over 72 cases, claim recall and citation resolution over the 32 answerable cases. The fixture's note text spells out every tuple, so text-scored ranked retrieval at `k` recovers the same claims that keyed recall supplies, and structured memory holds no advantage on it. The five structured gates hold at 1.0; the five ordinary trend entries record a move in either direction. A fixture whose evidence text leaves the tuple implicit separates the arms; v1 does not.

The baseline envelope binds the fixture version, exact manifest SHA-256, corpus SHA-256 and run configuration. Corpus hashing concatenates each sorted relative file path, a NUL, its bytes and a NUL. The envelope's `metrics` carry each arm's mean as a zero-band `value` and its sample count as `.n`, and `summary` repeats the report's per-metric means and counts; a report regenerates it after any fixture or corpus change. Baseline comparisons reuse `contextful-eval`'s metric paths, minimum sample size and dead-band comparison. A mean below 30 samples remains visible in the summary and receives only a sample-count comparison; every v1 mean has at least 32 samples.

`evals/ledger.toml` tracks five structured gate entries and five ordinary trend entries. The CLI integration test runs the committed baseline, asserts that an answerable ordinary case resolves its exact entity, claim and path, holds both arms to the envelope's means and counts, and emits all ten records through `contextful-eval::record::emit`, so `contextful-ci measure` and default-branch measurement history carry the same figures. Baseline and fixture changes require a versioned envelope with matching provenance.
