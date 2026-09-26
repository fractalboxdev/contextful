---
contract: assurance
owns:
  - structure-tree
  - automate
  - test
  - build
  - gate
  - evaluate
  - baseline
---

# Engineering gates and the quality harness

How the tree is written, built and checked: one home per capability, typed automation, test
placement, build targets and artifacts, the gate's stages and ceilings, and the harness that
turns read-path quality into a red or green verdict.

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
- `decision-module` — One module owns the enforcement decision; a gateway and the engine reaching the same verdict execute it, compiled native and to WebAssembly from one pinned source.
  *P5*
- `adapter` — A surface with no engine process on its request path implements an adapter against the engine's ports.
  *P5*
- `derivation-check` — A generated artifact declares the check that fails once its source moves, and the gate's schema stage runs that check.
  *P5*
- `mirror-exemption` — A review-time restatement of a build-time rule is admitted where its site carries a `mirrors: <clause id>` comment naming the clause it restates; an unannotated restatement stays {{assurance.structure-tree.duplicated-capability}}.
  *because a reviewer-facing check sometimes restates a rule, and the annotation keeps the engine its one home and the copy traceable to it*
- `mirror-unresolved` — A `mirrors:` comment in a tracked file under `crates/`, `tools/` or `apps/` naming no clause of `spec/spec.lock.json` raises `MirrorUnresolved` in the schema stage, naming the file and line.
  *because an annotation naming nothing exempts a copy while tying it to no rule it could drift from*

unsettled: Does a derived artifact prove currency by a schema-hash comparison, by regeneration and diff, or by a build-time export? owner: build affects: assurance.structure-tree

## automate

Typed subcommands over compiled binaries, and the boundary shell keeps.

- `typed-subcommand` — An automation step carrying a branch, a loop or a retry, or running past 5 lines, is a typed subcommand shelling out to compiled binaries.
  *P7*
- `subcommand-surface` — A subcommand exposes typed options, generated help and an entry point a test calls directly.
- `one-path` — The gate invokes the subcommand a contributor invokes.
  *because a local reproduction and a gate run then execute one code path*
- `unchecked-shell` — A shell file in the tree that `shellcheck` rejects raises `ShellCheckFailed` in the toolchain stage.
  *P7*
- `build-time-toolchain` — The automation toolchain links into no build profile and enters no release artifact.

## test

Assertion construction, guard validation, suite placement, test-first and acceptance tests, and the connector test kit.

- `presence-before-absence` — A test whose central claim is an absence first constructs the condition it names and verifies that condition obtains.
  *P7*
- `vacuous-exclusion` — An exclusion assertion evaluated over an empty collection raises `VacuousAssertion`.
  *P7*
- `guard-fires-both-ways` — A guard is validated against the fixture that motivated it and against the state it stands against, passing on the first and failing on the second.
- `one-integration-binary` — A crate's integration tests compile in one target, `tests/integration/main.rs`, which declares each suite as a module selected by path.
  *because each file directly under `tests/` links its own static binary, hundreds of MiB for an engine-linked crate*
- `feature-gated-suite` — A feature-gated suite carries a module-level `cfg` attribute at the head of its module file and compiles to nothing while its feature is off.
- `own-process` — A test needing its own process sits in a top-level file stating why at its site; a thread-scoped log capture is such a test.
- `connector-kit` — The connector authoring toolkit ships a conformance suite — discovery returns valid schemas, an opened table yields a finite stream, a position round-trips — plus recorded-HTTP fixture replay and property tests over position monotonicity.
- `test-first` — A change altering Rust source under `crates/` or `tools/` adds or alters a test under a package's `tests/` that fails against the base commit's source; a change without one raises `TestNotFirst`.
  *A-assurance*
- `test-first-scope` — The test-first stage builds each changed test file's target against the base source, then runs exactly the tests under that file's top-level module; a target failing to compile there counts as failing.
- `base-run-bound` — One test-first execution against the base, its build excluded, runs for at most 300 s; a run past the bound is killed with its process group and counts as failing.
- `base-unrunnable` — A base invocation whose output reports a full disk, an unloadable manifest or an unfetchable dependency raises `TestFirstBaseUnrunnable` instead of a verdict.
  *because such a fault fails at base whatever the tests assert, and reading it as red admits an untested change*
- `refactor-trailer` — A commit carrying the trailer `Test-First: refactor` exempts the source it alters from {{assurance.test.test-first}}; the rest of the range stays held, and the workspace stage alone holds that commit.
  *because a behavior-preserving change has no failing test to write, and the existing suite is its specification*
