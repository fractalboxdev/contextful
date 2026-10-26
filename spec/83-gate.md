---
contract: assurance
owns:
  - automate
  - gate
  - measure
  - release
---

# The gate and the release tag

How `contextful-ci` checks a change and cuts a release: typed subcommands, the gate's stages
in run order, its parts, folds and shards, the container's ceilings and footprint budgets,
the target ledger the evaluate stage reads, and the signed release tag with the local
packaging that follows it. Each operation keeps its `assurance` clause ids.

## automate

Typed subcommands over compiled binaries, and the boundary shell keeps.

- `typed-subcommand` — An automation step carrying a branch, a loop or a retry, or running past 5 lines, is a typed subcommand shelling out to compiled binaries.
  *P7*
- `subcommand-surface` — A subcommand exposes typed options, generated help and an entry point a test calls directly.
- `one-path` — The gate invokes the subcommand a contributor invokes.
  *because a local reproduction and a gate run then execute one code path*
- `unchecked-shell` — A shell file in the tree that `shellcheck` rejects raises `ShellCheckFailed` in the toolchain stage.
  *P7*
- `test-shellcheck` — A gate stage starting a test process over a tree tracking a shell file puts the `shellcheck` {{assurance.automate.unchecked-shell}} runs first on that process's `PATH`.
  *because a suite asserting a script passes `shellcheck` otherwise depends on whether the sandbox image ships the linter*
- `build-time-toolchain` — The automation toolchain links into no build profile and enters no release artifact.

## gate

Stage order, secrets of record, the crate-graph, row-token, egress and dependency rules, the formal stage, the container's ceilings, disk, footprint budgets and surface checks.

- `stage-sequence` — The gate runs its stages in order — pins, toolchain, schema, test-first, workspace, acceptance, evaluate, features, crate graph, connectors, TypeScript surfaces, formal, unwired, budget — all of both tiers when none is named, else the named subset, which `--plan` prints.
  *A-assurance#stage-subset-runs*
- `nightly-tier` — `contextful-ci stages --parts --tier nightly` prints the features, formal, unwired and budget parts in run order; `stages --parts` prints the remaining parts, those a change runs before merge, and `gate --stage` runs a part of either tier.
  *A-assurance#feature-runs-formal*
- `stage-subset` — A selected subset runs in the sequence's order; a stage reading an unselected predecessor's output, with that output absent, raises `StagePredecessorMissing`, naming both stages, before any stage runs.
  *A-assurance#stage-subset-runs*
- `test-first-parts` — The test-first parts require a changed test in each changed source package under {{assurance.test.test-first}}; each changed source package's part checks only its own changed tests against the base.
  *because one base build per package fits the sandbox's wall clock more reliably*
- `workspace-parts` — The workspace parts compile the feature-unified workspace untested, run differential CLI tests apart and remaining CLI tests in eight shards under {{assurance.gate.cli-shard-map}}, and each other non-acceptance package suite in one of four groups.
  *A-assurance#workspace-fits-stage*
- `cli-shard-map` — `tools/ci/cli-shards.toml` assigns each remaining CLI test, a nested-build group whole, one shard by recorded duration within 9 min per shard; an unmapped test, reported as drift, joins its group or the least-loaded shard, which adds unit tests and doctests.
  *A-assurance#workspace-fits-stage*
- `folded-parts` — `stages --parts` folds pins, toolchain, schema and test-first validation into `preflight`; each package group's workspace and evaluate parts into `suite.<group>`, probes into `suite.ci`; crate graph, connectors and surfaces into `perimeter`; members stay runnable alone.
  *because every part pays its own sandbox setup and compile, and a fold pays it once for every member*
- `compile-only` — `gate --stage <part> --compile-only` runs each `cargo test` invocation the part runs, in its own target directory with `--no-run` and without its `--` arguments, and nothing else; a CLI shard compiles every shard's invocations.
  *because a compile cache serves a part only when the warming build's cargo arguments, environment and target directory match the part's*
- `compile-groups` — `stages --parts --compile-groups` lists, in run order, the first part of each distinct compile signature — its ordered `--compile-only` invocations and target directory — across both tiers unless `--tier` names one.
  *because the eight CLI shards share one signature, and warming each compiles it eight times*
