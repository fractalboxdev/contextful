# 0263 — Aggregate disclosure is enforced when a derived table is built, not when a query runs

**Status:** accepted 2026-09-18
**Decides:** `disclosure.release.refusal.grain-column`, `disclosure.release.refusal.group-key`, `disclosure.release.refusal.contributor-key`, `disclosure.release.refusal.forbidden-column`, `disclosure.suppress.refusal.empty-policy`, `disclosure.suppress.refusal.zero-floor`, `disclosure.suppress.refusal.share-range`, `disclosure.suppress.refusal.grouping-allowlist`

## Context

A published aggregate table is not read through one door. It is read over the statement
surface, reached through retrieval, pulled by external consumers, and joined into further
models. Any control placed at one of those doors has to be reimplemented at the others, and
a door added later starts unguarded.

The materialization is the one point every one of those paths passes through. A cell that
was never staged cannot be read by any of them, so enforcing at build converts a policy
question into a property of the bytes on disk.

Shape decides it a second time. Suppression rules operate on per-contributor mass: how many
distinct contributors a group has, and whether any one of them dominates the metric. Neither
figure is recoverable from an already-rolled-up result, so a query-time evaluator looking at
a published aggregate is looking at exactly the output that destroyed its inputs. The
governed statement therefore emits one row per group and contributor, and the engine rolls
that breakdown up after suppression.

Once the policy is a build-time artifact, its own malformations become build-time questions.
A policy setting neither threshold suppresses nothing and is indistinguishable from no
policy; a zero group-size floor is the same thing spelled as a number; a share at or below
zero or above one is not a fraction of anything; an empty list of permitted grouping columns
permits no grouping at all. Each of those configurations looks like protection in a manifest
and provides none.

## Decision

A model declares the policy its materialization is safe by construction under, and the build
enforces it: a breaching cell is never staged, the contributor breakdown rolls up after
suppression, and the run records a hash over the declared policy fields. A group key outside
the permitted grouping columns raises `DisclosureGroupKeyNotPermitted`; a statement omitting
the contributor key raises `DisclosureContributorKeyAbsent`; a non-grain column that is not
constant within its group raises `DisclosureDuplicateGrain`; a declared or produced forbidden
column raises `DisclosureForbiddenColumnPublished`. A policy setting neither threshold raises
`DisclosurePolicySuppressesNothing`, a `min_group_size` of zero raises
`DisclosureMinGroupSizeZero`, a share at or below zero or above one raises
`DisclosureShareOutOfRange`, and an empty or non-column-shaped grouping allowlist raises
`DisclosureGroupingAllowlistEmpty`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Enforce at materialization; refuse a malformed policy** *(chosen)* | One enforcement point every read path passes through, and the guarantee is a property of the staged bytes. | The governed statement emits the per-contributor breakdown and the engine rolls it up after suppression, so a model author writes a shape wider than the published one; and a policy edit is a rebuild rather than a configuration reload. |
| A query-time aggregate evaluator | Policy changes take effect immediately, with no rebuild. | Loses on survival across read paths — it would be reimplemented at the statement surface, at retrieval and for external consumers — and on shape: per-contributor mass is not recoverable from a rolled-up result. |
| Trust the model's own statement to suppress | No policy language, no engine machinery; authors express intent directly in the statement. | Loses on reviewability: the thresholds that governed a build would be unrecoverable afterwards, so nothing can be verified after the fact or attested downstream. |
| Warn on a malformed policy and build anyway | No deployment is blocked by a configuration mistake. | Loses on indistinguishability: a policy that suppresses nothing looks identical to no policy at all, both in the manifest and in the output, so the warning is the only difference and it is not in the table. |
| Enforce at build and again at query time | Defense in depth against a table built under an older policy. | Loses on the shape criterion at the query-time half, which cannot recompute what it needs, and doubles the surface a rule has to be correct on. |

## Criteria

1. **Whether the control survives every read path.** *This criterion decides.* A published
   table has several consumers and gains more over time; a control that has to be present at
   each of them is a control that is eventually absent at one, and the absence is silent.
2. **Whether the rule's inputs exist where it runs.** Per-contributor mass exists before the
   rollup and nowhere after it.
3. **Whether the governing thresholds are recoverable after the fact.** The policy hash lands
   on the freshness record and on the append-only build log, so an inspection long afterwards
   reads which thresholds governed a given build.
4. **Latency of a policy change.** Given up — a rebuild.

## Consequences

Attaching a policy moves no published column: an identical statement and contract yield an
identical schema fingerprint whether governed or not, so a consumer cannot tell from the
shape that a table is governed, and the sentinel is the only signal it carries.

The accepted cost is on the author and the operator. The statement has to emit one row per
group and contributor — a wider shape than anything published — and the author writes it
knowing the engine collapses it. Tuning a threshold means rebuilding the table rather than
reloading a setting, so a deployment tightening a floor across many models pays a
materialization pass per model.

The log entry outlives the build. Freshness advances past a materialization and the appended
entry does not, so a downstream attestation names the exact policy behind the cells it rests
on rather than the policy currently in the manifest.

Reversing to query-time evaluation is not a relocation but a reimplementation, and it starts
from an output that no longer carries the inputs the rules need.

## Revisit triggers

- A published aggregate format appears that retains per-contributor mass alongside the rolled
  up measures, which would make query-time evaluation decidable.
- Rebuild cost for a threshold change is observed to block routine policy tuning across a
  deployment's models.
- A read path appears that reaches staged cells ahead of materialization, which would put a
  consumer in front of the enforcement point.