- `acceptance-surface` — An acceptance test drives a built binary through its command line, MCP or HTTP surface; a workspace package among the acceptance package's dependencies raises `AcceptanceLinksEngine`.
  *A-assurance*

The test-first check over one commit in a change's range:

```mermaid
flowchart LR
  CH["commit altering Rust source"] -->|"trailers read"| TR{"refactor trailer?"}
  TR -->|"yes"| WS["workspace stage"]
  TR -->|"no"| T{"adds or alters a test?"}
  T -->|"no: TestNotFirst"| AUTH(["change author"])
  T -->|"yes"| B{"test fails on base?"}
  B -->|"no: TestNotFirst"| AUTH
  B -->|"yes"| OK["test-first stage"]
  OK -->|"passing change"| WS
```

unsettled: Does a suite contending process-global state declare that state in its module, or acquire a named lock the integration binary owns? owner: build affects: assurance.test

## build

Target directories, the engine-linked invocation, linked query functions, build targets, release artifacts and the dependency allowlist.

- `target-dir-per-stage` — Each cargo stage builds into a target directory of its own, reclaimed once the stage passes.
  *because stages under different feature unification share no artifacts, and peak disk is then one stage*
- `one-engine-build` — The engine-linked packages build in one cargo invocation over the union of their features, compiling the bundled SQL engine once per gate run.
  *A-assurance*
- `staged-feature-runs` — A defect appearing under one feature combination alone is reached by a staged run, one command per container.
- `linked-query-functions` — Columnar file reading and statement serialization link into every build linking the SQL engine, and the read path loads no extension while serving.
  *A-assurance*
- `runtime-extension-load` — A read path loading an extension into a binary that statically links the SQL engine raises `ExtensionAutoloadRefused`.
  *A-assurance*
- `debug-info` — Development and test profiles carry line-tables-only debug information.
- `targets` — All three profiles cross-compile to `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`; edge and full also build for `aarch64-apple-darwin`, `x86_64-apple-darwin` and `x86_64-pc-windows-msvc`; edge also targets `wasm32-wasip2`.
- `release-artifact` — Each profile ships a release archive with a SHA-256 checksum and an SBOM, a package-manager formula and an independently tagged container image; the bare formula name and the install script resolve to the full profile.
- `dependency-allowlist` — The connector authoring dependency allowlist carries a linear-time regular-expression engine and bounded-depth deserialization, and admits no backtracking regex engine and no unbounded recursive parser.

unsettled: Does the edge profile build for `wasm32-wasip2` with the SQL engine inside its footprint budget? owner: build affects: assurance.build

## gate

Stage order, secrets of record, the crate-graph, row-token, egress and dependency rules, the formal stage, the container's ceilings, disk, footprint budgets and surface checks.

- `stage-sequence` — The gate runs its stages in order — pins, toolchain, schema, test-first, workspace, acceptance, features, crate graph, connectors, TypeScript surfaces, formal, budget — and a subset is selectable by name.
- `remote-check` — The pull-request workflow dispatches every stage the gate subcommand defines to a remote runner, each as one status check labelled with the stage's name.
  *A-assurance*
- `fork-dispatch` — The pull-request workflow dispatches only a head commit pushed to the repository itself; a pull request from a fork dispatches no stage and so carries none of the required checks.
  *because a dispatch carries the org's signing secret and runs the commit on the org's runner, and an absent required check fails closed*
- `stage-reports` — Each stage prints the environment it leaves and its memory limit, peak and event counts, and a failing stage prints its diagnostics before propagating its exit code.
  *because memory exhaustion is silent, and a kill then reads as a number in the log*
- `pins-stage` — The pins stage resolves every pinned artifact identity a run depends on before any compilation.
- `schema-stage` — The schema stage regenerates each derived artifact into a scratch location, compares it byte for byte against the committed copy, and runs `contextful-spec lint`.
- `secret-ciphertext` — In the schema stage, a key other than `DOTENV_PUBLIC_KEY*` in a git-tracked `.env*` file other than `.env.example` holding a value without the `encrypted:` prefix, or a tracked `.env.keys`, raises `SecretPlaintext`, naming file and key.
  *because a tracked file reaches every clone, and only dotenvx ciphertext is safe there while its private key stays untracked*
- `secret-scope` — In the schema stage, a key other than `DOTENV_PUBLIC_KEY*` in a git-tracked `.env*` file whose block — the assignments above it, up to the first blank line — opens with no comment carrying text raises `SecretScopeMissing`, naming file and key.
  *because a provider token cannot report its own grants, so the comment above it is the one record of what it reaches*
- `crate-graph` — A test over `cargo metadata` holds run-path crates to reaching read-path crates through the three crossing crates alone; another edge raises `CrateGraphViolation`, naming both crates.
  *P5*
