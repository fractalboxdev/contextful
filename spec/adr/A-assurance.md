# A-assurance — Assurance decisions

**Status:** accepted

## Consumer compatibility binds execution evidence to artifact bytes

Context: a manifest parse establishes neither replay behavior nor receiver durability. A release version does not establish source correspondence. Criteria: consumers reproduce the verdict outside the checkout with no inherited provider account; every required case retains its outcome.

Decision: a distributable matching-target suite drives public surfaces with synthetic sources and a transactional sink. Reports bind binary SHA-256, suite revision, selected storage profile and scenario outcomes. Foreign-target packaging preserves explicit skips. Each release archive includes the suite and evidence; release metadata carries the report reference.

| Option | Lost on | Cost |
| --- | --- | --- |
| Distributable public-surface scenarios *(chosen)* | — | Native execution requires a matching host and synthetic fixture capabilities. |
| Source-only integration tests | Consumer reproducibility | Consumers need the checkout and build toolchain. |
| Manifest and version checks | Behavioral evidence | Parsing hides replay and acknowledgement defects. |

Consequences: compatibility remains scoped to executed scenarios and named storage profiles. A foreign-target archive carries no native execution pass until a matching host runs its suite. Unknown source correspondence remains unknown.

## Assurance claims carry their qualifiers, and the proof gate audits assumptions

`assurance.scope-claim` states named authorization decisions, named specifications and stated translation and runtime assumptions; a wider claim raises `ClaimBeyondNamedDecisions`. `assurance.prove` publishes each theorem with the statement it leaves open; the composition theorem covers the one order the engine applies. `assurance.audit-assumptions` matches a hand-maintained inventory against each constant's transitive assumption footprint over a two-entry allowlist. `assurance.recheck` rebuilds from pinned source, credential-free, in a zero-dependency package.

| Option | Lost on | Cost |
| --- | --- | --- |
| Inventory plus transitive assumption audit, qualified claims, zero-dependency package *(chosen)* | — | Every lemma is hand-rolled; the inventory is maintained by hand; the claim covers named decisions under stated assumptions. |
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
| Commit mined queries verbatim | Data movement | Source text replicates to every clone of the tree. |
| A redactor written for goldens | One implementation of the rules | A second redactor diverges toward preserving more text. |
| Hosted trace collection for every run | Content | Primary data leaves the perimeter, beyond the store's deletion. |

Consequences: a zero in a report is a retrieval fact; spans never outlive the rows they describe.

## The model specifies the profile-to-effective-authority mapping, each target bound to one query path

`assurance.model` specifies the mapping from the delegation profile to effective authority. `assurance.prove` carries four targets: inclusion, narrowing, profile meaning and restriction preservation; read-path releases answer to the mediation relation; the rest of scoped-session execution stays outside every theorem. `assurance.prove` decides placement inclusion symbolically; deciding over sample values raises `ZoneInclusionSampled`. An unmodelled manifest category raises `UnmodelledConstructor`; a target bound to no authenticated query path raises `ProofTargetUnbound`.

| Option | Lost on | Cost |
| --- | --- | --- |
| The profile-to-authority mapping, symbolic, path-bound *(chosen)* | — | The delegation library, its cryptography and its parser are trusted dependencies the claim names. |
| A self-owned wire protocol's attenuation rules | Stability of the proof object | Every protocol correction re-opens the proofs. |
| The whole enforcement engine | Scope | The models define no SQL semantics, credential bytes or run-path effects. |
| Inclusion decided over probe zones | Coverage | A finite sample decides nothing over an unbounded identifier space. |
| Targets named without a path binding | Locatability | A reader cannot find the code a constant describes. |

Consequences: a defect in credential parsing or signature checks lies outside every theorem.
Revisit: the profile's maintainer changes the mapping's domain; a defect surfaces in the trusted remainder.

## Mediation ranges over statement execution, and restriction preservation is a proof target

`assurance.prove.mediation-relation` fixes the execution relation: the read path's statement execution, one step per pending row, releasing it when the session's fold admits it and dropping it otherwise. The induction shows every released row in a reachable state passed every session layer. Run steps reach stored rows through a read-path statement; their effects answer to the protocol model's fences and allowlist inclusion. `assurance.prove.restriction-preservation` makes restriction preservation the fourth target: every parent block admits what any attenuation chain permits.