- `fold-verdicts` — A fold runs each member after an earlier member fails, skips a member reading a failed member's output, and fails when any member fails.
  *because one fold check replaces a check per member, and one run reports every member's verdict*
- `stage-reports` — Each stage prints the environment it leaves and its memory limit, peak and event counts, and a failing stage prints its diagnostics before propagating its exit code.
  *because memory exhaustion is silent, and a kill then reads as a number in the log*
- `part-budget` — Every part completes within {{assurance.gate.container}} on cold and warm builds; oversized work splits into locally runnable parts retaining every required suite and predecessor obligation.
  *A-assurance#gate-parts-run*
- `pins-stage` — The pins stage resolves every pinned artifact identity a run depends on before any compilation.
- `evaluate-stage` — The evaluate stage runs every gate-tier ledger entry and the native case set in the deterministic tier, one part per package group plus a probe part, each reporting the floor and baseline verdicts over its entries.
- `schema-stage` — The schema stage regenerates each derived artifact into a scratch location, compares it byte for byte against the committed copy, and runs `contextful-spec lint`.
- `secret-ciphertext` — In the schema stage, a key other than `DOTENV_PUBLIC_KEY*` in a git-tracked `.env*` file other than `.env.example` holding a value without the `encrypted:` prefix, or a tracked `.env.keys`, raises `SecretPlaintext`, naming file and key.
  *because a tracked file reaches every clone, and only dotenvx ciphertext is safe there while its private key stays untracked*
- `secret-scope` — In the schema stage, a key other than `DOTENV_PUBLIC_KEY*` in a git-tracked `.env*` file whose block — the assignments above it, up to the first blank line — opens with no comment carrying text raises `SecretScopeMissing`, naming file and key.
  *because a provider token cannot report its own grants, so the comment above it is the one record of what it reaches*
- `formal-stage` — The nightly formal stage runs {{assurance.audit-assumptions.check-command}}, {{assurance.differential-test.command}}, {{assurance.model.protocol-check}}, the protocol differential over 10000 generated cases on both lease paths, and {{assurance.model.journal-check}}; a non-zero exit from any reds the run.
  *P7*
- `typescript-surfaces` — The TypeScript surfaces run each declared typecheck, test and build script in one stage; an absent script skips only that script.
- `native-test-files` — The surfaces stage runs every regular native `.test.ts` file under a direct TypeScript surface's `test/` or `tests/` tree through Node's TypeScript strip runner, and refuses absent passing tests or pinned names.
  *because a package script can omit a pinned test while reporting success*
- `surface-check-failed` — A surface whose typecheck, unit tests or framework build fails raises `SurfaceCheckFailed`, naming the surface and the script.
  *P7*

### affected parts

`stages --affected` maps a diff to the parts it reaches.

- `affected-parts` — `contextful-ci stages --parts --base <rev> --affected` lists, in run order, the parts of the selected tier the diff from the merge base of `<rev>` and HEAD reaches, the required tier always including `preflight`.
  *A-assurance#pull-request-runs*
- `affected-every` — A change to `Cargo.toml`, `Cargo.lock`, a toolchain file, anything under `crates/`, `tools/` or `.github/`, or any path no other affected clause maps reaches every part of either tier.
  *A-assurance#pull-request-runs*
- `affected-spec` — A change under `spec/` or `references/` reaches test-first validation, one under `spec/` the foundation workspace part, and each clause whose statement or `spec/pins.toml` entry changed the parts running its pinned test or theorem.
  *A-assurance#pull-request-runs*
- `affected-spec-inputs` — A change to `spec/01-topology.md` or `spec/capabilities.toml` reaches crate-graph, to `spec/adr/A-assurance.md` the formal stage, and to `spec/terms/assurance.toml` every budget part.
  *because those stages read the file directly, apart from any clause pin*
- `affected-removed` — A clause the merge base's lock holds and HEAD's lacks reaches the evaluate stage.
  *because the evaluate stage refuses a ledger entry naming a clause the lock lacks*