- `row-token` — Every row-returning function of the store crate takes the enforcement token type, whose constructor is private to the enforcement module; a row path outside a registered relation raises `EnforceUnmediatedPath`, naming the function.
  *P5*
- `dependency-deny` — The crate-graph stage runs cargo-deny over each profile's resolved graph, with a deny list holding every crate a dependency refusal of another contract names; a hit raises that clause's error.
  *P7*
- `unconfigured-egress` — An outbound call reachable under the default configuration — usage ping, license check, update probe — raises `UnconfiguredEgress` in the crate-graph stage, naming the call site.
  *because some deployments run with no external reach, and a call nobody configured moves data outside every grant, zone and audit record*
- `interpolated-claim` — A source lint over the runtime crates finding a subject claim formatted into SQL text raises `EnforceInterpolatedSubjectClaim`, naming the file and line.
  *P1*
- `formal-stage` — The formal stage runs {{assurance.audit-assumptions.check-command}}, {{assurance.differential-test.command}} and {{assurance.model.protocol-check}}; a non-zero exit from any reds the run.
  *P7*
- `container` — The gate container carries four ceilings — processor count, 12 GiB of memory, 18 GiB of usable disk, and a per-stage wall clock — and a run dies on any one.
- `parallel-jobs` — Parallel build jobs number the memory ceiling divided by 3 GiB.
- `free-disk` — A stage starting with less than 2 GiB of free disk raises `BuildDiskPrecondition` and exits 28 before doing work.
  *P7*
- `build-size` — The budget stage holds total build size to 12 GiB, printing that size, the free space and the largest artifacts; a larger build raises `BuildSizeCeilingExceeded`, naming the head of that list.
  *P7*
- `edge-budget` — The edge profile holds to 50 MiB compressed and 60 MiB idle resident set.
- `full-budget` — The full profile holds to 150 MiB compressed and 180 MiB idle resident set.
- `control-budget` — The control profile holds to 60 MiB compressed and 80 MiB idle resident set.
- `footprint` — Per change and per profile, the footprint step builds the static-linked Linux target, compresses it, and holds its size to the profile's budget and its dynamic dependencies to the platform C library.
  *P7*
- `footprint-exceeded` — An artifact over its profile's budget, or carrying a dynamic dependency beyond the platform C library, raises `FootprintBudgetExceeded`, naming the profile.
  *P7*
- `typescript-surfaces` — The TypeScript surfaces run typecheck, unit tests and framework build in one stage, and a surface declaring no script for a check skips that check.
- `surface-check-failed` — A surface whose typecheck, unit tests or framework build fails raises `SurfaceCheckFailed`, naming the surface and the script.
  *P7*

The stages, numbered in run order, under the container's ceilings:

```mermaid
flowchart LR
  subgraph contributor["contributor"]
    LOCAL["contextful-ci gate"]
  end
  subgraph forge["pull-request workflow"]
    WF["one remote check per stage"]
  end
  LOCAL -->|"local run"| C
  WF -->|"remote run"| C
  subgraph C["gate container, 12 GiB memory and 18 GiB disk"]
    S1["1 pins"]
    S2["2 toolchain"]
    S3["3 schema"]
    S4["4 test-first"]
    S5["5 workspace"]
    S6["6 acceptance"]
    S7["7 features"]
    S8["8 crate graph"]
    S9["9 connectors"]
    S10["10 TypeScript surfaces"]
    S11["11 formal"]
    S12["12 budget"]
  end
  S3 -.->|"runs"| LINT["contextful-spec lint"]
  S8 -.->|"runs"| DENY["cargo-deny per profile"]
  S11 -.->|"runs"| FORM["formal check"]
  S12 -.->|"measures, 12 GiB cap"| SIZE["build size"]
```

unsettled: Which workload, cadence and drift bound does the idle-resident soak run under, given that a multi-day soak fits no per-change gate? owner: build affects: assurance.gate

unsettled: What ordering holds when a selected stage subset omits a stage a later stage reads output from? owner: build affects: assurance.gate

## evaluate

The read-path quality harness: case format, deterministic metrics and floors, judged dimensions, the reader and the judge.

- `real-read-path` — An evaluation run ingests, indexes, retrieves, reads, judges and scores through the surfaces a caller uses, and the harness adds no storage or runtime primitive.
- `through-the-store` — The corpus loads through the real store, and the retriever under test calls {{read.retrieve.ranked-call}} with the options a caller passes.
  *because an unordered full-scan retriever cannot go red when ranking breaks*
