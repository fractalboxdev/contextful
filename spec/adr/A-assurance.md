# A-assurance — Assurance decisions

**Status:** accepted

## consumer-compatibility-binds — Consumer compatibility binds execution evidence to artifact bytes

A manifest parse establishes neither replay behavior nor receiver durability, and a release version does not establish source correspondence. A consumer reproduces the verdict outside the checkout with no inherited provider account.

| Option | Lost on | Cost |
| --- | --- | --- |
| Distributable public-surface scenarios *(chosen)* | — | Native execution requires a matching host and synthetic fixture capabilities. |
| Source-only integration tests | Consumer reproducibility | Consumers need the checkout and build toolchain. |
| Manifest and version checks | Behavioral evidence | Parsing hides replay and acknowledgement defects. |

## assurance-claims-carry — Assurance claims carry their qualifiers, and the proof gate audits assumptions

A hand-maintained inventory checks each constant's transitive assumption footprint against a two-entry allowlist. The package holds no dependency, so two machines reach one verdict offline.

| Option | Lost on | Cost |
| --- | --- | --- |
| Inventory plus transitive assumption audit, qualified claims, zero-dependency package *(chosen)* | — | Every lemma is hand-rolled; the inventory is maintained by hand. |
| A source pattern over package files | Catching a proof of `False` | A parenthesized hole or a respelled tactic passes. |
| Build and read the exit status | Catching a proof of `False` | A hole elaborates as a warning and exits zero. |

Revisit: a mathematics library elaborates inside the per-change budget.

## evaluation-data-follows — Evaluation data follows store policy

An evaluation that bypasses enforcement never shows an over-restrictive policy as a recall regression. A zero in a report is then a retrieval fact.

| Option | Lost on | Cost |
| --- | --- | --- |
| Labeled corpora, two-guard golden isolation, store redaction, in-perimeter traces *(chosen)* | — | Labeling precedes the first number; a collector runs per environment. |
| Bypass enforcement for evaluation runs | Coverage | An over-restrictive policy never surfaces as a recall regression. |
| Hosted trace collection for every run | Content | Primary data leaves the perimeter, beyond the store's deletion. |

## model-specifies-profile — The model specifies the profile-to-effective-authority mapping, each target bound to one query path

Proving a self-owned wire protocol re-opens every proof on each protocol correction. The delegation library, its cryptography and its parser are trusted dependencies the claim names.

| Option | Lost on | Cost |
| --- | --- | --- |
| The profile-to-authority mapping, symbolic, path-bound *(chosen)* | — | A defect in credential parsing or signature checks lies outside every theorem. |
| A self-owned wire protocol's attenuation rules | Stability of the proof object | Every protocol correction re-opens the proofs. |
| The whole enforcement engine | Scope | The models define no SQL semantics, credential bytes or run-path effects. |

Revisit: the profile's maintainer changes the mapping's domain; a defect surfaces in the trusted remainder.

## mediation-ranges-over — Mediation ranges over statement execution, and restriction preservation is a proof target

A run step reaches stored rows only through a read-path statement, so statement execution is the object the induction ranges over. Run-step effects answer to the protocol model's fences.

| Option | Lost on | Cost |
| --- | --- | --- |
| Statement execution, plus restriction preservation over attenuation chains *(chosen)* | — | The relation abstracts SQL to a fold per row; the planner, joins and aggregates stay outside. |
| Engine run steps | Object | A run step releases no stored row except through a statement. |
| Run steps and statement execution together | Budget | A joint relation needs the run journal and SQL in one model. |

Revisit: the read path releases a row outside a statement; the model gains SQL semantics.

## differentially-tested-reference — A differentially tested reference model is the standing link between proofs and binary

A refinement proof through a broken translator passes about the wrong object and leaves nothing to inspect. A differential leaves a minimized case.

| Option | Lost on | Cost |
| --- | --- | --- |
| Differential reference model, refinement scoped to pure functions *(chosen)* | — | Evidence, not equivalence; a case outside the generator's classes goes uncovered. |
| Refinement proof alone | Survivability of evidence | A broken translator passes about the wrong object. |
| Reporting disagreements without retaining them | Regression retention | A fixed divergence returns under another seed. |

Revisit: a pinned, reproducible translation toolchain exists; a production divergence falls outside the malformed, boundary and well-formed classes.

## executed-reference-proved — The executed reference model is the proved definitions

A disagreement implicates the proved definitions directly, and every theorem over `Authority.Grant` speaks about the function the harness executes.