- `affected-unplaced` — An unreadable spec lock or pins file, or a changed clause pinned outside every workspace package, reaches every required part.
  *because a pin the listing cannot place runs on a part the listing cannot name*
- `affected-surfaces` — A change under `apps/` or `packages/` reaches the TypeScript surfaces, acceptance and crate-graph parts.
  *A-assurance#pull-request-runs*
- `affected-formal` — A change under `formal/` other than a `lean-toolchain` file reaches the formal and evaluate stages, the foundation, CLI and CLI differential workspace parts, and every feature part.
  *A-assurance#pull-request-runs*

### dependency rules

The crate-graph stage holds the dependency graph and runtime source to their rules.

- `crate-graph` — The crate-graph stage holds run-path crates to reaching read-path crates through the three crossing crates alone; another edge raises `CrateGraphViolation`, naming both crates.
  *P5*
- `locked-resolve` — The crate-graph stage resolves every graph `--locked` against the committed `Cargo.lock`, so a lock behind its manifests, or one rewritten before the stage starts, fails the stage before any rule runs; the stage never rewrites it.
  *because a rule held over a graph the lock file does not record passes a build that resolves another*
- `row-token` — Every row-returning function of the store crate takes the enforcement token type, whose constructor is private to the enforcement module; a row path outside a registered relation raises `EnforceUnmediatedPath`, naming the function.
  *P5*
- `dependency-deny` — The crate-graph stage runs pinned cargo-deny `check bans` over each profile's resolved graph, denying every crate a profile-wide dependency refusal names; a hit raises that refusal's error, naming the profile and the path.
  *A-assurance#profile-graphs-answer*
- `deny-profile-graph` — A profile's graph is the binary resolved with default features off and the profile's feature on, across every target, excluding development dependencies; the stage prints each profile the binary declares no feature for.
  *A-assurance#profile-graphs-answer*
- `deny-list` — `deny.toml` holds one `[bans]` entry per crate or feature a profile-wide dependency refusal names, its reason that refusal's clause id; an entry missing or extra raises `DenyListDrift`, naming it.
  *because a refusal whose crate the list omits passes every profile graph linking it*
- `unconfigured-egress` — An outbound call reachable under the default configuration — usage ping, license check, update probe — raises `UnconfiguredEgress` in the crate-graph stage, naming the call site.
  *because some deployments run with no external reach, and a call nobody configured moves data outside every grant, zone and audit record*
- `interpolated-claim` — A source lint over the runtime crates finding a subject claim formatted into SQL text raises `EnforceInterpolatedSubjectClaim`, naming the file and line.
  *P1*

### footprint

Build resources and each profile's artifact size stay within their bounds.

- `container` — The gate container carries four ceilings — processor count, 12 GiB of memory, 18 GiB of usable disk, and a 45 min per-stage wall clock — and a run dies on any one.
  *A-assurance#gate-parts-run*
- `parallel-jobs` — Parallel build jobs number the memory ceiling divided by 3 GiB.
- `free-disk` — A stage starting with less than 2 GiB of free disk raises `BuildDiskPrecondition` and exits 28 before doing work.
  *P7*
- `edge-budget` — The edge profile compresses to at most 50 MiB.
- `full-budget` — The full profile compresses to at most 150 MiB.
- `control-budget` — The control profile compresses to at most 60 MiB.
- `footprint` — Per nightly run and per profile, the footprint step builds the static-linked Linux target, compresses it, and holds its size to the profile's budget and its dynamic dependencies to the platform C library.
  *P7*
- `budget-stage` — The budget stage runs {{assurance.gate.footprint}} over every profile, in one nightly-tier part per profile, and the evaluate stage builds no profile.
  *because one link-time-optimized release build of the full profile fills most of one stage's wall clock*
- `footprint-exceeded` — An artifact over its profile's budget, or carrying a dynamic dependency beyond the platform C library, raises `FootprintBudgetExceeded`, naming the profile.
  *P7*

unsettled: Which workload, cadence and drift bound does the idle-resident soak run under, given that a multi-day soak fits no per-change gate? owner: build affects: assurance.gate

