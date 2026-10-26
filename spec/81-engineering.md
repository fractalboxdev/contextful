---
contract: assurance
owns:
  - structure-tree
  - test
  - build
  - evaluate
  - baseline
  - backtest
  - forecast
---

# Engineering gates and the quality harness

How the tree is written, built and checked: one home per capability, test placement, build
targets and artifacts, and the harness that turns read-path quality into a red or green
verdict. The gate that runs these checks, the target ledger and the release tag live in
`spec/83-gate.md`.

The tree, the checks over it, and the two verdicts they produce:

```mermaid
flowchart LR
  SRC["source tree"] -->|"three profiles"| BUILD["build pipeline"]
  SRC -->|"one binary per crate"| TESTS["test suites"]
  AUTO["typed subcommands"] -->|"stage commands"| GATE["CI gate"]
  BUILD -->|"built binaries"| GATE
  TESTS -->|"test results"| GATE
  FORMAL["formal model"] -->|"proof check"| GATE
  GATE -->|"one check per stage"| PR["pull request"]
  BUILD -->|"archive, checksum, SBOM, image"| ART["release artifacts"]
  subgraph read["read contract"]
    STORE[("real store")]
  end
  CASES[("evals/cases JSONL")] -->|"cases"| EVAL["quality harness"]
  STORE -->|"ranked retrieval"| EVAL
  EVAL -->|"scores"| BASE{"within baselines and floors?"}
  BASE -->|"yes"| GREEN["green verdict"]
  BASE -->|"no"| REDV["red verdict"]
```

## structure-tree

One home per capability, the one enforcement decision module, adapters, mirror annotations and derived-artifact checks.

- `one-home` — Enforcement, control-plane state, credential resolution, statement guarding and visibility each resolve to one implementation in the engine; a surface reaches one through the engine route or the registered view compiling it in.
  *P5*
- `duplicated-capability` — A surface or crate carrying a second implementation of an engine capability, in any language, or a hand-written copy of an engine constant, raises `CapabilityDuplicated`, naming both sites.
  *P5*
- `capability-registry` — `spec/capabilities.toml` names each capability of {{assurance.structure-tree.one-home}} with one home path and one or more defining symbols; an absent registry, a missing or doubled capability, an absent home or an undefined symbol raises `CapabilityHomeUnresolved`.
  *A-assurance#duplicated-capabilities-match*
- `bound-constant` — A `[limit]` entry whose {{corpus.state.limit-const}} names `<file>::<NAME>` holds a constant that file defines outside tests at the bound, in its unit or another unit of its kind; a mismatch or an absent constant raises `BoundConstantDrift`.
  *because the fragment is the one home of a bound's value, and a constant drifting from it changes behavior while the clause states the old number*
- `cross-language` — The crate-graph stage matches Rust, TypeScript, JavaScript and Python functions, types and constants outside a home by name folded to lowercase without separators, against its symbols and its literal constants' names and values.
  *A-assurance#duplicated-capabilities-match*
- `decision-module` — One module owns the enforcement decision; a gateway and the engine reaching the same verdict execute it, compiled native and to WebAssembly from one pinned source.
  *P5, A-assurance#decision-module-compiles*
- `adapter` — A surface with no engine process on its request path implements an adapter against the engine's ports.
  *P5*
- `derivation-check` — A generated artifact declares the check that fails once its source moves, and the gate's schema stage runs that check.
  *P5, A-assurance#generated-artifacts-prove*
- `mirror-exemption` — A review-time restatement of a build-time rule is admitted where its site carries a `mirrors: <clause id>` comment naming the clause it restates; an unannotated restatement stays {{assurance.structure-tree.duplicated-capability}}.
  *because a reviewer-facing check sometimes restates a rule, and the annotation keeps the engine its one home and the copy traceable to it*
- `mirror-unresolved` — A `mirrors:` comment in a tracked file under `crates/`, `tools/` or `apps/` naming no clause of `spec/spec.lock.json` raises `MirrorUnresolved` in the schema stage, naming the file and line.
  *because an annotation naming nothing exempts a copy while tying it to no rule it could drift from*

## test

Assertion construction, guard validation, suite placement, test-first and acceptance tests, and the connector test kit.

- `presence-before-absence` — A test whose central claim is an absence first constructs the condition it names and verifies that condition obtains.
  *P7*
