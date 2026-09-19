---
contract: assurance
owns:
  - model
  - prove
  - audit-axioms
  - recheck
  - scope-claim
  - differential-test
---

# The formal model

Two mechanized models stand beside the engine: a Lean 4 package specifying the policy core,
and a TLA+ specification of the store's lease, compare-and-swap and fence protocol. The gate
checks both on every change, and a differential harness ties the Lean model to the decision
functions the engine runs.

## model

| Clause | Statement | Why |
| --- | --- | --- |
| `assurance.model.package` | The Lean model is a package rooted at `formal/`: `lakefile.toml` declares one `lean_lib` target and no `require` stanza, and `lean-toolchain` names one exact release string. | D44 |
| `assurance.model.declared-dependency` | A `require` stanza reaching any external library raises `FormalPackageDependency`, naming the library. | D44 |
| `assurance.model.toolchain-drift` | A build whose resolved toolchain string differs from the pinned one raises `ProofToolchainDrift`, printing both strings. | D44 |
| `assurance.model.build-cost` | A cold elaboration of the package completes within 60 s and writes at most 512 KiB of artifacts. | because the check runs on every change |
| `assurance.model.build-command` | `lake build` at the package root elaborates every declaration and writes the environment the audit reads. | |
| `assurance.model.module-layout` | Three modules carry the package — the layer algebra, placement with the floor, and the authority mapping — and the library root re-exports all three. | |
| `assurance.model.layer` | A layer is a total function from a row identifier to `Bool`; `composed` folds a list of layers by conjunction, admitting a row when every member admits it. | |
| `assurance.model.floor` | An allow-set is a decidable predicate over a placement value; `floor` intersects a list of allow-sets pointwise, and `floor []` admits every caller as the unit of that fold. | |
| `assurance.model.placement-inductive` | The declared placement is an inductive type carrying its identifier inside the constructor, with one case per category the manifest admits and one case for an absent declaration. | |
| `assurance.model.unmodelled-constructor` | A category the manifest admits with no case in the placement inductive raises `UnmodelledConstructor` at elaboration, naming the category. | D51 |
| `assurance.model.total-definitions` | Every definition is total and computable: none is marked `partial`, `noncomputable` or `opaque`, and decidable equality on each finite enumeration is derived. | |
| `assurance.model.from-the-spec` | Each definition is written from the specification text and derived from no engine source. | because a definition copied from code makes every theorem restate the code instead of checking the specification |
| `assurance.model.out-of-model` | The package defines no credential bytes, signature verification, SQL semantics, journal write, process state or attacker observation, and no theorem reaches one. | |
| `assurance.model.protocol-model` | A TLA+ specification under `formal/protocol/` models lease acquisition, renewal, expiry and release, the fenced compare-and-swap on the catalog row and the cursor object, holder pause, message delay and crash at every step. | because the store's concurrency protocol carries the highest-severity defects, and the Lean package does not reach it |
| `assurance.model.protocol-safety` | The protocol model checks two safety properties: no commit lands carrying a fence below the highest fence granted, and release keeps the lease object and its fence. | |
| `assurance.model.protocol-check` | The gate model-checks the protocol with three nodes and four lease generations; a violating trace raises `ProtocolInvariantViolated`, printing the shortest trace. | P7 |
| `assurance.model.protocol-pins` | {{store.lease.stale-fence}} and {{store.fold.partial-snapshot}} pin to the protocol model's safety properties. | |

## prove

