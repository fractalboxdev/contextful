---
contract: authority
owns:
  - redact
  - compose
  - filter-rows
  - mask
  - refuse
  - bound-redistribution
  - place
  - resist
---

# Enforcement and inference placement

Three layers stand between a stored row and a caller: removal at write time, a
redistribution bound at push, and a compiled relation at query time. Together they form
the reference monitor. This file states what each layer does, the order the compiled
relation applies, column classification and masking, and the grammar of an inference zone.

## redact

| Clause | Statement | Why |
| --- | --- | --- |
| `authority.redact.write-time` | Redaction runs inside the writer before columnar encoding. Each rule drops its value or transforms it into a policy-defined substitute: keyed digest, token, prefix or generalized band. | D17 |
| `authority.redact.rule` | A rule names a table, a column, one operation and that operation's argument. | D17 |
| `authority.redact.pipeline-wide` | A rule set declared on a pipeline binds every table the pipeline lands, child tables included. | D17 |
| `authority.redact.after-normalization` | Redaction runs after relational normalization; a rule naming a parent column reaches values shredded into child tables. | D17 |
| `authority.redact.before-the-record` | A removed value is absent from the run path's durable record of the step; {{run.journal.redacting-source}} governs a source journaled verbatim. | D17 |
| `authority.redact.refetch` | Re-running a pipeline over a table under removal rules reads the source again. | D17 |
| `authority.redact.at-rest-scope` | Stored bytes lack only write-time-removed values. A column protected at query time alone is cleartext in storage and readable with the object-store credential. | D17 |
| `authority.redact.summarize-only` | A `summarize_only` column stores its text whole; the flag lands at write time and travels with the column, binding every surface through {{authority.mask.verbatim-quote}}. | D17 |
| `authority.redact.rules-per-pipeline` | A pipeline carries at most 256 entries in its removal rule set. | because a longer rule set is a policy the operator cannot review |

## compose

| Clause | Statement | Why |
| --- | --- | --- |
| `authority.compose.three-layers` | The reference monitor is three layers: write-time removal, the redistribution bound at push, and row-and-column restriction at query time. A request is authorized at each. | P5 |
| `authority.compose.complete-mediation` | Every read of stored rows traverses the layers. A path reaching stored rows outside a registered relation is a defect, never a documented limitation. | P5 |
| `authority.compose.protection-is-a-rewrite` | Protection is a rewrite in the data plane: predicates compiled into the statement, rewritten projections, a per-credential filter at the sync edge, zone floors. Admission decides entry alone. | P5 |
| `authority.compose.registered-relation` | The compiler emits one relation per granted table, bound to the table's bare name for the session's lifetime. Every retrieval arm reads through it. | P5 |
| `authority.compose.relation-order` | A registered relation applies, in order: mirrored permission semi-join, tenant equality, credential predicate, table policy, project default, table zone, then the column projection carrying masks and zone nulls. | P5 |
| `authority.compose.conjunctive-narrowing` | Steps conjoin: a surviving row survives each step alone, and appending a step removes rows and adds none. | P5 |
| `authority.compose.before-the-cut` | Every step, zone exclusion included, completes before a ranked result is cut to its requested size; visible positions carry no trace of a withheld row. | P5 |
| `authority.compose.vector-arm` | A sidecar vector index returns candidate identifiers, which re-join through the registered relation before contributing to a result. | P5 |
| `authority.compose.replica-again` | A caller holding a filtered replica is constrained again per query, the replica's bytes unchanged. | P5 |
| `authority.compose.session-build` | A session builds its relations at admission and rebuilds them when a revocation or a manifest change invalidates a grant it holds. | P5 |
| `authority.compose.relations-per-session` | A session carries at most 1024 entries in its relation set. | because a larger grant set is a mis-scoped credential |

## filter-rows