- `vacuous-exclusion` — An exclusion assertion — `all`, or a negated `any` — in a tracked test file under `crates/` or `tools/`, over a collection no earlier statement of its function shows non-empty, raises `VacuousAssertion` in the test-first stage.
  *P7*
- `guard-fires-both-ways` — A guard is validated against the fixture that motivated it and against the state it stands against, passing on the first and failing on the second.
- `one-integration-binary` — A crate's integration tests compile in one target, `tests/integration/main.rs`, which declares each suite as a module selected by path.
  *because each file directly under `tests/` links its own static binary, hundreds of MiB for an engine-linked crate*
- `feature-gated-suite` — A feature-gated suite carries a module-level `cfg` attribute at the head of its module file and compiles to nothing while its feature is off.
- `global-state-lock` — A suite contending process-global state acquires a named lock the integration binary's root module owns; every suite touching that state takes the same lock.
  *because a lock declared inside one suite leaves another suite free to race the same state*
- `own-process` — A test needing its own process sits in a top-level file stating why at its site; a thread-scoped log capture is such a test.
- `connector-kit` — The connector authoring toolkit ships a conformance suite — discovery returns valid schemas, an opened table yields a finite stream, a position round-trips — plus recorded-HTTP fixture replay and property tests over position monotonicity.
- `test-first` — A change altering Rust source under `crates/` or `tools/` adds or alters a test under a package's `tests/` that fails against the base commit's source; a change without one raises `TestNotFirst`.
  *A-assurance#source-change-carries*
- `test-first-scope` — The test-first stage builds each changed test file's target against the base source with every feature enabled, then runs exactly the tests under that file's top-level module; a target failing to compile there counts as failing.
- `base-evidence` — Test-first reports assertion failures, compilation failures and bounded test executions separately; infrastructure faults follow {{assurance.test.base-unrunnable}} and supply no failing-test evidence.
  *A-assurance#gate-repair-evidence*
- `base-run-bound` — One test-first execution against the base, its build excluded, runs for at most 300 s; a run past the bound is killed with its process group and counts as failing.
- `base-unrunnable` — A base invocation whose output reports a full disk, an unloadable manifest or an unfetchable dependency raises `TestFirstBaseUnrunnable` instead of a verdict.
  *because such a fault fails at base whatever the tests assert, and reading it as red admits an untested change*
- `refactor-trailer` — A commit carrying the trailer `Test-First: refactor` exempts the source it alters from {{assurance.test.test-first}}; the rest of the range stays held, and the workspace stage alone holds that commit.
  *because a behavior-preserving change has no failing test to write, and the existing suite is its specification*
- `acceptance-surface` — An acceptance test drives a built binary through its command line, MCP or HTTP surface; a workspace package among the acceptance package's dependencies raises `AcceptanceLinksEngine`.
  *A-assurance#source-change-carries*

## build

Target directories, the engine-linked invocation, linked query functions, build targets, release artifacts and the dependency allowlist.

- `target-dir-per-stage` — Each cargo stage builds into a target directory of its own, reclaimed once the stage passes.
  *because stages under different feature unification share no artifacts, and peak disk is then one stage*
- `checkout-artifacts` — `.cargo/config.toml` names the checkout root, and every workspace package's build script folds its canonical path into the package fingerprint, so a shared target directory recompiles workspace packages per checkout while unchanged dependencies stay fresh.
  *A-assurance#gate-repair-evidence*
- `runtime-checkout` — Reusable test helpers resolve the workspace root and tested binary from explicit runtime inputs bound to the invocation; cached helpers carry no compile-time checkout path deciding which source or binary runs.
  *A-assurance#gate-repair-evidence*
- `one-engine-build` — The workspace stage builds every engine-linked package in one cargo invocation over the union of their features, and the store adapter's suites resolved without `read` link no SQL engine, so the features stage compiles one copy.
  *because one cargo invocation compiles the engine once, and a store-adapter suite asserting through SQL links a second copy into the no-`read` features run*
- `staged-feature-runs` — A defect appearing under one feature combination alone is reached by a staged nightly run, one command per container.
- `linked-query-functions` — Columnar file reading and statement serialization link into every build linking the SQL engine, and the read path loads no extension while serving.
  *A-assurance#columnar-read-statement*