| Option | Lost on | Cost |
| --- | --- | --- |
| Statement execution, plus restriction preservation over attenuation chains *(chosen)* | — | The relation abstracts SQL to a fold per row; the planner, joins and aggregates stay outside the model. |
| Engine run steps | Object | A run step releases no stored row except through a statement, so the induction proves nothing about rows. |
| Run steps and statement execution together | Budget | A joint relation needs the run journal and SQL in one model, beyond the zero-dependency package. |
| Restriction preservation left unproved | Coverage | A dropped parent block widens a descendant with no theorem to refuse it. |

Consequences: `MediationClaimedFromComposition` still refuses a list theorem offered as mediation; the mediation constant ranges over reachable states.
Revisit: the read path releases a row outside a statement; the model gains SQL semantics.

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

`assurance.model` holds the lease, compare-and-swap and fence protocol as an executable Lean 4 state machine over both lease paths: the compaction lease guarding a table pointer and the run lease guarding a commit log. Acquisition is a grant, then a raise. Safety is no lost update and no commit after a successor's acquisition; a commit between a successor's grant and raise lands below the highest grant, losing nothing. Five invariants are audited theorems; a bounded check over three nodes and four lease generations raises `ProtocolInvariantViolated`. `assurance.differential-test` drives the executable and the store's lease calls through generated sequences; a disagreement raises `ProtocolConformanceDrift`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Lean 4 executable model, conformance through the differential harness *(chosen)* | — | Invariant proofs are hand-written; generated interleavings are samples, exhaustive only inside the bounded check. |
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
| Count-first gate, trend tier, notes history *(chosen)* | — | The operator's `measure-publish` appends notes; a timing regression surfaces as an annotation. A missing baseline scans first-parent history; matching baselines stop the stream. |
| Separate baseline index | Extra state | An index needs synchronization with notes and reconstruction after missing updates; the stream preserves full-history comparisons across runner changes. |
| Gate on wall-clock p95 against a committed baseline | Determinism | Shared containers move p95 past any useful band; the check flakes until ignored. |
| History as a committed JSONL file | Branch policy | A bot commit on the default branch per run and a conflict with every open change. |
| History in an external artifact store | Offline verdicts and queries | A second store to operate, reachable only with network credentials. |

Consequences: a red evaluate stage reports correctness; timing movement remains visible per commit.
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

## A store format major moves the release minor

**Status:** accepted; amends the closed-milestone count above.

Context: a consumer pinning `~0.16` takes every `v0.16.*` tag as a patch, yet a patch can carry manifest format `2.0` and the commit counter marker, and a build reading `1.x` refuses a table either writes. Decision: the minor version adds each store format major past `1` to the closed-milestone count, the major read from `FormatVersion::CURRENT` at the tagged commit. The commit counter marker declares that same `format_version`, so it rides on the manifest major rather than counting apart. A tag whose format major differs from the last tag's under an unchanged minor is refused, and a tag raising the major says in its message that readers upgrade before writers. Criteria: a semver range never admits a store-format break; the version stays derivable from the tagged commit alone.

| Option | Lost on | Cost |
| --- | --- | --- |
| Minor adds the format major *(chosen)* | — | The minor no longer equals the closed-milestone count; the tag reads one Rust constant by its text. |
| Refuse a format break under an unchanged minor, numbering unchanged | Derivability | A maintainer must close a milestone, or wait, to release a format break. |
| A separate format field in the tag name | Semver tooling | Cargo and npm ranges ignore a build-metadata suffix. |

Consequences: 16 closed milestones under format `2.0` compute `v0.17.0`; the tag message names the reader-first upgrade order.
Revisit: the store format promises compatibility across majors, or `v1` opens.

## The workspace fits the stage wall clock through parts

**Status:** accepted; answers the test-first decision's revisit trigger, a base-commit build exceeding the stage wall clock.

Context: the feature-unified workspace suite exceeds the 1800 s sandbox cap on a cold build. Criteria: every package suite runs, the feature union compiles, and each part runs as one local command.

Decision: `assurance.gate.workspace-parts` compiles the union untested, runs `differential::` tests in one check, and splits the remaining CLI tests into eight shards by the map of `assurance.gate.cli-shard-map`: local four-thread durations scaled to a measured gate shard, a group of tests sharing one costly nested build on one shard. Four parts run the other suites; the local stage keeps one invocation under `assurance.build.one-engine-build`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Duration-balanced shards *(chosen)* | — | Each shard compiles the CLI suite cold, about 650 s; a new test runs unbalanced until the map is regenerated. |
| Four name-hashed shards | Stage wall clock | Each shard rebuilt both nested examples; three exceeded 2700 s. |
| One CLI test build handed to every shard | Operability | A part run in a fresh checkout receives no test executable another part built. |
| `cargo nextest --partition hash:k/4` | Operability | The gate image carries no nextest, which runs no doctests. |

