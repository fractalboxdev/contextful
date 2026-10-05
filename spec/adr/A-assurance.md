# A-assurance — Assurance decisions

**Status:** accepted

## Assurance claims carry their qualifiers, and the proof gate audits assumptions

`assurance.scope-claim` states named authorization decisions, named specifications and stated translation and runtime assumptions; a wider claim raises `ClaimBeyondNamedDecisions`. `assurance.prove` publishes each theorem with the statement it leaves open; the composition theorem covers the one order the engine applies. `assurance.audit-assumptions` matches a hand-maintained inventory against each constant's transitive assumption footprint over a two-entry allowlist. `assurance.recheck` rebuilds from pinned source, credential-free, in a zero-dependency package.

| Option | Lost on | Cost |
| --- | --- | --- |
| Inventory plus transitive assumption audit, qualified claims, zero-dependency package *(chosen)* | — | Every lemma is hand-rolled; the inventory is maintained by hand; the claim is narrower than a buyer prefers. |
| A source pattern over package files | Catching a proof of `False` | A parenthesized hole or a respelled tactic passes. |
| Build and read the exit status | Catching a proof of `False` | A hole elaborates as a warning and exits zero. |
| An inventory generated from the declarations | Independence | A weakened statement carries its inventory with it. |
| Claiming the enforcement engine is verified | Checkability | The models define no credential bytes, SQL semantics or attacker observations. |

Consequences: a proof of `False`, a hole, a deleted theorem and a weakened statement each fail loudly; two machines reach one verdict offline.
Revisit: a mathematics library elaborates inside the per-change budget.

## Evaluation data follows store policy

`assurance.evaluate` runs through the real access path. An unlabeled corpus raises `EvalCorpusUnlabeled`; a golden set ingested into the store it measures raises `GoldenSetIngested`, held by a manifest lint and a connector-side path refusal. A mined golden passes the store's write-time redaction and zone policy; only an adjudicated label promotes. Traces go to a self-hosted collector inside the operator's perimeter, retained under the store's zone policy.

| Option | Lost on | Cost |
| --- | --- | --- |
| Labeled corpora, two-guard golden isolation, store redaction, in-perimeter traces *(chosen)* | — | Labeling precedes the first number; the golden set lives outside every ingestion root; a collector is operated per environment. |
| Bypass enforcement for evaluation runs | Coverage | An over-restrictive policy never surfaces as a recall regression. |
| Commit mined queries verbatim | Data movement | Customer text replicates to every clone of the tree. |
| A redactor written for goldens | One implementation of the rules | A second redactor diverges toward preserving more text. |
| Hosted trace collection for every run | Content | Primary data leaves the perimeter, beyond the store's deletion. |

Consequences: a zero in a report is a retrieval fact; spans never outlive the rows they describe.

## The model specifies the profile-to-effective-authority mapping, each target bound to one query path

`assurance.model` specifies the mapping from the delegation profile to effective authority. `assurance.prove` carries two targets, inclusion and narrowing; profile meaning, restriction preservation and scoped-session execution wait on an execution relation. `assurance.prove` decides placement inclusion symbolically; deciding over sample values raises `ZoneInclusionSampled`. An unmodelled manifest category raises `UnmodelledConstructor`; a target bound to no authenticated query path raises `ProofTargetUnbound`.

| Option | Lost on | Cost |
| --- | --- | --- |
| The profile-to-authority mapping, symbolic, path-bound *(chosen)* | — | The delegation library, its cryptography and its parser are trusted dependencies the claim names. |
| A self-owned wire protocol's attenuation rules | Stability of the proof object | Every protocol correction re-opens the proofs. |
| The whole enforcement engine | Scope | Mediation needs an execution relation the model lacks. |
| Inclusion decided over probe zones | Coverage | A finite sample decides nothing over an unbounded identifier space. |
| Targets named without a path binding | Locatability | A reader cannot find the code a constant describes. |