- `runtime-extension-load` — A read path loading an extension into a binary that statically links the SQL engine raises `ExtensionAutoloadRefused`.
  *A-assurance#columnar-read-statement*
- `debug-info` — Development and test profiles carry line-tables-only debug information for workspace code and none for dependencies.
  *because every engine-linked test binary embeds the bundled engine, and full debug information multiplies each past a sandbox's disk*
- `release-profile` — Release builds compile with thin link-time optimization, one codegen unit per crate and symbols stripped, and unwind on panic.
  *because the run keeper survives a panicking job only by unwinding to its guard*
- `profile-build` — The features stage tests each package under every feature set its manifest lists in `feature-runs`: each other package has its own nightly check; each binary feature run has separate nightly checks for differential tests and all remaining tests.
  *because one cold build of the binary under one feature set fills most of a stage's wall clock, and a listed set reaches what none and all miss*
- `container-image` — The repository's `Dockerfile` builds one profile, `contextful-full` by default, as a static binary for the `PLATFORM` and `RUST_TARGET` its arguments name, and ships it in a shell-free runtime image as a non-root user over a declared store volume.
- `release-image` — Each profile's release image is an OCI index of a `linux/amd64` and a `linux/arm64` manifest; `contextful-ci image` assembles each from the profile's `x86_64` or `aarch64` musl release binary, with the `Dockerfile`'s runtime user, volume and entrypoint.
  *because arm64 hosts pull the same tag natively, and each release cell already holds its target's binary, so no image compiles twice*
- `image-binary-mismatch` — A binary whose ELF machine differs from its target's, or whose program headers name a dynamic interpreter, raises `ImageBinaryMismatch`, naming the target.
  *because a mislabelled layer runs only under emulation, and a dynamic binary finds no C library in the static runtime*
- `targets` — All three profiles cross-compile to `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`; edge and full also build for `aarch64-apple-darwin` and `x86_64-apple-darwin`.
  *A-assurance#windows-release-support*
- `windows-target` — Windows targets are excluded from the release matrix; no profile publishes Windows artifacts.
  *A-assurance#windows-release-support*
- `wasi-probe` — No profile ships a `wasm32-wasip2` release; a scheduled-tier ledger entry builds the edge profile for it and holds the compressed artifact to {{assurance.gate.edge-budget}}.
  *A-assurance#edge-profile-ships*
- `release-artifact` — Each profile ships a release archive with a SHA-256 checksum and an SBOM, a package-manager formula and an independently tagged container image; the bare formula name and the install script resolve to the full profile.
  *A-assurance#edge-profile-ships*
- `release-builder` — The release command uses `cargo build` by default and `cargo zigbuild` under `--builder zigbuild`, forwarding the selected target and profile features.
  *because Cloudflare Linux builds Darwin targets through Zig while the local release command keeps its native build path*
- `release-metadata` — Each release cell writes a JSON record naming its profile, target, archive, SHA-256 digest and SBOM.
  *because distributed release cells need a small authenticated input for formula generation*
- `formula-manifest` — The formula command accepts a manifest covering every release matrix cell once, with matching asset names and SHA-256 digests, and writes formulas and SHA256SUMS without local archives.
  *because one aggregation sandbox need not download every archive to produce formulas*
- `licence-field` — Every workspace package under `crates/` or `tools/` declares `license = "Apache-2.0"`, inherited from `[workspace.package]`; a package declaring another value or none raises `PackageLicenceMissing`, naming its manifest.
  *because cargo-deny, cargo-about and SBOM generators read the manifest field, not the `LICENSE` file, so an unlicensed package fails a consumer's licence check*
- `dependency-allowlist` — The connector authoring dependency allowlist carries `regex` and `serde_json` at its depth limit; a profile graph reaching `fancy-regex`, `pcre2`, `onig` or `serde_json`'s `unbounded_depth` feature raises `DependencyAllowlistViolation`, naming the profile and path.
  *because a backtracking matcher or a parser without a depth limit lets one hostile record pin a core or exhaust the stack*

#### Scenarios

- `assurance.build.checkout-artifacts`: WHEN checkout A and checkout B build different source into one target directory, THEN B's integration tests, doctests and binary execute B's behavior on cold and warm builds, and their shared dependency stays fresh.
- `assurance.build.runtime-checkout`: WHEN a helper cached from checkout A receives checkout B's runtime inputs, THEN its workspace operations and binary execution reach B alone.