Consequences: fourteen workspace checks cover the local stage's packages, each ordinary CLI test once. A nested example build inherits no package environment from the test process, so it reuses the shard's compiled graph. A shard over budget raises the shard count; a unit over it alone must shorten.

## Gate parts run under a 45 min exec deadline

**Status:** accepted

Context: a part building the binary cold and running its CLI suite takes 1250–1750 s; under a 1800 s deadline the slowest regularly end without a verdict. Each part repeats its own cold build. Criteria: every part reports a verdict, and the whole gate finishes soonest.

Decision: `assurance.gate.container` gives each part 45 min. The slowest passing part uses about 65% of it, and the part list stays as `assurance.gate.workspace-parts` and `assurance.build.profile-build` define it.

| Option | Lost on | Cost |
| --- | --- | --- |
| A 45 min exec deadline *(chosen)* | — | A hung part reports 15 min later. |
| Splitting each long part by test module | Gate wall clock | Each extra part adds a cold build of 550–760 s. |
| A shared build cache | Operability | A cache bucket and credential become part of the gate. |

Consequences: a part nearing 45 min splits under `assurance.gate.part-budget`.

## Feature runs, formal checks and profile budgets run in a nightly tier

**Status:** accepted

Context: the pre-merge gate runs about 6 h of wall clock; the features, formal and budget parts sum to over 2 h of it, each a cold build of its own feature set or release profile. Criteria: a pre-merge verdict within 30 min; every check stays runnable against the default-branch head; P7 holds. P7 rejects a nightly cadence for every assertion and keeps one for a check too costly per change, naming the change range.

Decision: `assurance.gate.nightly-tier` moves the features, formal and budget parts out of the pre-merge parts. `contextful-ci gate --stage <part>` runs each on demand against the default-branch head.

| Option | Lost on | Cost |
| --- | --- | --- |
| A nightly tier for features, formal and budget *(chosen)* | — | A feature-only, proof or footprint regression lands and reds the next run of its part, attributed to the change range since the last passing run rather than one change. |
| Every part per change | Gate wall clock | Thirteen extra cold builds on every pull request. |
| Per-change parts selected by changed paths alone | Detectability | A spec or dependency change reaches proofs and footprints no path filter names. |
| One shared build cache across parts | Operability | A cache bucket and credential become part of the gate, and feature runs still resolve separately. |

Consequences: a nightly part runs only when an operator invokes it; no scheduler runs it or opens an issue.

Revisit: pull-request parts share builds, so the nightly parts fit the 30 min verdict.

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
| No WASI release, a scheduled probe *(chosen)* | — | A function-class host runs the musl edge build or none; the probe's verdict waits for a scheduled-tier measure run. |
| Ship `wasm32-wasip2` for edge | Buildability | The engine's threads and file locks have no WASI preview-2 equivalent; every release blocks on a port. |
| Drop the target with no probe | Measured viability | Nobody learns when the engine's WASI support reaches the budget. |

Consequences: the accepted cost is that a WASI host stays unserved until the probe records a figure inside the budget and the matrix gains the target.

## Gate repair evidence binds one checkout and one comparison

**Status:** accepted.

Context: workspace artifacts retain checkout paths, merge history changes test selection, and a stage timeout without phase evidence obscures whether compilation or tests exhaust the budget. Criteria: each verdict identifies its source, comparison and failure class.

Decision: `assurance.build.checkout-artifacts` and `assurance.build.runtime-checkout` bind builds and helpers to invocation identity. `assurance.test.comparison-base`, `assurance.test.merge-source` and `assurance.test.base-evidence` bind test-first obligations to one comparison and distinguish infrastructure from regression evidence. `assurance.gate.stage-timings`, `assurance.gate.part-budget`, `assurance.gate.build-admission` and `assurance.gate.timeout-cleanup` bind resource evidence to parts.

| Option | Lost on | Cost |
| --- | --- | --- |
| Checkout isolation, one comparison, measured parts and owned cleanup *(chosen)* | — | Workspace artifacts repeat across checkouts; cold and warm validation consumes runner time; reports require phase boundaries. |
| Shared workspace artifacts with compile-time checkout paths | Source identity | A cached helper selects another checkout's source or binary. |
| Independent comparison rules for selection and replay | Regression evidence | A test runs against source outside the comparison that selected it. |
| Retry full gates or increase their timeout | Diagnosis and platform budget | Repeated cold builds provide no phase evidence and can exceed the sandbox cap. |