| Option | Lost on | Cost |
| --- | --- | --- |
| One set of definitions, proved and executed *(chosen)* | — | A definition that misreads a clause passes its proofs and its differential alike. |
| A second Lean rendering written from the specification text | Proof reach | No theorem links the executed types to the proved ones. |
| A refinement proof between the two renderings | Maintenance cost | Every grant dimension needs a simulation lemma across two type families. |

Revisit: a misreading of a clause reaches a release through a definition both proofs and differential accepted.

## store-protocol-modelled — The store protocol is modelled in Lean 4

A commit between a successor's grant and raise lands below the highest grant and loses nothing. A theorem proves each invariant for every lease generation, where a model checker bounds it.

| Option | Lost on | Cost |
| --- | --- | --- |
| Lean 4 executable model, conformance through the differential harness *(chosen)* | — | Invariant proofs are hand-written; generated interleavings are samples. |
| Quint with quint-connect | One formal language | A second specification language, and a v0.1 bridge crate from one vendor. |
| TLA+ with a harness over ITF traces | Maintained Rust bridge | Action dispatch, state projection and comparison are built and kept in a second language. |

Revisit: the bounded check exceeds the formal stage's wall clock; a maintained Rust bridge to a model checker reaches the protocol's step vocabulary.

## columnar-read-statement — The columnar-read and statement-serialization functions link into every engine-linked build

An autoloaded extension brings a second copy of the engine's type information. A cast between copies aborts, and on Apple platforms corrupts reads.

| Option | Lost on | Cost |
| --- | --- | --- |
| Link both function sets into every engine-linked build *(chosen)* | — | 55 MiB on every engine-linked binary, the read replica included. |
| Autoload on first use | Correctness | Two type tables abort one platform and corrupt another. |
| Reimplement both capabilities | Correctness | The guard passes statements the engine parses differently. |

Revisit: the engine resolves extensions against the host binary's type tables; a profile's footprint budget cannot absorb 55 MiB.

## source-change-carries — A source change carries a test that fails first, and a milestone carries its acceptance test before it progresses

A test written after its code is textually identical to one written first, so only a run against the base commit tells them apart.

| Option | Lost on | Cost |
| --- | --- | --- |
| Red-before-green over the diff, acceptance test before the first pin *(chosen)* | — | Every source change pays a second build at the base commit; a refactor declares itself by trailer. |
| A line-coverage threshold | Order | A test written after its code satisfies it. |
| Per-change mutation testing | Wall clock | Minutes per mutated function across an engine-sized crate graph. |

Revisit: the base-commit build exceeds the stage wall clock; the refactor trailer appears on changes that alter a public surface.

## tracked-targets-gate — Tracked targets gate on counts and record per commit in notes

**Status:** proposed

Wall-clock and resident-memory figures move on shared containers, so they annotate and gate nothing. Gate-tier targets measure counts, within-run ratios or locked-resolve sizes.

| Option | Lost on | Cost |
| --- | --- | --- |
| Count-first gate, trend tier, notes history *(chosen)* | — | A timing regression surfaces as an annotation; a missing baseline scans first-parent history. |
| Gate on wall-clock p95 against a committed baseline | Determinism | Shared containers move p95 past any useful band. |
| History as a committed JSONL file | Branch policy | A bot commit per run conflicts with every open change. |

Revisit: a dedicated runner class holds p95 within 5 percent across runs.

## version-tag-names — A version tag names a gated revision and counts closed milestones

A consumer pinning a bare commit reads no promise. Derivability from computed state decided the scheme, so no mislabelled tag can be cut. Below `v1` no surface promises compatibility.

| Option | Lost on | Cost |
| --- | --- | --- |
| Computed version, maintainer-invoked subcommand *(chosen)* | — | Tagging is a human step; a version bump commit precedes each tag. |
| A tag cut by hand | Derivability | A version claims milestones its revision does not pass. |
| Minor equal to the highest milestone number | Monotonic meaning | Milestone 11 closing before 8 claims milestones never reached. |

Revisit: an external consumer needs a compatibility promise on a public surface, which opens `v1`.

## store-format-major — A store format major moves the release minor

A consumer pinning `~0.16` takes every `v0.16.*` tag as a patch, yet a build reading format `1.x` refuses a table format `2.0` writes. The commit counter marker declares the same `format_version`, so it counts with the manifest major. The bucket manifest's `MANIFEST_FORMAT` counts the same way, since a build parsing manifest format `1` refuses format `2`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Minor adds the format major *(chosen)* | — | The minor no longer equals the closed-milestone count; the tag reads one Rust constant by its text. |
| Refuse a format break under an unchanged minor | Derivability | A maintainer closes a milestone, or waits, to release a format break. |
| A separate format field in the tag name | Semver tooling | Cargo and npm ranges ignore a build-metadata suffix. |