Consequences: a defect in credential parsing or signature checks lies outside every theorem.
Revisit: the profile's maintainer changes the mapping's domain; an execution relation makes mediation provable; a defect surfaces in the trusted remainder.

## A differentially tested reference model is the standing link between proofs and binary

`assurance.differential-test` drives a reference model and the engine's decision functions over the same generated case; a disagreement shrinks and raises `ReferenceModelDrift`, and one reported without a corpus entry raises `CounterexampleDiscarded`. Minimized cases replay first; the seed is recorded. `assurance.scope-claim` limits refinement to pure decision functions — inclusion and grant narrowing — else `RefinementScopeExceeded`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Differential reference model, refinement scoped to pure functions *(chosen)* | — | Evidence, not equivalence; a case outside the generator's classes goes uncovered; two implementations move together. |
| Refinement proof alone | Survivability of evidence | A broken translator passes about the wrong object, leaving nothing to inspect. |
| The model alone | Relevance to the binary | No statement reaches the artifact that runs. |
| Translating cryptography, adapters, database calls and concurrency | Yield | The hand-written model set grows with no guarantee gained. |
| Reporting disagreements without retaining them | Regression retention | A fixed divergence returns under another seed. |

Consequences: the report names its case classes — malformed, boundary, well-formed.
Revisit: a pinned, reproducible translation toolchain exists; a production divergence falls outside all three classes; keeping the artifacts in step costs more than the divergences caught.

## The store protocol is modelled in Lean 4

`assurance.model` holds the lease, compare-and-swap and fence protocol as an executable Lean 4 state machine in a second package; its four invariants are audited theorems, and a bounded check over three nodes and four lease generations raises `ProtocolInvariantViolated`. `assurance.differential-test` drives the model's compiled executable and the Rust store through generated operation sequences and interleavings; a disagreement raises `ProtocolConformanceDrift`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Lean 4 executable model, conformance through the differential harness *(chosen)* | — | Invariant proofs are hand-written until SMT automation is proven; generated interleavings are samples, and exhaustiveness holds only inside the bounded check. |
| Quint with quint-connect | One formal language | A second specification language, and a v0.1 bridge crate from one young vendor. |
| TLA+ with a harness over ITF traces | Maintained Rust bridge | We build and keep action dispatch, state projection and comparison, in a second language. |
| TLA+ trace validation of an instrumented store | Java-only instrumentation | A Rust logging API is built from scratch, and a run checks only the interleavings it provokes. |

Consequences: one toolchain, one assumption audit and one differential harness cover both models; a theorem proves the invariant for every generation, where a model checker bounds it.
Revisit: the bounded check exceeds the formal stage's wall clock; a maintained Rust bridge to a model checker reaches the protocol's step vocabulary.

## The columnar-read and statement-serialization functions link into every engine-linked build

`assurance.build` links both SQL-engine function sets into each engine-linked build, because an autoloaded extension brings a second copy of the engine's type information: a cast between copies aborts, and on Apple platforms corrupts reads. Reaching for an extension while serving raises `ExtensionAutoloadRefused`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Link both function sets into every engine-linked build *(chosen)* | — | 55 MiB on every engine-linked binary, the read replica included; one crate's dependency declaration fixes the feature list for every profile. |
| Autoload on first use | Correctness | Two type tables abort one platform and corrupt another, plus serve-time egress and a raced shared directory. |
| Extensions in a separate process | Read-path latency | Every columnar read and statement guard crosses a process boundary. |
| Dynamically link the engine | Footprint posture | Profiles hold their dynamic set to the platform C library. |
| Reimplement both capabilities | Correctness | The guard passes statements the engine parses differently. |

Consequences: reversal requires the static-link posture to change first.
Revisit: the engine resolves extensions against the host binary's type tables; a profile's footprint budget cannot absorb 55 MiB.

## A source change carries a test that fails first, and a milestone carries its acceptance test before it progresses