## evaluate

The read-path quality harness: case format, deterministic metrics and floors, judged dimensions, the reader and the judge.

- `real-read-path` — An evaluation run ingests, indexes, retrieves, reads, judges and scores through the surfaces a caller uses, and the harness adds no storage or runtime primitive.
- `through-the-store` — The corpus loads through the real store, and the retriever under test calls {{read.retrieve.ranked-call}} with the options a caller passes.
  *because an unordered full-scan retriever cannot go red when ranking breaks*
- `policy-labels` — An evaluation corpus carries zone and row-policy labels in its table declarations, and a run reads it through the access-control path under one admitted credential.
  *A-assurance#evaluation-data-follows*
- `unlabeled-corpus` — A run over a corpus whose tables declare no zone or row policy, with no `local:` zone passed as `--zone`, raises `EvalCorpusUnlabeled` before any row lands.
  *A-assurance#evaluation-data-follows*
- `case-format` — A case carries an id, a corpus path relative to its case file, a question, tags, and an expected block of answer, artifacts, edges, entities, must-cite, must-abstain, must-not-retrieve, time anchor and recency bound.
- `row-reference` — A case names a row as `<table>#<key>`, the key joining the row's declared primary-key values with commas, and the runner keys each returned row the same way.
- `regression-case` — A case tagged `regression` is a regression case, and its must-not-retrieve set scores {{assurance.evaluate.forbidden-row-rate}}.
- `corpus-layout` — A corpus is a directory holding `contextful.toml`, its table declarations and policy labels, and one `rows/<table>.jsonl` per table.
- `run-command` — `contextful eval run --goldens <file>` loads the case file, lands and folds each referenced corpus into a scratch store at `--landed-at`, the Unix epoch by default, reads every case, and writes the run report.
  *because an undated row landed at the epoch predates every question's window, so a replay scores alike on any day*
- `entity-command` — `contextful eval entity-evidence --manifest <file>` loads versioned synthetic or licensed adjudicated fixtures through {{assurance.evaluate.through-the-store}}, resolving mentions through knowledge cards, following requested predicates through keyed recall, and comparing ordinary ranked evidence retrieval.
- `entity-eligibility` — Both entity-evidence arms receive identical evidence admitted by keyed recall under the case anchor and one reader credential; fixed adjudicated tuples grant no answer credit to later, expired, superseded, inaccessible or unresolved evidence.
- `entity-metrics` — Entity-evidence reports canonical entity accuracy, eligible-claim recall, ordered relationship-path correctness, citation resolution and appropriate abstention independently under deterministic labels; model judgments occupy separate labels.
- `entity-baseline` — Entity-evidence baselines bind fixture version, manifest digest, corpus digest and run configuration, extending {{assurance.baseline.metric-path}} with `entity_evidence.<arm>.<metric>`; versioned ledger entries collect {{assurance.measure.record}} records.
- `legs` — Each case calls {{read.retrieve.ranked-call}} three times at limit k: the lexical leg with the question alone, the vector leg with an embedding and an empty query, the hybrid leg with both.
- `stub-embedder` — The deterministic tier embeds every row and question with a seeded feature-hashing stub embedder; a corpus row or a case carrying its own embedding keeps it.
- `semantic-cache` — `--semantic-manifest` hashes corpus, held-out labels, development queries, configuration, and the model's license, embedding configuration and artifacts at an immutable revision; a changed, unhashed or symbolically linked input or a second table raises `EvalSemanticCacheUnverified` before {{assurance.evaluate.through-the-store}}.
  *because model quality needs real vectors whose provenance and inputs survive offline replay*
- `semantic-inputs` — A missing, non-finite, zero-norm or mismatched vector, a relocated held-out file, an overlapping development query or a configuration difference raises `EvalSemanticInputRejected` before retrieval; every leg consumes the same verified cases and corpus.
  *because a cache fallback or corpus substitution changes the experiment while preserving its name*