unsettled: Which cache hit rate holds `test-first` and `workspace` under the per-stage wall clock once the `Cargo.lock`-keyed build cache serves the gate container (issue 31)? owner: build affects: assurance.gate

## measure

The target ledger: each tracked target, the clause it serves, how it is measured and its record per commit.

- `ledger` — `evals/ledger.toml` holds one entry per tracked target: an id, an owning clause id, a run-report metric path, a tier, a method naming one integration test, case set or probe, and a threshold.
- `unresolved-entry` — An entry whose owning clause, method or metric path resolves to nothing raises `MeasureEntryUnresolved` before any measure runs.
  *P7*
- `open-entry` — An entry naming an issue in place of a method reports open and gates nothing, and `evals/ledger.md` carries every entry's computed status.
- `tier` — A gate-tier entry decides the evaluate stage, a trend-tier entry decides nothing, and `contextful-ci measure` runs the gate tier alone unless `--tier` names `trend`, `scheduled` or `all`.
- `judged-tier` — An entry whose metric names a judged figure is scheduled-tier; on another tier it resolves to nothing under {{assurance.measure.unresolved-entry}}.
  *because a judged figure costs a model call per item and moves with the judge, not with the change*
- `count-first` — A gate-tier entry measures a count, a ratio within one run or a size under a locked resolve; a wall-clock or resident-memory figure is trend-tier.
  *because a shared container moves wall-clock figures past any band narrow enough to catch a regression*
- `record` — A measure writes one JSON record carrying its entry id, value, sample count, seed and run stamp; a gate-tier method finishing without one raises `MeasureRecordMissing`.
  *P7*
- `seeded` — Every generated fixture and randomized schedule derives from the record's seed, and replaying that seed reproduces a count-valued entry's value.
  *because a failure that cannot replay cannot be fixed*
- `seed-mismatch` — A gate or scheduled record whose seed differs from its entry's declared seed raises `MeasureSeedMismatch` and counts as red; a trend mismatch leaves no comparable record.
  *because a figure measured under another seed replays nothing the ledger names*
- `timing-iterations` — A timed batch reports p50 and p95 over 200 repeats of its operation, each after warm-up.
- `timing-batches` — A timed figure is the median of 5 repeats of its timed batch.
- `trend-direction` — Each trend-tier ledger entry declares `higher_is_better` or `lower_is_better` for its metric.
  *A-assurance#tracked-targets-gate*
- `trend-baseline` — A trend figure compares with the newest earlier successful default-branch report for the same entry and seed under {{assurance.measure.runner-stamp}}; absent matching history yields no annotation.
  *A-assurance#tracked-targets-gate*
- `trend-band` — A trend figure more than 25 percent worse than its baseline annotates the run report and fails no stage.
- `runner-stamp` — The run block carries the runner's processor model, processor count and memory limit, and a trend figure compares only against a baseline with the same stamp.
- `absolute-threshold` — A threshold is an absolute figure of this system's own measure, and it tightens only through {{assurance.baseline.raise-only}}.
- `threshold-cost` — A gate-tier target that rises under `<=`, falls under `>=` or moves under `==` carries a `# cost:` comment above its entry that its entry at `<rev>` lacks, or `contextful-ci measure --since <rev>` raises `MeasureThresholdLoosened`.
  *because a loosened gate otherwise reads as a changed number, and the cost it accepts goes unrecorded*
- `history` — A run on the default branch attaches its run report to the measured commit under `refs/notes/measures`, and no verdict reads a note.
  *A-assurance#tracked-targets-gate*
- `run-report` — `contextful-ci measure-report` writes the note line — commit, run id, attempt, exit code, records and trend annotations — with an integer run id, and reads back every earlier note whatever its run id's JSON type.
  *because earlier notes on `refs/notes/measures` carry text run ids, and an unreadable note hides every older baseline*
- `measured-basis` — A bound with a measured basis names a trend-tier or scheduled-tier ledger entry id as its benchmark.

#### Scenarios