| Clause | Statement | Why |
| --- | --- | --- |
| `authority.filter-rows.grammar` | A row predicate is the typed boolean subset of SQL in Shapes: column references, comparisons, conjunction, disjunction, negation, membership lists, pattern matching, null tests and declared-safe scalar functions. | P1 |
| `authority.filter-rows.outside-grammar` | Predicates parse at manifest load; a node outside the grammar raises `EnforcePredicateOutsideGrammar`, naming the node. | P1 |
| `authority.filter-rows.subject-relation` | Subject claims reach a predicate as prepared-statement parameters through a session-scoped subject relation. A claim value carrying SQL syntax occupies a parameter slot and reaches no parser. | P1 |
| `authority.filter-rows.exception` | An exception condition is a boolean expression over subject fields in the same grammar, bound the same way. | P1 |
| `authority.filter-rows.override` | A table-policy predicate overriding for a matching subject condition replaces that one predicate; it reaches neither the tenant equality, the mirrored join nor the credential predicate. | P5 |
| `authority.filter-rows.default-deny` | A table reached with no matching grant yields no rows. | P5 |
| `authority.filter-rows.tenant-equality` | A tenant-scoped grant compiles a bound-parameter byte equality on the tenant column, conjoined by the engine; no consumer statement carries it. | P5 |
| `authority.filter-rows.predicate-size` | A declared predicate holds at most 4 KiB of source text. | because a longer predicate is unreviewable policy |
| `authority.filter-rows.membership-list` | A membership list inside a predicate holds at most 512 entries. | because a longer list belongs in a mirrored table |

## mask

| Clause | Statement | Why |
| --- | --- | --- |
| `authority.mask.column-policy` | A column declares `class` and `strategy`, optionally `combine`, `crowd` and `summarize_only`, under `[pipeline.tables.policy.columns]`. An undeclared column carries no class. | D31 |
| `authority.mask.class-registry` | `class` takes a value from the class registry in Shapes. `phi` marks protected health data; `ssn`, `phone`, `email` and `mrn` are exhaustible, each carrying a registered domain size. | D31 |
| `authority.mask.unknown-class` | A `class` outside the registry raises `EnforceUnknownClass` at manifest check. | D31 |
| `authority.mask.class-reach` | A column's class reaches write-time removal, query-time masking, zone floors and the protections a release applies to a key it joins on. | D31 |
| `authority.mask.strategies` | The strategies are `drop`, `hash` (keyed), `tokenize` (format-preserving), `truncate`, `bucket` and `range`. | D31 |
| `authority.mask.drop` | `drop` nulls the cell, or yields an empty string for a string column. | D31 |
| `authority.mask.one-value` | A column mask is one value whose primary and combine halves are private; it drives both the write-time and query-time layers, and no consumer applies the primary alone. | D31 |
| `authority.mask.digest-alone` | `hash` standing alone over an exhaustible class raises `EnforceDigestAloneOnExhaustibleClass`. | D31 |
| `authority.mask.high-entropy` | A column of random identifiers or opaque tokens carries `hash` with no combine. | D31 |
| `authority.mask.combine-generalizes` | A `bucket` or `range` combine behind `hash` or `tokenize` raises `EnforceCombineWithoutGeneralization`; `truncate` is the one combine that generalizes a digest. | D31 |
| `authority.mask.hash-width` | `hash` emits 32 chars of hexadecimal. | D31 |
| `authority.mask.token-width` | `tokenize` emits 20 chars. | D31 |
| `authority.mask.cuts-nothing` | A `truncate` combine at or past its primary's output width raises `EnforceTruncationCutsNothing`. | D31 |
| `authority.mask.crowd` | A column's `crowd`, the fewest inputs one masked output covers, is at least 1000 values and defaults to that floor. | D31 |
| `authority.mask.truncation-ceiling` | Behind `hash` over an exhaustible class, a `truncate` width above floor(log16(domain ÷ `crowd`)), with domain from the class registry, raises `EnforceTruncationTooWide`. | D31 |
| `authority.mask.pepper` | One environment variable supplies the key for `hash` and `tokenize`, read by both layers as one resolved value. | D31 |
| `authority.mask.development-pepper` | Left unset, the pepper resolves to a constant in the source tree and the process emits one signal per run. | D31 |
| `authority.mask.pseudonymous` | `hash` output is an HMAC-SHA-256 under the pepper and is pseudonymous; no surface labels a masked value anonymous. | D31 |
| `authority.mask.native-digest` | Both layers compute the keyed digest natively; the query layer calls a scalar function holding the pepper in process memory, and a value masked at either layer joins the other. | D31 |
| `authority.mask.key-material` | A compiled relation, query plan or session catalog entry carrying pepper-derived key state raises `EnforceKeyMaterialInRelation`. | D31 |
| `authority.mask.subject-exception` | A mask carries exceptions, each a subject condition in the row-predicate grammar bound through the subject relation. | P1 |
| `authority.mask.in-place` | The masked projection substitutes named columns where they stand; the caller receives the column set and order the unmasked statement produces, and `*` expands to it. | D31 |
| `authority.mask.equality` | An equality filter on a `drop` column matches nothing; on a `hash` column it matches equal digests. | D31 |
| `authority.mask.aggregates` | Aggregation reads the masked column. | D31 |
| `authority.mask.summarize-only` | `summarize_only` is a flag, not a strategy: the value stays legible to grounding and citation. | D31 |
| `authority.mask.verbatim-quote` | One citation releases at most 280 chars of a `summarize_only` value verbatim, on every surface. | D31 |
| `authority.mask.absent-column` | A mask naming a column the table's schema omits raises `EnforceMaskOnAbsentColumn` at manifest load, refusing the whole manifest. | because a skipped declaration serves reads under a policy its author believed covered a column |
| `authority.mask.masks-per-table` | A table carries at most 128 entries in its column mask set. | because a table beyond it needs a drop, not a mask per column |