Consequences: 16 closed milestones under format `2.0` compute `v0.17.0`; the tag message names the reader-first upgrade order.
Revisit: the store format promises compatibility across majors, or `v1` opens.

## workspace-fits-stage — The workspace fits the stage wall clock through parts

The feature-unified workspace suite exceeds the 1800 s sandbox cap on a cold build. Tests sharing one costly nested build stay on one shard.

| Option | Lost on | Cost |
| --- | --- | --- |
| Duration-balanced shards *(chosen)* | — | Each shard compiles the CLI suite cold, about 650 s; a new test runs unbalanced until the map regenerates. |
| Four name-hashed shards | Stage wall clock | Each shard rebuilt both nested examples; three exceeded 2700 s. |
| One CLI test build handed to every shard | Operability | A part in a fresh checkout receives no executable another part built. |

## feature-runs-formal — Feature runs, formal checks and profile budgets run in a nightly tier

The features, formal and budget parts each need a cold build of their own feature set or profile. P7 keeps a nightly cadence for a check too costly per change, naming the change range.

| Option | Lost on | Cost |
| --- | --- | --- |
| A nightly tier for features, formal and budget *(chosen)* | — | A regression reds the next run of its part, attributed to a change range rather than one change. |
| Every part per change | Gate wall clock | Thirteen extra cold builds on every pull request. |
| Per-change parts selected by changed paths alone | Detectability | A spec or dependency change reaches proofs and footprints no path filter names. |

Revisit: pull-request parts share builds, so the nightly parts fit a 30 min verdict.

## decision-module-compiles — One decision module compiles native and to WebAssembly

One verdict computed from two sources drifts. The gateway needs verification, admission and placement, never SQL, under a host with no JavaScript. The module imports only the credential library's evaluator clock.

| Option | Lost on | Cost |
| --- | --- | --- |
| The policy package, the credential library's clock as the one import *(chosen)* | — | A 2.7 MB module; a credential-library release may rename the imports the host links. |
| The domain crate alone, verification outside the module | Gateway need | The gateway verifies signatures from a second source. |
| The credential library's JavaScript bindings | Host | The differential host and a native embedder run no JavaScript. |

## profile-graphs-answer — Profile graphs answer to cargo-deny bans, one run per profile feature

One walk over workspace-unified metadata cannot tell which profile pulled a crate. A red check follows a change, not the calendar, so advisories stay out.

| Option | Lost on | Cost |
| --- | --- | --- |
| `bans` per profile feature, pinned binary *(chosen)* | — | Three resolutions per run; an advisory against a locked crate is never reported. |
| One run over default features | Profile attribution | The CRDT library in `full` hides behind `control`'s admission. |
| `bans`, `advisories` and `licenses` together | Change-driven verdicts | An advisory published overnight reds an untouched pull request. |

## edge-profile-ships — The edge profile ships no WASI release, and a scheduled probe measures it

The SQL engine relies on threads, file locks and a statically linked extension set that `wasm32-wasip2` does not provide. A target's viability is a measured figure.

| Option | Lost on | Cost |
| --- | --- | --- |
| No WASI release, a scheduled probe *(chosen)* | — | A WASI host stays unserved until the probe records a figure inside the budget. |
| Ship `wasm32-wasip2` for edge | Buildability | Every release blocks on a port of threads and file locks. |
| Drop the target with no probe | Measured viability | Nobody learns when WASI support reaches the budget. |

## gate-repair-evidence — Gate repair evidence binds one checkout and one comparison

Workspace artifacts retain checkout paths, and a timeout without phase evidence hides whether compilation or tests exhausted the budget. Each verdict identifies its source, comparison and failure class.

| Option | Lost on | Cost |
| --- | --- | --- |
| Checkout isolation, one comparison, measured parts *(chosen)* | — | Workspace artifacts repeat across checkouts; reports require phase boundaries. |
| Shared workspace artifacts with compile-time checkout paths | Source identity | A cached helper selects another checkout's source or binary. |
| Retry full gates or increase their timeout | Diagnosis | Repeated cold builds provide no phase evidence. |

Revisit: representative timings show checkout isolation or partitioning exceeds the declared resource budget.

## windows-release-support — Windows release support is deferred independently of Linux and macOS delivery

Native Windows execution adds MSVC toolchains, executor registration and descendant-lifetime verification to the release boundary.