`assurance.test.test-first` requires every change to Rust source under `crates/` or `tools/` to carry a test under `tests/` that fails against the base commit's source, then passes in the workspace stage. `corpus.state.acceptance-first` requires a milestone's acceptance test, driving a built binary through a public surface, before its first pin; `corpus.state.verdict` counts an ignored test as broken. Each gate stage runs remotely as its own required check, invoking the local subcommand.

| Option | Lost on | Cost |
| --- | --- | --- |
| Red-before-green over the diff, acceptance test before the first pin *(chosen)* | — | Every source change pays a second build at the base commit; a refactor declares itself by trailer. |
| A line-coverage threshold | Order | A test written after its code satisfies it. |
| A test file changed alongside source | Vacuity | A test green against the base specifies nothing the change adds. |
| Per-change mutation testing | Wall clock | Minutes per mutated function across an engine-sized crate graph. |
| Review discipline alone | Detectability | A test written second is textually identical to one written first. |

Consequences: inline unit tests satisfy the workspace stage but not the red check.
Revisit: the base-commit build exceeds the stage wall clock; the refactor trailer appears on changes that alter a public surface.

## Tracked targets gate on counts and record per commit in notes

**Status:** proposed

`assurance.measure` keeps one ledger of tracked targets, each owned by a clause. A gate-tier target measures a count, a within-run ratio or a locked-resolve size and decides the evaluate stage. Wall-clock and resident-memory figures are trend-tier: p50 and p95 over repeated batches on a stamped runner, annotated past a band, gating nothing. A trend entry declares its direction and compares with the newest earlier successful report for the same entry, seed and runner stamp. Missing history yields no annotation. Each default-branch run attaches its report under `refs/notes/measures`; gate verdicts read in-tree baselines and floors alone.

| Option | Lost on | Cost |
| --- | --- | --- |
| Count-first gate, trend tier, notes history *(chosen)* | — | GitHub Actions `contents: write` pushes notes; a timing regression surfaces as an annotation. A missing baseline scans first-parent history; matching baselines stop the stream. |
| Separate baseline index | Extra state | An index needs synchronization with notes and reconstruction after missing updates; the stream preserves full-history comparisons across runner changes. |
| Gate on wall-clock p95 against a committed baseline | Determinism | Shared containers move p95 past any useful band; the check flakes until ignored. |
| History as a committed JSONL file | Branch policy | A bot commit on the default branch per run and a conflict with every open change. |
| History in an external artifact store | Offline verdicts and queries | A second store to operate, reachable only with network credentials. |
| A benchmark framework's saved baselines | One home | Baselines live under `target/`, reclaimed after each stage, with statistics apart from the run report. |

Consequences: a red evaluate stage is a correctness fact; timing movement is visible per commit and argued in review.
Revisit: a dedicated runner class holds p95 within 5 percent across runs.

## A version tag names a gated revision and counts closed milestones

**Status:** accepted.

Context: a consumer pinning a git dependency reads a bare commit that promises nothing. Decision: an annotated tag `v0.<closed>.<patch>` names a commit on the default branch whose gate passes; `<closed>` counts roadmap milestones computing `closed` there, and `<patch>` counts tags since `<closed>` last rose. Below `v1` no surface promises compatibility. A maintainer runs a typed `contextful-ci` subcommand that computes the version from `spec/status.md`, refuses a commit off the default branch, a failing gate, a version below the last tag and a `[workspace.package]` version differing from it, then signs the tag. A tag never moves. Criteria: derivability from computed state decided it; no mislabelled tag can be cut.

| Option | Lost on | Cost |
| --- | --- | --- |
| Computed version, maintainer-invoked subcommand *(chosen)* | — | Tagging is a human step; a version bump commit precedes each tag; milestones closing out of roadmap order make `<closed>` a count, not a position. |
| A tag cut by hand | Derivability | A version claims milestones its revision does not pass. |
| A tag on every merge | Signal | Every commit is a version, and a pin reads no more than a commit id. |
| Minor equal to the highest milestone number | Monotonic meaning | Milestone 11 closing before 8 claims milestones never reached. |

