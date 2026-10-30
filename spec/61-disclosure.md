---
contract: disclosure
owns:
  - set-mode
  - release
  - suppress
  - template
  - bound-cohort
---

# Aggregate disclosure

A derived result answers a question over many contributors without handing any
contributor's rows to the party asking. This file fixes the setting a result is derived
in, the ordered release, the suppression signal a consumer sees, the reviewed templates a
releasing principal executes, and the line between a per-person table and a cohort table.

A derived result, from the deployment's setting to the published rows:

```mermaid
flowchart LR
  OP(["operator"]) -->|"chosen setting"| MODE{"deployment setting?"}
  MODE -->|"single operator"| SO["write-time disclosure policy"]
  MODE -->|"clean room"| CR["per-owner partitions"]
  CALLER(["releasing principal"]) -->|"template id, arguments"| TA["template allowlist"]
  TA -->|"reviewed template"| REL["release job"]
  SO -->|"policy"| REL
  CR -->|"partitions"| REL
  UNITS[("contributing units")] -->|"reserved budgets"| REL
  REL -->|"size floor, dominance"| SUPP["suppression pass"]
  SUPP -->|"noised figures"| PUB[("published derived table")]
  PUB -->|"cohort read, floor applied"| CONS(["asking party"])
```

## set-mode

The two deployment settings a derived result is computed in, the offline diagnostic, and the cross-owner hashed join.

- `check-verb` — `contextful disclosure check --config <path>` diagnoses each published model's disclosure policy and statement from local files and reports every refusal by model name.
  *A-disclosure#cross-owner-release*
- `offline-diagnostic` — The single-operator diagnostic reads the manifest and each published model's statement text from local disk and issues 0 requests to the object store.
- `aggregate-shape` — A statement is aggregate-shaped when its parsed SELECT groups rows, has DISTINCT or HAVING, or calls an aggregate catalog function outside a window expression.
  *A-disclosure#cross-owner-release*
- `policy-absent` — A published model matching {{disclosure.set-mode.aggregate-shape}} and carrying neither a disclosure policy nor a recorded opt-out fails the diagnostic with `DisclosurePolicyAbsent`.
  *A-disclosure#cross-owner-release*
- `opt-out-record` — A model records an opt-out as a nonempty `disclosure_opt_out` reason; combining it with `[model.disclosure]` raises {{run.declare.spec-invalid}}.
  *A-disclosure#cross-owner-release*
- `model-unreadable` — A published model whose `sql_file` under {{run.model.statement-source}} does not read raises `DisclosureModelUnreadable`.
  *A-disclosure#cross-owner-release*
- `clean-room-preconditions` — A cross-owner store carries per-owner signed manifest subtrees, per-owner write prefixes enforced by the object store's access policy, and per-owner signing keys; one missing, checked before any cross-prefix write, raises `DisclosureCleanRoomPreconditionUnmet`.
  *A-disclosure#cross-owner-release*
- `hashed-join` — Two owners match on a key each hashes at write time under a per-pair pepper re-randomized per join; matched rows leave as an aggregate under a group-size floor. A static pepper raises `DisclosureStaticPepper`.
  *A-disclosure#cross-owner-release*
- `pepper-version` — A cross-owner join accepts only rows carrying its pair's current pepper version; rotation re-lands both owners' join keys before the next release.
  *A-disclosure#cross-owner-release*

unsettled: Which CLI or MCP verb opens a cross-owner store and runs `hashed-join` and `pepper-version`, given that no production path reaches `open_cross_owner`? owner: disclosure affects: disclosure.set-mode

## release

The ordered statistics release, the write-time disclosure policy, and the per-unit budget reservation.

- `model-policy` — A published {{run.model.model-block}} governed by aggregate disclosure declares `[model.disclosure]`; `metric_bounds` names each noised sum's lower bound, upper bound and quantum.
  *A-disclosure#model-declares-spend*
- `release-verb` — `contextful disclosure release <model> --project <project>` builds the named model from its declared statement, applies its disclosure policy, and publishes one derived table.
  *A-disclosure#model-declares-spend*
- `governed-build` — A build of a model declaring `[model.disclosure]` outside {{disclosure.release.release-verb}} raises `DisclosureReleaseRequired` and publishes nothing.
  *because a plain build publishes the statement's per-contributor rows, contributor key included; only the release step replaces them with a derived table*