- `semantic-report` — Semantic reports carry {{assurance.evaluate.deterministic-tier}} with sample counts and case-bootstrap uncertainty; a comparable baseline reports overall and per-slice regressions under the pinned tolerance.
- `semantic-separate` — Semantic quality reports use their own tier and input identity, exclude answer judging and deterministic mechanics verdicts, and compare baselines only for matching corpus, labels, model and configuration identities.
- `run-verdict` — A run holds its report to the floors and, given `--baseline`, to that file; either red verdict exits non-zero, and `--update-baseline` applies {{assurance.baseline.raise-only}} on green alone.
- `converter` — Each benchmark and application ground truth converts once into the case format through its own converter, and the runner reads nothing else.
- `systems-metrics` — Tokens per query, latency and cost report beside the quality figures, bucketed by corpus size in tokens relative to the reader's context window.
- `checkpoint` — A run appends each case's result as JSONL, and a crash re-runs the in-flight case.
- `run-report` — A run report carries one field per gated metric, the sample count behind each mean, the run block, the per-slice breakdown, and the tally of cases sampled, dropped and unscored.

### deterministic metrics

Ranked metrics and floors computed with no model call.

- `deterministic-tier` — Recall@k, R-precision, hit-rate@k, reciprocal rank and nDCG@k run with no model call, reported per leg — lexical, vector, hybrid — and per slice.
- `distinct-top-k` — Membership in the top k is distinct, and a repeated id counts once.
- `absent-truth` — A ranked metric over a case with no relevant set is NaN and drops out of every aggregate; an empty ranking against a non-empty relevant set scores zero.
- `precision-floor` — Precision at rank min(k, R), R being the relevant-set size, holds at or above 60 percent.
  *because precision@k caps at R/k, below the floor whenever R is small*
- `forbidden-row-rate` — A row named in a must-not-retrieve set holds at 0 percent of a regression case's returned rows.
- `duplicate-row-rate` — A repeated table-and-row-key pair holds at 0 percent of the ranking.
- `in-window-rate` — A case declaring a recency bound holds its in-window rate at or above 95 percent.
- `abstention` — A must-abstain case returns zero rows.
- `truth-per-surface` — Expected artifacts score the artifact retriever and expected edges score the edge retriever, reported apart; edge truth with no edge surface configured counts in an unscored tally.

### judged tier

The reader and the pinned judge score answers through the model endpoint.

- `judged-dimensions` — The reading stage adds four judged dimensions: answer accuracy; abstention, as correct-refusal and hallucination-on-unknown rates; citation faithfulness; and temporal correctness against the bitemporal columns.
- `grounded-reader` — The harness reader answers from retrieved rows alone, and the deterministic tier substitutes a stub reader.
- `model-endpoint` — The judge and the reader reach a model through {{connector.infer.model-endpoint}}, and the harness holds no credential.
- `judge` — The judge is one pinned open-weights instruct model, called once per judged item at temperature zero; a judge swap is a full re-baseline.
  *because a majority vote at temperature zero samples one mode and adds cost without reducing variance*
- `judged-run` — `contextful eval run --tier judged` reads and judges every case through the endpoint and model `--endpoint` and `--model` name, stamping that model in the run block; the deterministic tier judges with the stubs.
- `cutoff-flags` — `contextful eval run` takes `--snapshot`, `--pretrain-cutoff` and `--posttrain-cutoff` for the reader and `--judge-snapshot`, `--judge-pretrain-cutoff` and `--judge-posttrain-cutoff` for the judge, each cutoff value a month and its tag, as `2024-10:documented`.
- `judged-cutoffs` — A judged run block stamps the reader's and the judge's snapshot ids and their {{connector.infer.cutoffs}}.
  *because a score from a model trained past a case's resolution measures recall, not reading*

#### Scenarios

- `assurance.evaluate.policy-labels`: WHEN a regression case names a row another agent owns under `owner = subject.agent`, THEN no leg returns it and the hybrid forbidden-row rate reads 0.
- `assurance.evaluate.cutoff-flags`: WHEN a run passes `--pretrain-cutoff 2024-10:documented` and no `--posttrain-cutoff`, THEN the reader's post-training cutoff compares as each call's own instant.
- `assurance.evaluate.judged-cutoffs`: WHEN a judged run passes `--judge-posttrain-cutoff 2025-03:estimated`, THEN the run block carries that cutoff tagged `estimated` beside the judge's snapshot id.

## baseline

Holding a run against committed baselines and absolute floors, intervals, golden custody and case generation.