Consequences: integration and doctest contrasts, history fixtures, cold/warm part runs and forced descendant hangs provide independent acceptance controls.
Revisit: representative timings demonstrate checkout isolation or partitioning exceeds the declared resource budget.

## Windows release support is deferred independently of Linux and macOS delivery

**Status:** accepted.

Context: native Windows execution adds MSVC toolchains, executor registration, descendant-lifetime verification and bundled read-engine validation to the release boundary. Criteria: Linux and macOS delivery has a bounded platform scope independent of Windows executor availability.

Decision: `assurance.build.windows-target` and `assurance.gate.windows-checks` define the Windows exclusion. The supported release matrix remains owned by `assurance.build.targets`; Windows diagnostics preserve porting evidence without widening its delivery obligations.

| Option | Lost on | Cost |
| --- | --- | --- |
| Defer Windows releases and required native checks together *(chosen)* | — | Windows consumers receive no supported artifact; diagnostic failures carry no delivery verdict. |
| Require Windows execution before every merge | Platform scope | Linux and macOS delivery depends on Windows executor availability and port completion. |
| Exclude Windows artifacts but require Windows checks | Independent delivery | The release scope narrows while the same executor dependency remains. |

Consequences: Windows source and regression fixtures remain available for porting; support claims cover the declared release matrix alone.
Revisit: an explicit Windows support decision carries passing real MSVC lifetime, filesystem and bundled read-engine evidence for each admitted architecture within the gate budget.

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

Context: the toolchain stage reads the pins stage's record, and the formal stage reads the toolchain stage's environment. A part run in a fresh checkout holds no earlier stage's output, and a contributor reruns one stage locally. Criteria: a gate fails rather than skips (P7); a fresh run and a rerun invoke one command; a rerun reuses an output already on disk.

Decision: `assurance.gate.stage-subset` runs a selection in the sequence's order and refuses before any stage starts when a selected stage reads an unselected predecessor's output and that output is absent. `--predecessors` adds those predecessors for a run in a fresh checkout.

| Option | Lost on | Cost |
| --- | --- | --- |
| Sequence order, refuse an absent predecessor output *(chosen)* | — | Each fresh-checkout formal run reruns pins and toolchain. |
| Run the named stages alone, whatever is absent | Fail rather than skip | The formal stage runs without the pinned Lean toolchain and reports a verdict on another one. |
| Always add every predecessor | Rerun reuse | A local formal rerun repeats the pin fetch and provisioning each time. |
| Run in the order named | One command | Two contributors naming one subset differently run different gates. |

Consequences: a predecessor's output on disk satisfies its reader, so a stale record from an earlier run reaches the stage. The accepted cost: a fresh-checkout formal run pays the pins and toolchain stages every time.

## Generated artifacts prove currency by regeneration and diff

**Status:** accepted

Context: a generated file can retain a plausible header after its source changes. `spec/derived.toml` names each output and its generator. Criteria: the schema stage catches a stale committed byte without trusting a hand-maintained hash or a generator's success status alone.

Decision: `assurance.structure-tree.derivation-check` exports the committed tree, runs each declared check in that scratch tree, and compares each regenerated output byte for byte with its committed copy.

| Option | Lost on | Cost |
| --- | --- | --- |
| Regenerate and diff *(chosen)* | — | The schema stage pays for the generators and keeps a declaration for each output. |
| Compare a recorded source hash | Coverage | A new generator input can be omitted from the hash. |
| Trust a build-time export | Committed artifact currency | The build can succeed while the committed output remains stale. |

Consequences: a generator must produce stable bytes; nondeterministic output reds the schema stage.

## Duplicated capabilities match registered symbols and home constants by folded name

**Status:** accepted

Context: a second implementation of an engine capability can sit in a TypeScript gateway, a Python script or another crate, so no compiler sees both. Criteria: the lint names both sites, passes on the current tree, and needs no per-language parser beyond Rust.