- `budget-reservation` — Before computing, a release reserves each contributing unit's declared per-run spend against its lifetime cap in one catalog transaction. A failed reservation raises `DisclosureUnitBudgetExhausted`, and the release publishes nothing.
  *A-disclosure#aggregate-disclosure-enforced*
- `noise-mechanism` — A noised release draws exact two-sided geometric noise scaled to each statistic's sensitivity for distinct-contributor counts and fixed-point sums; the per-run epsilon is divided among released statistics.
  *A-disclosure#exact-integer-noise*
- `metric-bounds` — A noised sum folds each contributor to one value, clips that total to declared finite bounds and rounds it to a positive quantum; absent bounds or quantum raise `DisclosureMetricBoundsAbsent`.
  *A-disclosure#exact-integer-noise*
- `release-envelope` — A release reports its published table, policy hash, reserved per-run spend, suppressed-group presence and row count without carrying a contributor key.
  *A-disclosure#aggregate-disclosure-enforced*
- `lookalike-refusal` — A release requesting an activation handle or lookalike segment, as `--output activation-handle` or `--output lookalike-segment`, raises `DisclosureLookalikeUnsupported` and publishes nothing.
  *A-disclosure#exact-integer-noise*
- `spend-declaration` — `[model.disclosure]` declares `per_run_spend`, the positive epsilon one release reserves from each contributing unit; a release whose policy declares none raises `DisclosureSpendUndeclared` and publishes nothing.
  *A-disclosure#model-declares-spend*
- `lifetime-cap` — A project manifest's top-level `[disclosure]` block declares `unit_lifetime_cap`, the epsilon each contributing unit spends across every release; a release grants it to each unit it reserves from, and an undeclared cap is zero.
  *A-disclosure#model-declares-spend*
- `budget-ledger` — A project's budget ledger is `.contextful/disclosure/<project>/budget.sqlite`, outside the synced store root, and records each contributing unit under the keyed digest of its key under the project's pepper.
  *A-disclosure#model-declares-spend*
- `duplicate-grain` — A non-numeric, non-grain column rides the grouping, and one not constant within its group raises `DisclosureDuplicateGrain`.
  *A-disclosure#aggregate-disclosure-enforced*
- `group-key` — A group key not a subset of the permitted grouping columns raises `DisclosureGroupKeyNotPermitted`.
  *A-disclosure#aggregate-disclosure-enforced*
- `contributor-key` — A governed statement omitting the contributor key raises `DisclosureContributorKeyAbsent`.
  *A-disclosure#aggregate-disclosure-enforced*
- `forbidden-column` — A contract declaring a forbidden column, or a materialization producing one, raises `DisclosureForbiddenColumnPublished`. The check runs after the contributor key leaves the published columns.
  *A-disclosure#aggregate-disclosure-enforced*


## suppress

The group-size floor, the contributor-share ceiling, and the single signal a withheld group leaves.

- `min-group-size` — `min_group_size` counts distinct contributors and is at least 2 subjects. A group whose noised distinct-contributor count falls under it is suppressed, and a smaller declared value raises `DisclosureMinGroupSizeBelowFloor`.
  *A-disclosure#aggregate-disclosure-enforced*
- `contributor-share` — `max_contributor_share` lies above 0 percent and at most 100 percent of a group's sign-insensitive metric mass; a value outside raises `DisclosureShareOutOfRange`.
  *A-disclosure#aggregate-disclosure-enforced*
- `dominance-unverifiable` — A group under a share constraint whose per-contributor masses are unavailable raises `DisclosureDominanceUnverifiable` and is suppressed.
  *A-disclosure#aggregate-disclosure-enforced*
- `empty-policy` — A policy setting neither threshold, or only a share ceiling at a whole group's mass, raises `DisclosurePolicySuppressesNothing`.
  *A-disclosure#aggregate-disclosure-enforced*
- `grouping-allowlist` — An empty permitted-grouping list, or a permitted name that is not column-shaped, raises `DisclosureGroupingAllowlistEmpty`.
  *A-disclosure#aggregate-disclosure-enforced*
- `sentinel-row` — A release with `emit_sentinel = true` represents every suppressed group together as one `__suppressed__` row carrying no group key or exact figure.
  *A-disclosure#aggregate-disclosure-enforced*