- `file` — A baseline file carries a reserved run block `{k, tier, model, samples}` and one entry per gated metric: a bare number at the run's dead band, or an object overriding the band.
- `default-dead-band` — A rate-valued entry with no override gates at a 2 percent dead band.
- `rank-quality-dead-band` — The ranked-quality entry gates at a 3 percent dead band.
- `band-units` — A band carries its metric's own units, and a count entry is pinned at zero with no band.
- `metric-path` — An entry names a metric by the report's field path: `retrieval.<modality>.<metric>`, `edge_retrieval.<modality>.<metric>`, `judge.<dimension>`, `latency_ms`, `n_cases`, or `slices.<tag>.` before a bare `<metric>` or any of these but `latency_ms`.
- `sample-count` — Every mean-valued path takes a `.n` suffix naming its sample count.
  *because blanking one case's truth lifts a NaN-filtered mean without moving the case count*
- `unresolvable-path` — A path resolving to nothing, a mean over fewer than 30 cases, a malformed entry or a non-finite band raises `BaselinePathUnresolved` before the suite runs.
  *P7*
- `run-stamp-drift` — A run block differing from the run's own configuration raises `BaselineRunStampMismatch` before the suite runs.
  *P7*
- `cutoff-block` — A judged baseline's run block carries the reader's and the judge's snapshot ids and {{connector.infer.cutoffs}}, so {{assurance.baseline.run-stamp-drift}} refuses a run under a changed snapshot or cutoff.
  *because a score from a model trained later measures more recall, not better reading*
- `interval` — Each judged figure reports a 95 percent percentile-bootstrap interval over cases from 1000 resamples.
- `raise-only` — A baseline update runs after every gate and floor passes, moves each improved entry to its measured value, moves none in the worse direction, and adds no path.
- `floors-are-absolute` — An absolute floor gates independently of every committed value.
- `floor-coverage` — A report lacking a floor's figure on any leg of its `retrieval` surface breaches that floor; only a forbidden-row or in-window rate over no case holds.
- `offline` — The gate is a local command comparing the report against in-tree baselines and floors, reporting both verdicts, with or without a trace store reachable.
- `trace-store` — A trace store records run history and curation staging, and decides no verdict.
- `golden-custody` — Version-controlled JSONL under `evals/cases/` is the canonical golden set, one loader is its only ingestion door, and a reviewer's edit reaches the gate as a change to the tree.
  *A-assurance#evaluation-data-follows*
- `answer-key-in-the-store` — An artifact path normalizing under the evaluation directory, reached by a manifest lint or a connector, raises `GoldenSetIngested`.
  *A-assurance#evaluation-data-follows*
- `generated-redaction` — A golden mined from a live query or promoted from a labeled outcome passes {{authority.redact.write-time}} before commit; a case it cannot redact raises `GoldenRedactionFailed`.
  *A-assurance#evaluation-data-follows*
- `promotion-source` — Only an adjudicated outcome label promotes into a golden set; a self-rated row does not.
- `generators` — Three generators draft candidate cases from the deployment's store — entity-to-fact-to-edge walks, composed edge chains, and questions about absent entities — and a human approves each before commit.
- `draft-command` — `contextful eval draft` reads the declared entity, fact and edge tables through one admitted session, runs the three generators, and writes their candidates as case lines; it commits nothing.
- `commit-command` — `contextful eval commit` appends each draft an `--approve <id>=<reviewer>` names, redacted under the declaration's removal rules, to the named case file; an id the file already holds refuses the commit.
- `held-out` — Retrieval and synthesis are tuned on no gate input.
- `rotation` — The native set rotates on a cadence, a sample of its truth is reviewed on each rotation, and a comparison across generations resolves through the pinned run stamp.
- `native-gate` — A native benchmark generated from a real corpus is the red or green gate; public benchmark sets run beside it as held-out comparison and decide nothing.
- `public-corpora` — A public benchmark corpus enters through a fetch-manifest entry naming its URL, its dataset's terms and a SHA-256 digest, and is never committed; fetched bytes under another digest raise `PublicCorpusDigestMismatch`.
  *because a corpus whose bytes drift scores another benchmark under the same name*
- `trace-export` — A run touching a deployed store with a hosted trace endpoint configured raises `TraceExportOutOfPerimeter`; hosted collection serves runs over public or synthetic fixtures alone.
  *A-assurance#evaluation-data-follows*


## backtest