Consequences: a consumer reads a tag's closed milestones from `spec/status.md` at that commit; publishing to a package registry stays a separate decision.
Revisit: an external consumer needs a compatibility promise on a public surface, which opens `v1`.

## The remote workspace fits the stage wall clock through parts

**Status:** accepted; answers the test-first decision's revisit trigger, a base-commit build exceeding the stage wall clock.

Context: the feature-unified workspace suite exceeds the 1800 s sandbox cap on a cold remote build. Criteria: every package suite runs, the feature union compiles, and each dispatched command runs locally too.

Decision: `assurance.gate.workspace-parts` dispatches one compile part over the full union and runs the CLI suite from its compiled artifacts. Four other parts partition the remaining non-acceptance package suites. The unsplit local workspace stage retains its one cargo invocation under `assurance.build.one-engine-build`.

| Option | Lost on | Cost |
| --- | --- | --- |
| One union compile with CLI suite and four other suite groups *(chosen)* | — | Remote sandboxes rebuild shared dependencies; five checks replace one. |
| A lock-keyed shared build cache | Operability | A cache bucket and credential become part of the gate; a cold lock still exceeds the cap. |
| One check for the full workspace | Stage wall clock | A cold sandbox times out before it reports a verdict. |
| A higher step timeout | Platform limit | Workflow steps admit at most 30 minutes. |

Consequences: the five remote checks cover the same package set as the unsplit local stage. The compile part reuses CLI artifacts before its sandbox exits; other parts compile their dependencies independently.

## One decision module compiles native and to WebAssembly

**Status:** accepted

Context: a gateway verifies a credential, then admits and places the request ahead of the engine; one verdict from two sources drifts. The credential adapter lives in `contextful-policy`. Criteria: one source for every verdict the gateway and the engine share decided it; the gateway needs verification, admission and placement, never SQL; the module runs under a host with no JavaScript.

Decision: `assurance.structure-tree.decision-module` is `contextful_policy::decide`. A `verify` case admits one credential as a network checkpoint does; coverage, narrowing, zone admission and session-zone resolution delegate to the domain crate. It builds native and as a `wasm32-unknown-unknown` `cdylib` exporting three functions and importing the credential library's evaluator clock, which the host answers. `assurance.differential-test` compares both builds on every case, and the reference model on all but credential cases.

| Option | Lost on | Cost |
| --- | --- | --- |
| The policy package, the credential library's clock as the one import *(chosen)* | — | A 2.7 MB module each differential run compiles once. |
| The domain crate alone, verification outside the module | Gateway need | The gateway verifies signatures from a second source. |
| The credential library's JavaScript bindings | Host | The differential host and a native embedder run no JavaScript. |
| Query rewriting in the module too | Gateway need | The rewriter carries the SQL engine's types. |
| A gateway reimplementation in TypeScript | One home | Two sources for one verdict. |

Consequences: a divergence between builds surfaces as a disagreement. The accepted cost: the host links imports by the binding generator's names, which a credential-library release may rename, and no reference model checks a credential case.

## Profile graphs answer to cargo-deny bans, one run per profile feature

**Status:** accepted

Context: script runtimes, vendor SDKs, the CRDT library and super-linear matchers are refused per profile, yet one walk over workspace-unified metadata cannot tell which profile pulled a crate. Criteria: a hit names its profile and path; the command is identical locally and remotely; a red check follows a change, not the calendar.

Decision: the crate-graph stage provisions cargo-deny at a pinned release, verified by SHA-256, and runs `check bans` once per `contextful-edge`, `contextful-full` and `contextful-control` feature of the binary. `deny.toml` lists the denied crates, each with its clause id as reason; the control run drops the entries its profile admits. Per-package rules stay with `contextful-ci topology`.

| Option | Lost on | Cost |
| --- | --- | --- |
| `bans` per profile feature, pinned binary *(chosen)* | — | Three resolutions per run; a download on a cold sandbox. |
| One run over default features | Profile attribution | The CRDT library in `full` hides behind `control`'s admission. |
| `bans`, `advisories` and `licenses` together | Change-driven verdicts | An advisory published overnight reds an untouched pull request. |
| `cargo install` from source | Stage time | Minutes of compilation per cold sandbox. |