## template

Query templates a releasing principal calls by name: the single-statement rule, the checks they pass, and the truncation flag a capped read carries.

- `single-statement` — A declaration holding more than one statement raises `DisclosureTemplateMultiStatement`.
  *A-authority#query-templates*
- `overfetch` — A capped result sets the envelope's truncation flag by reading exactly 1 rows past the effective ceiling.
- `read-envelope` — A template read returns rows, truncation, row ceiling, suppression presence and the disclosure policy hash; set-valued policy fields follow {{run.publish.disclosure-digest}} at every nesting depth.
  *A-disclosure#grants-rows-under*

unsettled: Does a template read take `read-envelope`'s suppression presence and policy hash from the release envelope its table was published under, and which surface serves that read? owner: disclosure affects: disclosure.template

## bound-cohort

The line between a per-person table and a cohort table, and the floor a cohort read carries.

- `governance-declaration` — `[model.disclosure]` declares `governance`, `per-person` or `cohort`, naming whether each published figure describes one person or a cohort; an absent `governance` is `per-person`, and another value raises {{run.declare.spec-invalid}}.
  *because an undeclared model takes the stricter regime: a per-person model refuses exact figures, so omitting `governance` never widens what a release publishes*
- `cohort-widening` — A template or statement read grouping a `cohort` model's published table by a strict subset of the group key its release records raises `DisclosureCohortWidening`; grouping by the whole key or a superset of it reads.
  *because a coarser grouping sums released groups into figures no release noised or floored, and the recorded key decides each read without a ledger of earlier releases*
- `widening-groupings` — A SELECT groups each cohort table its `FROM` reaches by each `GROUP BY` set, each aggregate window's partition list, and the empty set when it aggregates under neither; only a bare column reference covers a key column.
  *because an expression over a key column can merge its values, so only the column itself keeps released groups apart*
- `per-individual-row` — A query, template or retrieve read touching a table a `cohort` model's statement names raises `DisclosurePerIndividualUngranted` unless a grant holds `read-individual` over that table.
  *because the face applies no floor to an ad-hoc read, so a read of a source table returns contributors' rows that no release has suppressed*
- `singleton-cohort` — A `cohort` model whose permitted grouping columns or non-contributor grain columns name its contributor key or a forbidden column raises `DisclosureSingletonCohort` at declaration, before any row lands under it.
  *A-disclosure#aggregate-disclosure-enforced*
- `exact-figures` — A policy declaring `figures = "exact"` on a `per-person` table raises `DisclosureExactPerPerson`. `figures` defaults to `"noised"`, and exact figures publish only from a `cohort` table.
  *A-disclosure#aggregate-disclosure-enforced*

## Shapes

A disclosure policy on a governed model:

```toml
[model.disclosure]
grouping_allowlist    = ["category", "region", "period"]
contributor_key       = "tenant_id"
min_group_size        = 5
max_contributor_share = 0.4
emit_sentinel         = true
figures               = "noised"   # the default; "exact" only on a cohort table
governance            = "per-person"   # the default; "cohort" for figures describing a cohort
forbidden_columns     = ["tenant_id", "subject_id", "account_email"]
per_run_spend         = 0.5        # epsilon reserved from each contributing unit per release

[model.disclosure.metric_bounds]
value = { lower = 0, upper = 1000000, quantum = 1 }

[disclosure]
unit_lifetime_cap = 10             # epsilon each contributing unit spends across every release
```

The breakdown a governed statement emits, and the rows published from it:

```
-- emitted: one row per (group, contributor)
category  region  period  tenant_id  value
group-a   emea    2       t-0191     142000
group-a   emea    2       t-0044     128000
group-a   emea    2       t-0332     111000
group-a   emea    2       t-0072     102000
group-a   emea    2       t-0088      88000
group-b   emea    2       t-0044      55000

-- published after noisy partition selection, suppression and roll-up;
-- the figure is noised, not the exact sum 571000
category  region  period  value
group-a   emea    2       568940
__suppressed__
```

The envelope a capped template read returns:

```json
{
  "rows": [{ "period": 2, "value": 568940 }],
  "truncated": true,
  "row_ceiling": 500,
  "suppression_present": true,
  "disclosure_policy_hash": "sha256:4f1c…"
}
```