Historical evaluations, called backtests: bounded cases, model-knowledge controls, label intervals and splits, execution metadata, planted leaks and replayable trajectories. Financial return-series and execution profiles are consumer-selected applications of this flow ({{topology.bound-application.domain-neutral}}).

- `bounded-case` — A bounded case carries a horizon, an instant each of its reads passes as {{store.available-time.available-as-of}}; its trajectory is the ordered tool calls, queries and retrieved documents its run records.
- `checkpoint-route` — A reader, derive engine or embedder naming a checkpoint family resolves each bounded case to the newest family checkpoint whose post-training {{connector.infer.cutoffs}} precedes the case's horizon.
  *because date-bounded checkpoints remove memorized outcomes at the source*
- `cutoff-slice` — A backtest report places each case by its resolution instant minus the reader's post-training cutoff and states leakage against a declared reference, a cutoff discontinuity or a clean control model, never a bare pre/post gap.
  *A-assurance#backtests-estimate-leakage, A-assurance#case-s-judge*
- `cutoff-sensitivity` — The report repeats {{assurance.backtest.cutoff-slice}} with each `estimated` cutoff moved across its declared uncertainty interval, and states the minimum detectable effect.
  *because finding no leakage bounds it and does not certify its absence*
- `latest-arm` — A case set run under a checkpoint family also runs the family's latest checkpoint as a paired arm, and the report states the latest-minus-routed difference.
  *because the latest-minus-dated gap isolates the leakage premium*
- `answer-key-only` — A judge scores a case resolved before the judge's post-training cutoff against the case's stored expected answer alone.
  *because a judge reasoning about an outcome may remember it*
- `run-stamp` — Beyond {{assurance.evaluate.judged-cutoffs}}, taken through {{assurance.evaluate.cutoff-flags}}, a backtest run block stamps the derive engine's and embedder's snapshot ids and cutoffs, the temperature, seed, prompt digest and tool-schema version.
  *because two runs differing in any of these are two trials, and a report cannot slice on a value it never recorded*
- `label-interval` — Every bounded case declares its label interval `[t0, t1]`, the span its outcome is observed over; a bounded case without one raises `EvalLabelIntervalMissing` before any of its reads.
  *because purge and embargo need each label's span, and a label observed past the horizon is the leak itself*
- `split` — A split purges each training case whose label interval overlaps a test case's, and embargoes each training case starting within a declared span after a test fold ends.
  *because serially correlated labels carry a test fold's outcome into adjacent training cases*
- `trial-ledger` — A financial return-series profile records each run against its case set as a trial row and computes the deflated Sharpe ratio from trial count, Sharpe variance, series length, skew and kurtosis.
  *because the deflated Sharpe needs every trial's Sharpe, not a reported count*
- `seed-repeats` — A configuration runs its declared repeat count; its repeats count as one trial, and the report shows their spread.
- `universe-table` — A case evaluating changing membership names a membership table declaring valid time and `available_time`; a case naming literal identifiers reports under a separate `fixed-universe` slice.
  *because a fixed identifier list names only the members known when the case was written*
- `fill-instant` — A financial execution profile fills each decision at the first price available strictly after the decision's recorded instant.
  *A-assurance#backtests-estimate-leakage*
- `execution-component` — A backtest report stamps its declared execution component's digest and parameters under {{topology.bound-application.domain-neutral}}; a report without one raises `EvalExecutionUndeclared`.
  *A-assurance#backtests-estimate-leakage*
- `planted-future` — Each bounded case plants a row available after its horizon in every table it reads; a planted row in the case's trajectory fails the case.
  *because a planted row proves the bound held on the path the case took, not only in its configuration*
- `planted-delisting` — A financial universe profile plants a member delisted inside its window; the case fails unless its trajectory holds that member's delisting return.
  *because survivorship shows as a missing delisting return, which no aggregate figure reveals*
- `leak-score` — The case's judge model scores each retrieved document 0 to 4 against a reference answer from rows available before the horizon, landing one row per document and case horizon; the report gives Brier per leak tier.
  *A-assurance#case-s-judge*
- `trajectory-check` — A bounded case fails when a call in its trajectory lacks the {{disclosure.record.bound-entry}} attributes, or a retrieved document scores at or above the case's declared leak threshold.
  *because a bound proves the configuration, and leaked page content passes the bound*