## refuse

| Clause | Statement | Why |
| --- | --- | --- |
| `authority.refuse.scope-denied` | A tenant-scoped credential reading another tenant's rows on a granted table raises `EnforceScopeDenied`, as wire code `scope_denied`, HTTP `403` or an in-band tool error, naming the table and both scopes. | P2 |
| `authority.refuse.not-empty` | A scope refusal is a typed error, never an empty result. | P2 |
| `authority.refuse.echo` | The requested scope in a refusal echoes the statement's value, never a stored value. | P2 |
| `authority.refuse.ungranted-table` | A relation the caller holds no grant on, whether or not a table of that name exists, raises `EnforceUnknownRelation` and is absent from listings and descriptions. | P2 |
| `authority.refuse.payload` | A refusal payload carries at most 512 B. | P2 |
| `authority.refuse.scope-guard` | The scope guard walks the engine's parse once, deciding literal equalities and membership lists over the tenant column and bound template parameters destined for it. | P2 |
| `authority.refuse.top-level` | The guard reads top-level conjuncts of a statement whose own FROM names a scoped table. | P2 |
| `authority.refuse.guard-walk` | The guard visits at most 4096 entries of one parse tree. | P2 |
| `authority.refuse.undecidable` | A constraint the guard cannot settle composes as an ordinary conjunct; the engine-applied equality isolates. | P5 |
| `authority.refuse.drifted-scope` | A tenant value matching no partition reads empty, like a tenant that wrote nothing; the read path repairs no transformed scope value. | D25 |

## bound-redistribution

| Clause | Statement | Why |
| --- | --- | --- |
| `authority.bound-redistribution.withhold` | A cleared redistribution flag withholds every object of the table at push and omits the table's name from the bucket manifest. | D38 |
| `authority.bound-redistribution.before-diff` | The exclusion applies before the object diff is computed. | D38 |
| `authority.bound-redistribution.outranks-grants` | The bound holds against a credential granted everything. | D38 |
| `authority.bound-redistribution.envelopes` | The bound covers the table's columnar files and each pull's verbatim record. | D38 |
| `authority.bound-redistribution.root-allowlist` | While any table is withheld, a root-level object ships only when the allowlist in Shapes names it. | D38 |
| `authority.bound-redistribution.root-not-allowlisted` | A root-level object absent from the allowlist raises `EnforceRootObjectNotAllowlisted` at push. | D38 |
| `authority.bound-redistribution.left-behind` | Clearing the flag over a table an earlier push uploaded raises `EnforceStaleRedistributedObjects`, naming each object and the remedy, deleting none. | D38 |
| `authority.bound-redistribution.manifest-is-an-index` | An object stays fetchable by its key after the manifest stops naming it; a manifest authorizes nothing. | D38 |
| `authority.bound-redistribution.report` | A left-behind report names at most 100 files and states the total found. | D38 |
| `authority.bound-redistribution.two-checks` | The left-behind check runs before and after the upload loop; the guarantee holds for one bucket under one pushing configuration. | D38 |
| `authority.bound-redistribution.edge-filter` | A serving edge in front of the bucket filters a materialized replica by the pulling credential's grants. | D38 |