Decision: `assurance.structure-tree.capability-registry` holds one home per capability with distinctive defining symbols, beside `spec/spec.lock.json`. `assurance.structure-tree.cross-language` folds names to lowercase without separators, so `singleStatement` meets `single_statement`; a constant matches on name and literal value together. Test, fixture, benchmark and vendored directories, `#[cfg(test)]` items, TypeScript type aliases and interfaces define nothing. Script class methods count as functions; bodiless signatures do not. Integer arithmetic over literals folds, and numbers take one spelling. A method of a Rust implementation of a trait declared under a registered port path is an adapter. A comment line opening with `mirrors:` among the annotation lines directly above admits a site, and `contextful-ci mirrors` resolves those lines.

| Option | Lost on | Cost |
| --- | --- | --- |
| Registered symbols and home constants, folded names *(chosen)* | — | The registry is maintained by hand; a renamed copy or a constant with another name passes. |
| Every public engine name | Precision | Generic names such as `resolve` collide across unrelated modules. |
| Structural similarity over syntax trees | Explainability | A per-language parser and a threshold that flags refactors. |
| Constant values alone | Precision | Small integers and common strings recur in unrelated code. |

Consequences: a registry symbol renamed away from its home raises `CapabilityHomeUnresolved` rather than leaving the check vacuous.
Revisit: a copy under another name escapes to production.

## Backtests estimate leakage against a reference, and their tracks never pool

**Status:** accepted

Context: a model can remember outcomes its weights saw, and recency alone lifts accuracy near a cutoff. Criteria: a reported leak survives a model that cannot have seen the outcome; one figure answers one question.

Decision: `assurance.backtest.cutoff-slice` states leakage against a declared reference, a cutoff discontinuity or a clean control model. `assurance.forecast.tracks-separate` keeps historical, prospective, tool-free and tool-using scores apart. Each report stamps execution metadata (`assurance.backtest.execution-component`). Consumer-selected financial profiles own return-series scoring, price fills and execution parameters (`assurance.backtest.trial-ledger`, `assurance.backtest.fill-instant`); prediction-market profiles supply probability-price baselines (`assurance.forecast.market-context`). Decoding corrections belong to the consumer, reading `connector.infer.logprobs` and the run record.

| Option | Lost on | Cost |
| --- | --- | --- |
| Reference-based estimate, separate tracks, declared execution metadata, optional domain profiles *(chosen)* | — | Every report declares a reference and an execution component; a consumer selects sector-specific scoring; small tracks report wide intervals. |
| Bare pre/post-cutoff gap | Survives a clean model | Recency reads as leakage near every cutoff. |
| One pooled score across tracks | One figure, one question | A tool-using backtest lifts a prospective forecaster's rank. |
| Costs inside the platform | One home per capability | Every venue's cost model becomes an engine release. |
| Platform decoding corrections | Consumer choice | One correction fixed for every model and question. |

Consequences: a report without a reference or an execution component refuses; a consumer correcting decoding reads the landed log-probabilities. Financial helper functions retain their public names and apply only to their selected profile.

## The case's judge scores leakage against a pre-horizon reference

`assurance.backtest.leak-score` runs in the platform: the case's own judge model scores each retrieved document 0 to 4 against a reference answer drawn from rows available before the horizon, and the run lands one row per document and horizon. The score feeds the Brier-per-tier report and the zero threshold a late snapshot capture answers under.

| Option | Lost on | Cost |
| --- | --- | --- |
| The case's judge against a pre-horizon reference *(chosen)* | — | One judge call per retrieved document. |
| Consumer-landed scores | Comparability | Two runs of one case score leakage by different rules. |
| A fixed classifier | Domain | A generic classifier misses domain-specific outcomes. |

## A pull request runs the parts its diff reaches

**Status:** accepted

Context: running every pre-merge part, each repeating its own cold build, makes a prose change wait hours behind binary feature suites. Criteria: no change skips a part its diff can reach, and the selection runs locally.

Decision: `assurance.gate.affected-parts` lists the required parts the diff from the merge base reaches. Build inputs and unmapped paths reach every part under `assurance.gate.affected-every`; spec, TypeScript and Lean paths reach their own parts.

| Option | Lost on | Cost |
| --- | --- | --- |
| A path map with a dependency closure through pins, every unknown reaching every part *(chosen)* | — | A new top-level directory reaches every part until a clause maps it. |
| A cargo dependency graph per changed package | Locality | Every part learns which packages it compiles; a lock change still reaches everything. |
| Every part on every change | Gate wall clock | A spec-only change waits on hours of builds. |

Consequences: a change confined to `spec/`, `references/`, `apps/`, `packages/` or `formal/` runs a fraction of the parts. A part the map omits wrongly stays green until a change under `crates/` or `tools/` runs it.