| Clause | Statement | Why |
| --- | --- | --- |
| `assurance.prove.composition-sound` | `composed_sound`: a row the fold over a list admits is admitted by every member of that list. | |
| `assurance.prove.narrowing` | `composed_narrows`: a row admitted by `l :: ls` is admitted by `ls`. | because appending a layer then removes rows and adds none |
| `assurance.prove.order-is-specified` | The composition theorem ranges over the single order {{authority.compose.relation-order}} fixes, and filtering then masking is a different relation from masking then filtering. | |
| `assurance.prove.commutation-claim` | A claim that the enforcement stages commute raises `CommutationClaimed`. | D44 |
| `assurance.prove.placement-is-a-layer` | `zone_layer_sound`: with the placement check as one more list member, a row the whole list admits was admitted by that check, with no bridging hypothesis. | |
| `assurance.prove.floor-no-downgrade` | `floor_no_downgrade`: a floor admitting a caller forces every member of its evidence list to admit that caller. | |
| `assurance.prove.fail-closed` | An evidence list holding one fail-closed member yields a floor admitting no public-cloud caller, and the fail-closed allow-set rejects both cloud categories. | |
| `assurance.prove.symbolic-inclusion` | Zone inclusion is decided by subsumption over patterns, quantified over every constructor and every identifier, the named provider and the undeclared case included. | |
| `assurance.prove.sampled-inclusion` | Inclusion decided by evaluating a fixed set of example values raises `ZoneInclusionSampled`. | D51 |
| `assurance.prove.effective-policy` | An effective policy computed from two allow-sets is included in both. | |
| `assurance.prove.proof-targets` | The authority mapping of {{authority.profile.delegation-profile}} carries two proof targets: inclusion, and narrowing — for a fixed trusted environment, permission under an attenuated child implies permission under its parent. | because both are pure decision functions; the other candidate targets need an execution relation no clause specifies |
| `assurance.prove.target-binding` | A proof target is bound to one authenticated query path, recorded beside it as a module path and a test, before the claim names it. | |
| `assurance.prove.unbound-target` | A target the claim names with no binding raises `ProofTargetUnbound`, naming the target. | D51 |
| `assurance.prove.negative-space` | Beside each theorem, its negative space states what it leaves open: that the required members were in the list, that a member removes rather than tests, and that execution reaches the fold before an effect. | |
| `assurance.prove.no-negative-space` | A theorem published without its negative space raises `TheoremWithoutNegativeSpace`, naming the constant. | D44 |
| `assurance.prove.mediation-from-composition` | A filter-composition theorem offered as a mediation theorem raises `MediationClaimedFromComposition`. | because mediation quantifies over the reachable states of an execution; composition quantifies over a list |
| `assurance.prove.names-carry-reach` | A constant's name states the object it ranges over, not the property a reader hopes for. | |

unsettled: Do profile meaning, restriction preservation and scoped-session execution become proof targets, and which execution relation does the mediation induction range over? owner: formal affects: assurance.prove

unsettled: Is inclusion decided by subsumption over patterns or by a normal form on allow-sets, given a category pattern subsuming every identifier under it? owner: formal affects: assurance.prove

unsettled: What discharges the completeness of an evidence list backing a floor, given that the empty list folds to admit-everything? owner: formal affects: assurance.prove

## audit-axioms

| Clause | Statement | Why |
| --- | --- | --- |
| `assurance.audit-axioms.allowlist` | The axiom allowlist holds 2 entries, `propext` and `Quot.sound`, and every theorem's footprint is a subset of it. | because no required constant draws on `Classical.choice` |
| `assurance.audit-axioms.transitive-audit` | Each required constant's axiom footprint is read transitively off the elaborated environment, naming every axiom reached. | |
| `assurance.audit-axioms.verdict-input` | A verdict is a function of the elaborated environment and the inventory alone; source text, declaration counts and the build's exit status decide nothing. | because elaboration reports a hole as a warning and exits zero, and a character pattern misses a hole spelled in parentheses |
| `assurance.audit-axioms.axiom-outside-allowlist` | An axiom in a footprint and absent from the allowlist raises `AxiomOutsideAllowlist`, printing the constant and the axiom. | D44 |
| `assurance.audit-axioms.hole-axiom` | A footprint containing the hole axiom raises `ProofHoleAxiom`, naming the declaration, whatever its syntax spells. | D44 |
| `assurance.audit-axioms.native-evaluation-axiom` | A footprint containing a per-declaration native-evaluation axiom raises `NativeEvaluationAxiom`, naming the declaration that minted it. | D44 |
| `assurance.audit-axioms.inventory` | `formal/inventory.toml` holds one row per required constant — name, module, expected statement text, admitted axioms, binding, negative space — edited apart from the declarations satisfying it. | |
| `assurance.audit-axioms.missing-constant` | A required constant absent from the elaborated environment raises `TheoremConstantMissing`. | D44 |
| `assurance.audit-axioms.statement-drift` | A required constant whose elaborated statement differs from its expected text raises `TheoremStatementDrift`, printing both. | D44 |
| `assurance.audit-axioms.inventory-change` | A change to a required statement lands in the commit carrying the proof it admits. | |
| `assurance.audit-axioms.report` | The report names the commit, the resolved toolchain, the allowlist and inventory revision applied, and for each required constant its statement match and the axioms it reaches. | |
| `assurance.audit-axioms.check-command` | `contextful formal check` elaborates the package, matches every inventory row, audits every footprint, writes the report, and exits non-zero naming the first failing constant. | |

unsettled: Which review admits a new axiom to the allowlist, and where is it recorded beside the constant that draws on it? owner: formal affects: assurance.audit-axioms

## recheck

| Clause | Statement | Why |
| --- | --- | --- |
| `assurance.recheck.two-phase` | A recheck rebuilds the package from the commit's own source and pinned toolchain into an empty artifact directory, fetching nothing, and re-runs the audit against the rebuilt environment. | |
| `assurance.recheck.credential-free` | A recheck environment exposing a token, a signing key or a registry login raises `RecheckEnvironmentCredentialed`, naming the variable or file carrying it. | D44 |
| `assurance.recheck.report-mismatch` | A recheck whose per-constant statement text or axiom set differs from the first phase's raises `RecheckReportMismatch`, naming the constant. | because `.olean` output is not byte-reproducible across hosts and paths, while the per-constant report is the fact under check |
| `assurance.recheck.wall-time` | A recheck completes within 600 s. | |