- `query-finding` — A recorded query naming an entity first available after the case's horizon lands a finding naming the call.
  *because a model that remembers the outcome steers its queries toward it*
- `trajectory-replay` — `contextful eval replay <run>` re-serves each recorded response without reading the store and reproduces the run report's digest.
  *because a backtest a third party cannot re-score is a claim, not evidence*
- `replay-root` — A backtest run report carries the signed audit-chain root covering its run's {{disclosure.record.bound-entry}} entries.
  *because the root lets a verifier prove each replayed response is the one the run recorded*

#### Scenarios

- `assurance.backtest.planted-future`: WHEN a bounded case's trajectory returns the row planted one day after its horizon, THEN the case fails and the report names the call that returned it.

## forecast

Probability forecasts over consumer-declared outcomes: freeze instant, settlement, lead time, imputation, coverage, reference baselines and separate scoring tracks. Prediction-market baselines belong to an explicitly selected profile ({{topology.bound-application.domain-neutral}}).

- `forecast-case` — A forecast case carries a freeze instant, a resolution statement naming its vintage, a resolution instant and a probability answer; a backtest forecast case is a {{assurance.backtest.bounded-case}} whose horizon is its freeze instant.
- `resolved-before-freeze` — A backtest forecast case whose resolution instant precedes its freeze instant raises `EvalCaseResolvedBeforeFreeze`.
  *because a question picked after its answer is known leaks through its own existence*
- `settlement` — A forecast case settles through the label join within {{read.settle.grace-window}}, and its scores report as {{read.settle.calibration-report}} groups them.
  *because one settlement path keeps a forecast's Brier score comparable with an application's calibration*
- `lead-time` — An answer recorded less than the case's declared minimum lead before resolution scores on a separate track.
  *because an answer given near resolution reads the outcome rather than forecasting it*
- `market-context` — A prediction-market profile supplies freeze-instant probability prices as available rows, and the report shows each forecaster's score improvement over that baseline.
- `imputed` — A missing answer scores as probability 0.5 for a dataset case; a prediction-market profile instead uses its freeze-instant probability price.
  *because dropping a missing answer rewards abstaining on hard questions*
- `coverage-floor` — A forecaster answering below 95 percent of a round's cases is reported unranked.
  *because imputation alone still lets a forecaster pick its questions*
- `no-evidence-baseline` — Each round also scores a forecaster given the question text alone, and the report subtracts its Brier score from each forecaster's.
  *because which questions exist can reveal their answers without any retrieval*
- `tracks-separate` — Backtest, prospective, tool-free and tool-using scores never pool into one figure.
  *A-assurance#backtests-estimate-leakage*

#### Scenarios

- `assurance.forecast.resolved-before-freeze`: WHEN a backtest forecast case freezes on `2014-06-01` and resolved on `2014-05-20`, THEN the run raises `EvalCaseResolvedBeforeFreeze` before reading the case.

unsettled: Which consumer-selected reference anchors difficulty-adjusted probability scores across case sets? owner: eval affects: assurance.forecast

## Shapes

A baseline file:

```json
{
  "_run": { "k": 10, "tier": "judged", "model": "pinned-open-weights", "samples": 1 },
  "retrieval.hybrid.recall_at_k": 0.71,
  "retrieval.hybrid.recall_at_k.n": 184,
  "retrieval.hybrid.ndcg_at_k": { "value": 0.63, "band": 0.03 },
  "retrieval.hybrid.ndcg_at_k.n": 184,
  "judge.citation_faithfulness": 0.82,
  "judge.citation_faithfulness.n": 184,
  "latency_ms": { "value": 480, "band": 60 },
  "n_cases": 184
}
```

A case:

```json
{
  "id": "edge-chain-0117",
  "corpus": "native/r3",
  "question": "Which supplier does the northern plant share with the coastal plant?",
  "expected": {
    "answer": "Meridian Castings",
    "artifacts": [["supplier_notes", "b7f1c2"], ["plant_registry", "0a44de"]],
    "edges": [["plant:north", "supplied_by", "supplier:meridian"]],
    "entities": ["supplier:meridian"],
    "must_cite": ["supplier_notes"],
    "must_abstain": false,
    "time_anchor": "<RFC 3339 instant>"
  },
  "tags": ["multi_hop", "entity_resolution"]
}
```