- `assurance.measure.unresolved-entry`: WHEN an entry names `run.journal.entry-keys`, THEN the run raises `MeasureEntryUnresolved` and no test runs.
- `assurance.measure.trend-band`: WHEN a trend p95 moves from 40 ms to 52 ms on a matching runner stamp, THEN the report carries a +30 percent annotation and the stage passes.
- `assurance.measure.trend-baseline`: WHEN a newer failed report follows two successful matches, THEN the trend compares with the newer successful match and the stage passes.
- `assurance.measure.run-report`: WHEN the baseline note's run id is the text `contextful-measures_2026-10-14` and the run's is `42`, THEN the report writes `42` and its annotation names that text run id as its baseline.
- `assurance.measure.open-entry`: WHEN an entry's method is `{ issue = 43 }`, THEN `evals/ledger.md` lists it open and the evaluate stage ignores it.

## release

The release tag's version, the refusals guarding it, the signed annotated tag `contextful-ci tag` creates, and the local packaging the operator runs from it.

- `version` — A release tag is `v0.<minor>.<patch>`: `<minor>` counts the milestones computing `closed` in the tagged commit's `spec/status.md` plus each store format major past `1`, and `<patch>` counts the existing tags `v0.<minor>.*`.
  *A-assurance#store-format-major*
- `store-format` — A commit's store format is the `FormatVersion::CURRENT` constant in its `crates/contextful-core/src/store/lay_out.rs`, and `1.0` where that constant is absent; a constant that does not parse raises `TagStoreFormatUnread`.
  *A-assurance#store-format-major*
- `annotated` — `contextful-ci tag` creates a signed annotated tag on `HEAD` naming the closed milestones once every refusal of this operation clears, and never moves or replaces an existing tag.
  *A-assurance#version-tag-names*
- `dirty-tree` — A working tree or index differing from `HEAD` raises `TagTreeDirty`.
  *because the gate then judges files the tagged commit does not hold*
- `off-branch` — A `HEAD` unreachable from the default branch, `origin/HEAD` unless `--branch` names another, raises `TagOffDefaultBranch`.
  *A-assurance#version-tag-names*
- `version-regressed` — A computed version below the highest existing `v<major>.<minor>.<patch>` tag raises `TagVersionRegressed`, naming both.
  *A-assurance#store-format-major*
- `format-break-patch` — A computed `<minor>` equal to the highest existing tag's, while `HEAD`'s store format major differs from that tag's, raises `TagFormatBreakPatch`, naming both formats.
  *A-assurance#store-format-major*
- `format-notes` — A tag whose store format major exceeds the highest existing tag's carries the message line `Store format <old> → <new>: upgrade every reader, then every writer together.`
  *because an older reader refuses a manifest of the newer major, and an older writer refuses the table once a newer writer commits*
- `workspace-version` — A `[workspace.package]` version in `HEAD`'s `Cargo.toml` that is absent or differs from the computed version raises `TagWorkspaceVersionMismatch`, naming both.
  *A-assurance#version-tag-names*
- `gate-failed` — Once the other refusals clear, every gate stage runs against `HEAD` with `HEAD~1` as base; a failing stage raises `TagGateFailed`, naming the stage's refusal, and no tag is created.
  *A-assurance#version-tag-names*
- `release-tag-version` — `contextful-ci release` over a `HEAD` carrying a `v*` tag, none of which equals `v` plus the `[workspace.package]` version, raises `ReleaseTagVersionMismatch`, naming both, before any build.
  *because a tag cut without `contextful-ci tag` otherwise ships binaries reporting another version*

#### Scenarios

- `assurance.release.version`: WHEN milestones 0 to 7 and 11 compute `closed` and `v0.9.0` exists, THEN the computed version is `v0.9.1`.
- `assurance.release.version-regressed`: WHEN milestone 11 reopens after `v0.9.0`, THEN the computed `v0.8.0` raises `TagVersionRegressed`.
- `assurance.release.store-format`: WHEN 16 milestones compute `closed`, `v0.16.1` declares store format `1.0` and `HEAD` declares `2.0`, THEN the computed version is `v0.17.0` and its message carries `Store format 1.0 → 2.0`.
- `assurance.release.format-break-patch`: WHEN milestone 11 reopens after `v0.9.0` while the store format moves from `1.0` to `2.0`, THEN the computed `v0.9.1` raises `TagFormatBreakPatch`.