## scope-claim

| Clause | Statement | Why |
| --- | --- | --- |
| `assurance.scope-claim.claim-sentence` | The assurance claim reads: these named decisions satisfy these named Lean specifications under these stated translation and runtime assumptions; each list resolves to code paths, constants and components in the tree. | |
| `assurance.scope-claim.beyond-named-decisions` | A claim that the enforcement engine, or any object wider than the named decisions, is verified raises `ClaimBeyondNamedDecisions`. | D44 |
| `assurance.scope-claim.trusted-dependencies` | The claim names the delegation library, its cryptography, its parser and the translator as trusted dependencies, and proves none of them. | |
| `assurance.scope-claim.unnamed-dependency` | A claim resting on a component absent from its trusted-dependency list raises `TrustedDependencyUnnamed`, naming the component. | D44 |
| `assurance.scope-claim.translation-chain` | A statement about translated code inherits four links: the compiler's lowering, the translator, hand-written models of external definitions, and the production build configuration. | |
| `assurance.scope-claim.unstated-chain` | A theorem claimed over translated code without its translation chain raises `TranslationChainUnstated`. | D44 |
| `assurance.scope-claim.refinement-scope` | Refinement covers the decision functions behind the two proof targets, whose inputs are values and whose outputs are decisions. | |
| `assurance.scope-claim.refinement-exceeded` | Translation reaching cryptography, a parsing adapter, a database call or concurrency raises `RefinementScopeExceeded`, naming the module. | D52 |

unsettled: Which translation toolchain reaches Lean from the engine's source, and does it resolve against the pinned release string? owner: formal affects: assurance.scope-claim

## differential-test

| Clause | Statement | Why |
| --- | --- | --- |
| `assurance.differential-test.reference-model` | The reference model is an executable Lean rendering of the decision functions, built to a binary that reads one case on standard input and prints one decision. | |
| `assurance.differential-test.harness` | The harness drives the reference binary and the engine's decision function over each generated case and compares the two decisions field by field. | D52 |
| `assurance.differential-test.case-classes` | Each run generates malformed inputs, boundary values and well-formed requests, and its report names the classes it drew from. | because a green run is evidence over the generated classes, not an equivalence proof |
| `assurance.differential-test.minimized` | A disagreement is shrunk until removing any further field makes the two agree, and the minimized case is recorded. | |
| `assurance.differential-test.corpus-replay` | Every counterexample-corpus case replays before any freshly generated case. | |
| `assurance.differential-test.corpus-entries` | The counterexample corpus retains at most 256 entries, evicting the oldest case a fresh seed reproduces. | |
| `assurance.differential-test.run-budget` | One invocation, corpus replay included, completes within 300 s. | |
| `assurance.differential-test.disagreement` | A case on which the two decisions differ raises `ReferenceModelDrift`, printing the minimized case and both decisions. | D52 |
| `assurance.differential-test.discarded-counterexample` | A run reporting a disagreement and writing no corpus entry raises `CounterexampleDiscarded`. | D52 |
| `assurance.differential-test.seed` | Each run records its generator seed, and re-running with that seed reproduces the same case sequence. | |
| `assurance.differential-test.command` | `contextful formal differential --seed <n> --cases <n>` replays the corpus, then runs the generated cases, exiting non-zero on the first disagreement. | |

unsettled: What generated case separates a native build of a decision function from its WebAssembly build on malformed input? owner: formal affects: assurance.differential-test

## Shapes

The package on disk:

```
formal/
  lakefile.toml            one lean_lib, no require stanza
  lean-toolchain           one exact release string
  Contextful.lean          library root, re-exports the three modules
  Contextful/
    Layer.lean             Layer, composed, composed_sound, composed_narrows
    Placement.lean         placement inductive, allow-sets, floor, zone_layer_sound
    Authority.lean         profile elements, effective authority, narrowing
  inventory.toml           required constants and their expected statements
  reference/               the executable model the differential harness drives
  protocol/                the TLA+ lease, compare-and-swap and fence specification
```

One inventory row:

```toml
[constant.composed_sound]
module    = "Contextful.Layer"
statement = "∀ {ls : List Layer} {r : RowId}, composed ls r = true → ∀ l ∈ ls, l r = true"
axioms    = ["propext", "Quot.sound"]
binding   = "engine/policy/src/relation.rs::compose_layers"
negative  = """
Leaves open: that the required members were present in the list; that a member
performs removal rather than a test; that an execution reaches the fold ahead of
an effect.
"""
```