Consequences: `licenses` stays out because `assurance.build.licence-field` holds every workspace manifest. The accepted cost: an advisory against a locked crate reds no pull request, and the gate never reports it.

## The edge profile ships no WASI release, and a scheduled probe measures it

**Status:** accepted.

Context: the edge profile links the SQL engine, C++ relying on threads, file locks and a statically linked extension set that `wasm32-wasip2` does not provide. Criteria: every released artifact builds from the release matrix; a target's viability is a measured figure, not an assumption.

Decision: releases ship Linux on musl for every profile and macOS for edge and full. `assurance.build.wasi-probe` builds the edge profile for `wasm32-wasip2` on the scheduled tier and records its compressed size against the edge budget; a failed build records nothing and gates nothing.

| Option | Lost on | Cost |
| --- | --- | --- |
| No WASI release, a scheduled probe *(chosen)* | — | A function-class host runs the musl edge build or none; the probe's verdict waits for the nightly run. |
| Ship `wasm32-wasip2` for edge | Buildability | The engine's threads and file locks have no WASI preview-2 equivalent; every release blocks on a port. |
| Drop the target with no probe | Measured viability | Nobody learns when the engine's WASI support reaches the budget. |

Consequences: the accepted cost is that a WASI host stays unserved until the probe records a figure inside the budget and the matrix gains the target.

## The store adapter's write suites assert without the SQL engine

**Status:** accepted.

Context: a store-adapter suite asserting through SQL links the bundled engine as a dev-dependency, so the no-`read` run in the features stage compiles a second copy, about 282 MB of rlib, beside the read-enabled one. Criteria: the no-`read` graph, tests included, holds to `topology.package.store-write-engine-free`; the features stage compiles the engine once.

Decision: a suite asserting through SQL runs on the adapter's own read engine and compiles only under `read`; the write suites assert on files, manifests and returned values, and the package declares no engine dev-dependency.

| Option | Lost on | Cost |
| --- | --- | --- |
| Write suites without the engine *(chosen)* | — | The no-`read` run covers fewer tests; a fold's SQL relation is asserted only with `read` on. |
| Keep the engine dev-dependency | One engine copy | A second engine build per features stage, in disk and wall clock. |
| A second Parquet reader for every assertion | One home | A parallel evaluator of the scan relation drifts from the one reads execute. |

Consequences: the accepted cost is that a defect in the relation a scan resolves surfaces only in the read-enabled runs.

## A stage subset runs in sequence order and refuses a missing predecessor

**Status:** accepted

Context: the toolchain stage reads the pins stage's record, and the formal stage reads the toolchain stage's environment. Each remote check runs one stage in a fresh sandbox, and a contributor reruns one stage locally. Criteria: a gate fails rather than skips (P7); a remote check and a local run invoke one command; a rerun reuses an output already on disk.

Decision: `assurance.gate.stage-subset` runs a selection in the sequence's order and refuses before any stage starts when a selected stage reads an unselected predecessor's output and that output is absent. `--predecessors` adds those predecessors, and the workflow passes it (`assurance.gate.remote-predecessors`).

| Option | Lost on | Cost |
| --- | --- | --- |
| Sequence order, refuse an absent predecessor output *(chosen)* | — | Each remote formal check reruns pins and toolchain. |
| Run the named stages alone, whatever is absent | Fail rather than skip | The formal stage runs without the pinned Lean toolchain and reports a verdict on another one. |
| Always add every predecessor | Rerun reuse | A local formal rerun repeats the pin fetch and provisioning each time. |
| Run in the order named | One command | Two contributors naming one subset differently run different gates. |

Consequences: a predecessor's output on disk satisfies its reader, so a stale record from an earlier run reaches the stage. The accepted cost: the remote formal check pays the pins and toolchain stages on every run.