| Option | Lost on | Cost |
| --- | --- | --- |
| Defer Windows releases and required native checks together *(chosen)* | — | Windows consumers receive no supported artifact. |
| Require Windows execution before every merge | Platform scope | Linux and macOS delivery depends on Windows executor availability. |
| Exclude Windows artifacts but require Windows checks | Independent delivery | The same executor dependency remains. |

Revisit: a Windows support decision carries passing MSVC lifetime, filesystem and read-engine evidence for each admitted architecture within the gate budget.

## stage-subset-runs — A stage subset runs in sequence order and refuses a missing predecessor

A part in a fresh checkout holds no earlier stage's output, and a gate fails rather than skips (P7). A predecessor's output already on disk satisfies its reader.

| Option | Lost on | Cost |
| --- | --- | --- |
| Sequence order, refuse an absent predecessor output *(chosen)* | — | A fresh-checkout formal run reruns pins and toolchain. |
| Run the named stages alone, whatever is absent | Fail rather than skip | The formal stage reports a verdict on an unpinned Lean toolchain. |
| Always add every predecessor | Rerun reuse | A local rerun repeats the pin fetch and provisioning. |

## generated-artifacts-prove — Generated artifacts prove currency by regeneration and diff

A generated file can keep a plausible header after its source changes. Neither a hand-maintained hash nor a generator's exit status shows a stale committed byte.

| Option | Lost on | Cost |
| --- | --- | --- |
| Regenerate and diff *(chosen)* | — | The schema stage pays for the generators; nondeterministic output reds it. |
| Compare a recorded source hash | Coverage | A new generator input can be omitted from the hash. |
| Trust a build-time export | Committed artifact currency | The build succeeds while the committed output is stale. |

## duplicated-capabilities-match — Duplicated capabilities match registered symbols and home constants by folded name

A second implementation can sit in a TypeScript gateway or a Python script, where no compiler sees both. Folding `singleStatement` to `single_statement` needs no parser beyond Rust.

| Option | Lost on | Cost |
| --- | --- | --- |
| Registered symbols and home constants, folded names *(chosen)* | — | The registry is maintained by hand; a renamed copy passes. |
| Every public engine name | Precision | Generic names such as `resolve` collide across unrelated modules. |
| Structural similarity over syntax trees | Explainability | A per-language parser and a threshold that flags refactors. |

Revisit: a copy under another name escapes to production.

## backtests-estimate-leakage — Backtests estimate leakage against a reference, and their tracks never pool

Recency alone lifts accuracy near a cutoff, so a reported leak survives a model that cannot have seen the outcome. Consumer-selected profiles own return scoring, price fills and decoding corrections.

| Option | Lost on | Cost |
| --- | --- | --- |
| Reference-based estimate, separate tracks, declared execution metadata *(chosen)* | — | Every report declares a reference and an execution component; small tracks report wide intervals. |
| Bare pre/post-cutoff gap | Survives a clean model | Recency reads as leakage near every cutoff. |
| One pooled score across tracks | One figure, one question | A tool-using backtest lifts a prospective forecaster's rank. |

## case-s-judge — The case's judge scores leakage against a pre-horizon reference

The score feeds the Brier-per-tier report and the zero threshold a late snapshot capture answers under.

| Option | Lost on | Cost |
| --- | --- | --- |
| The case's judge against a pre-horizon reference *(chosen)* | — | One judge call per retrieved document. |
| Consumer-written scores | Comparability | Two runs of one case score leakage by different rules. |
| A fixed classifier | Domain | A generic classifier misses domain-specific outcomes. |

## pull-request-runs — A pull request runs the parts its diff reaches

Running every part makes a prose change wait hours behind binary feature suites. No change skips a part its diff can reach.

| Option | Lost on | Cost |
| --- | --- | --- |
| A path map with a dependency closure through pins, every unknown reaching every part *(chosen)* | — | A part the map omits wrongly stays green until a change under `crates/` or `tools/` runs it. |
| A cargo dependency graph per changed package | Locality | A lock change still reaches everything. |
| Every part on every change | Gate wall clock | A spec-only change waits on hours of builds. |

## protocol-package-holds — The protocol package holds the journal, merge and fold machines

`run.journal` and `store.merge` carry the engine's concurrency risk. One toolchain and one audit cover them, beside five policy modules.

| Option | Lost on | Cost |
| --- | --- | --- |
| Both machines in `formal/protocol` *(chosen)* | — | The bounded checks share the formal stage's wall clock. |
| One Lean package per machine | One audit | Three inventories, toolchain pins and executables. |
| Machines in `formal/Contextful` | Scope | The policy package gains IO-shaped state. |

Consequences: `no_dangling_blob` holds under the grace hypothesis; `--no-grace` reproduces `BlobMissing` on every adapter.