## place

| Clause | Statement | Why |
| --- | --- | --- |
| `authority.place.zone-string` | A zone string parses, after trimming, as `local:device`, `on-prem:<id>`, `private-cloud:<id>` or `public-cloud:<id>`; any other text is undeclared. The identifier is part of the value. | D32 |
| `authority.place.allow-set-entry` | An allow-set entry is `*`, `local:device`, `on-prem:*`, `private-cloud:*`, `public-cloud:*`, or a category carrying a concrete identifier. | D32 |
| `authority.place.unparsed-pattern` | An allow-set entry matching no entry form raises `EnforceZonePatternUnparsed`, naming the entry. | because an unparsed entry matching nothing hides a policy error; a new zone category updates the grammar first |
| `authority.place.disjunctive` | A zone is admitted when any entry matches. A bare category matches every identifier in it; an entry carrying an identifier matches that identifier alone. | D32 |
| `authority.place.undeclared` | Only `*` admits an undeclared zone. | D32 |
| `authority.place.caller-zone` | The calling process declares its zone per request; the store carries none. | D32 |
| `authority.place.picks-no-model` | Placement selects no model and dispatches no inference; the engine's one invocation point is an operator-configured endpoint capability for synthesis. | D32 |
| `authority.place.fail-closed` | A table declaring no zone policy resolves to the fail-closed pair `local:device` and `on-prem:*`, declared and discovered tables alike. | D32 |
| `authority.place.permissive-default` | A default resolving an unlabeled table to `*` raises `EnforcePermissiveZoneDefault` where more than one principal owns the store. | D32 |
| `authority.place.protected-floor` | A `phi` column or table carries a floor at the fail-closed pair; a wider declared set resolves down to it at serve time. | D32 |
| `authority.place.floor-widened` | A manifest widening a protected-class surface past its floor without the override flag raises `EnforceProtectedFloorWidened`. | D32 |
| `authority.place.removal-with-public-cloud` | A surface declaring write-time removal on a column and an allow-set admitting a public cloud raises `EnforceRemovalWithPublicCloud`. | P1 |
| `authority.place.narrowest-grain` | A column's set narrows its table's; a principal's set narrows both. | D32 |
| `authority.place.excluded-row` | Where the table's effective set omits the session's zone, the row leaves the result. | D32 |
| `authority.place.excluded-cell` | A cell whose effective set omits the session's zone arrives null, a mask exception notwithstanding. | D32 |
| `authority.place.per-principal` | A principal's allow-set pins every row it authors wherever the row lands. A row with no verified author resolves as an undeclared zone. | D32 |
| `authority.place.evidence-floor` | A synthesized row declaring a set wider than the intersection of its evidence tables' sets raises `EnforceEvidenceFloorExceeded` and resolves to that intersection. | D32 |
| `authority.place.incognito` | Incognito pins the session to the fail-closed pair, the uncredentialed local owner's reads included. | D32 |
| `authority.place.incognito-widening` | A session asserting a zone wider than the incognito pin raises `EnforceIncognitoWidening`. | D32 |
| `authority.place.team-default` | A project declares a default zone, whether users widen it, and whether incognito is on by default. | D32 |
| `authority.place.serve-outcome` | Serving one row yields a drop flag and the list of zone-masked columns. | D32 |
| `authority.place.envelope` | A response envelope carries row counts removed by predicate and by zone, column names masked by policy and by zone, the asserted zone and the incognito flag, and no removed value. | D32 |
| `authority.place.symbolic-inclusion` | Inclusion against a floor is decided over every constructor and identifier, never by probing sample zones. | D32 |
| `authority.place.proxy-boundary` | A proxy replica resolves zones at the proxy boundary. | D32 |
| `authority.place.allow-set-entries` | An allow-set holds at most 32 entries. | D32 |
| `authority.place.identifier-length` | A zone identifier holds at most 128 chars. | D32 |

unsettled: What fixes the completeness of the evidence list a synthesized row's floor intersects over, given that an empty list intersects to everything? owner: authority affects: authority.place