- `policy-labels` — An evaluation corpus carries zone and row-policy labels, and a run reads it through the access-control path.
  *A-assurance*
- `unlabeled-corpus` — A run over a corpus carrying neither labels nor an explicit local zone raises `EvalCorpusUnlabeled`.
  *A-assurance*
- `case-format` — A case carries an id, a corpus reference, a question, tags, and an expected block of answer, artifacts, edges, entities, must-cite, must-abstain and time anchor.
- `converter` — Each benchmark and application ground truth converts once into the case format through its own converter, and the runner reads nothing else.
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
- `judged-dimensions` — The reading stage adds four judged dimensions: answer accuracy; abstention, as correct-refusal and hallucination-on-unknown rates; citation faithfulness; and temporal correctness against the bitemporal columns.
- `grounded-reader` — The harness reader answers from retrieved rows alone, and the deterministic tier substitutes a stub reader.
- `model-endpoint` — The judge and the reader reach a model through {{connector.infer.model-endpoint}}, and the harness holds no credential.
- `judge` — The judge is one pinned open-weights instruct model, called once per judged item at temperature zero; a judge swap is a full re-baseline.
  *because a majority vote at temperature zero samples one mode and adds cost without reducing variance*
- `systems-metrics` — Tokens per query, latency and cost report beside the quality figures, bucketed by corpus size in tokens relative to the reader's context window.
- `checkpoint` — A run appends each case's result as JSONL, and a crash re-runs the in-flight case.
- `run-report` — A run report carries one field per gated metric, the sample count behind each mean, the run block, the per-slice breakdown, and the tally of cases sampled, dropped and unscored.

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
- `interval` — Each judged figure reports a 95 percent percentile-bootstrap interval over cases from 1000 resamples.
- `raise-only` — A baseline update runs after every gate and floor passes, moves each improved entry to its measured value, moves none in the worse direction, and adds no path.
- `floors-are-absolute` — An absolute floor gates independently of every committed value.
- `floor-coverage` — A report lacking a floor's figure on any leg of its `retrieval` surface breaches that floor; only a forbidden-row or in-window rate over no case holds.
- `offline` — The gate is a local command comparing the report against in-tree baselines and floors, reporting both verdicts, with or without a trace store reachable.
- `trace-store` — A trace store records run history and curation staging, and decides no verdict.
- `golden-custody` — Version-controlled JSONL under `evals/cases/` is the canonical golden set, one loader is its only ingestion door, and a reviewer's edit reaches the gate as a change to the tree.
  *A-assurance*
- `answer-key-in-the-store` — An artifact path normalizing under the evaluation directory, reached by a manifest lint or a connector, raises `GoldenSetIngested`.
  *A-assurance*
- `generated-redaction` — A golden mined from a live query or promoted from a labeled outcome passes {{authority.redact.write-time}} before commit; a case it cannot redact raises `GoldenRedactionFailed`.
  *A-assurance*
- `promotion-source` — Only an adjudicated outcome label promotes into a golden set; a self-rated row does not.
- `generators` — Three generators draft candidate cases from the deployment's store — entity-to-fact-to-edge walks, composed edge chains, and questions about absent entities — and a human approves each before commit.
- `held-out` — Retrieval and synthesis are tuned on no gate input.
- `rotation` — The native set rotates on a cadence, a sample of its truth is reviewed on each rotation, and a comparison across generations resolves through the pinned run stamp.
- `native-gate` — A native benchmark generated from a real corpus is the red or green gate; public benchmark sets run beside it as held-out comparison and decide nothing.
- `trace-export` — A run touching a deployed store with a hosted trace endpoint configured raises `TraceExportOutOfPerimeter`; hosted collection serves runs over public or synthetic fixtures alone.
  *A-assurance*

unsettled: Which public benchmark corpora are admissible gate inputs under research and non-commercial licences: fetched under the dataset's terms, transformed into a synthetic subset, or pinned by a content-hashed fetch manifest? owner: build affects: assurance.baseline

## Shapes

Where the build-time material sits:

```
tools/
  ci/                     typed subcommands the gate invokes
  spec/                   the corpus checker
  eval/                   the quality harness's metrics, floors and baseline gate
crates/acceptance/
  tests/integration/mNN.rs  one milestone's acceptance test, driving a built binary
evals/
  cases/                  version-controlled JSONL, one case per line
  baselines/              one file per gated configuration
  generators/             graph walk, edge chain, absent entity
  converters/             one per external ground-truth source
crates/<name>/tests/
  integration/main.rs     the crate's one integration binary
  integration/<suite>.rs  a module, selected by path
  <isolated>.rs           its own process, with a comment saying why
```

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