## resist

| Clause | Statement | Why |
| --- | --- | --- |
| `authority.resist.credential-shaped-value` | The write path raises `EnforceCredentialShapedValue` on a value matching a credential shape, before it reaches storage. | D37 |
| `authority.resist.read-only-face` | An organization-wide face serves the read subset of the tool surface; conversational writes are server-authored. | D43 |
| `authority.resist.write-tool` | Registering a write tool on an organization-wide face raises `EnforceWriteOnReadOnlyFace`. | D43 |

## Shapes

The order a registered relation applies, one table, one session:

```mermaid
flowchart TD
  T[(table)] --> M[mirrored permission semi-join]
  M --> TEN[tenant byte equality]
  TEN --> RP[credential predicate via the subject relation]
  RP --> TP[table policy, overrides replace one predicate]
  TP --> DEF[project default]
  DEF --> ZONE[table zone: row drop]
  ZONE --> PROJ[projection: column and principal zone nulls, masks, quote bound]
  PROJ --> V[[registered relation]]
```

The class registry. The width ceiling assumes the default `crowd`:

```text
class   exhaustible   domain   hash truncate ceiling
phi     no            -        -
ssn     yes           1e9      4
phone   yes           1e10     5
email   yes           1e10     5
mrn     yes           1e8      4
```

A column policy and a zone declaration:

```toml
[pipeline.tables.policy.columns]
patient_mrn   = { class = "mrn",   strategy = "hash", combine = "truncate:4" }
contact_email = { class = "email", strategy = "tokenize" }
case_notes    = { class = "phi",   summarize_only = true }
salary_band   = { strategy = "bucket:10000" }
internal_id   = { strategy = "hash" }

[pipeline.tables.policy.zone]
allow = ["local:device", "on-prem:*"]
protected_class_override = false

[pipeline.tables.policy.rows]
predicate = "region = subject.region AND classification <= subject.clearance"

  [[pipeline.tables.policy.rows.exception]]
  when      = "subject.role = 'auditor'"
  predicate = "true"

[pipeline.tables.policy.redistribution]
replicate = false

[[principal]]
id   = "analyst-loop"
zone = { allow = ["local:device"] }

[project.zone]
default = "on-prem:hq"
users_may_widen = false
incognito_default = true
```

The row-predicate grammar:

```ebnf
predicate     = disjunction ;
disjunction   = conjunction , { "OR" , conjunction } ;
conjunction   = negation , { "AND" , negation } ;
negation      = [ "NOT" ] , primary ;
primary       = comparison | membership | pattern | null-test | "(" predicate ")" ;
comparison    = operand , ( "=" | "<>" | "<" | "<=" | ">" | ">=" ) , operand ;
membership    = column , [ "NOT" ] , "IN" , "(" , literal , { "," , literal } , ")" ;
pattern       = column , [ "NOT" ] , "LIKE" , string ;
null-test     = column , "IS" , [ "NOT" ] , "NULL" ;
operand       = column | subject-field | literal | safe-call ;
subject-field = "subject" , "." , identifier ;
safe-call     = declared-function , "(" , [ operand , { "," , operand } ] , ")" ;
```

The root-object allowlist while a table is withheld:

```text
tables/<withheld>/    withheld: no object, no manifest entry
access/               ships: mirrored permission tables
stale_memory.idx      ships
manifest.json         ships, naming no withheld table
catalog.db            withheld
records/              withheld: verbatim pull records
memory.db             withheld
```

The serve-time outcome and the response envelope:

```json
{
  "row": { "drop": false, "masked_columns": ["case_notes"] },
  "envelope": {
    "rows_removed_by_predicate": 41,
    "rows_removed_by_zone": 7,
    "columns_masked_by_policy": ["patient_mrn", "contact_email"],
    "columns_masked_by_zone": ["case_notes"],
    "asserted_zone": "on-prem:ward-3",
    "incognito": true
  }
}
```

The two refusals on the wire:

```json
{ "error": { "code": "scope_denied", "http": 403, "table": "patient_visits", "granted_scope": "ward-3", "requested_scope": "ward-7" } }
{ "error": { "code": "unknown_relation", "http": 404, "relation": "payroll" } }
```
