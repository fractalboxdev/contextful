---
contract: disclosure
owns:
  - record
  - explain
  - attest
  - erase
  - receipt
---

# Audit, erasure and the receipt

`record` is what a read leaves behind. `explain` answers why a person does or does not
reach a resource. `attest` turns the record into evidence a third party checks offline.
`erase` removes a subject or a tenant from the store, and `receipt` is the signed artifact a
tenant purge hands back. Every digest over a low-entropy identifier here is keyed or salted,
and the result is pseudonymous.

The chain, what appends to it, and what reads it back:

```mermaid
flowchart LR
  subgraph READ["read"]
    RD["read face"]
  end
  subgraph CHAIN["audit chain"]
    ENTRY["audit entry"]
    SEG[("chain segment")]
  end
  subgraph STORE["store"]
    SNAP[("snapshot commit")]
  end
  TEL(["telemetry collector"])
  BKT[("replication bucket")]
  VER(["offline verifier"])
  ACC[("exchange policy")]
  EXPLAINER["decision explainer"]
  OPS(["operator"])
  ERASER["erasure cascade"]
  TXN[("erasure transaction record")]
  RD -->|"span, hash-linked"| ENTRY
  RD -->|"span"| TEL
  ENTRY -->|"4096 per signed root"| SEG
  SEG -->|"every 10 min"| BKT
  SEG -->|"chain to verify"| VER
  ACC -->|"grants behind a decision"| EXPLAINER
  SEG -->|"window replay"| EXPLAINER
  EXPLAINER -->|"admit or deny, with coverage"| OPS
  ERASER -->|"signed erasure entry"| ENTRY
  ERASER -->|"survivor replacements"| TXN
  TXN -->|"committed frontier"| SNAP
  ERASER -->|"signed purge receipt"| OPS
```

## record

What a read leaves behind: the span, the hash-linked audit entry, where telemetry lands, and the key custody a log opens under.

- `segment` — Entries append to a numbered segment file in ascending `seq`, and a segment closes under one signed root once it holds its chain header's segment size, 4096 entries by default.
- `chain-header` — `header.json`, written before a new chain's first entry, fixes the chain's format, its digest, `sha256` or `blake3`, and a segment size of at most 65536 entries; the first v1 entry's `prev_hash` is the canonical header's digest.
  *A-disclosure#audit-format-v1*
- `header-unsupported` — Opening or verifying a chain whose header names another format or digest, a segment size outside {{disclosure.record.chain-header}}, or any field beyond {{type:contextful_policy::audit::ChainHeader}} raises `AuditHeaderUnsupported`.
  *because a verifier guessing at a digest it does not implement checks nothing*
- `entry-format` — A v1 {{type:contextful_policy::audit::AuditEntry}} carries `format: 1`, and its `entry_hash` is the chain header's digest over the RFC 8785 canonical JSON of the entry's `format`, `seq`, `prev_hash` and `attributes`.
  *A-disclosure#audit-format-v1*
- `entry-fields` — Verifying a chain holding an entry with any field beyond {{type:contextful_policy::audit::AuditEntry}} raises {{disclosure.attest.broken-chain}} at that entry.
  *because the entry digest covers only `format`, `seq`, `prev_hash` and `attributes`, so without this an injected field verifies clean*
- `inexact-integer` — Appending a v1 entry whose attributes hold an integer beyond ±(2^53 − 1) raises `AuditAttributeInexact` and appends nothing; verifying a chain holding such an entry raises {{disclosure.attest.broken-chain}}.
  *because RFC 8785 writes every number as an IEEE 754 double, so integers past 2^53 share a digest and an edit between two of them verifies*
- `v0-chain` — A chain holding entries without `header.json` is a v0 chain: it verifies and appends under v0 rules, each entry digest over `seq`, `prev_hash` and attributes and each root its segment's last entry digest.
  *A-disclosure#audit-format-v1*
- `group-commit` — Concurrent appends form one append group, which issues 1 sync of its segment file between segment opens; every member returns after that sync, or every member raises {{disclosure.record.unpersisted-entry}}.
  *A-disclosure#append-group-pays*
- `segment-open` — An append group that creates a segment file adds 1 sync of the segments directory.
  *A-disclosure#append-group-pays*
- `tip-signing` — The signed tip writes at segment close, once the log sees no append for 1 s, and at export; an append group closing no segment writes no tip.
  *A-disclosure#append-group-pays*
- `unpersisted-entry` — A read whose entry fails to reach local durable storage raises `AuditEntryUnpersisted` and returns no rows.
  *A-disclosure#hash-chain-attestable*
- `unpersisted-wire` — The read face answers {{disclosure.record.unpersisted-entry}} in-band as wire code `audit_entry_unpersisted`, HTTP `503`, with no `rows` in the result.
  *because a storage fault is the server's and passes, so a client retries it rather than rewording the read*
- `read-entry` — Each read tool call the face answers with a result, over stdio or HTTP, appends one entry, and the result leaves only after that entry's append group syncs; `initialize`, `ping` and `tools/list` append none.
  *A-disclosure#append-group-holds*
- `read-attributes` — A read's entry carries `contextful.tool`, `contextful.credential`, `contextful.subject.<member>` and `contextful.subject.attestation.<member>` per present subject member, `contextful.result.rows`, `contextful.read.at`, `contextful.read.outcome` and `contextful.tables`, the relations the read names.
  *because the chain answers who read how much under which credential, and the attestation keeps an asserted member from reading as identity*
- `memory-read-tables` — A `memory.recall` or `memory.explain` entry's `contextful.tables` lists each relation the read ran a statement on under the caller's session: the entities, the claims, every evidence table its gate read, and a labels table.
  *because the evidence gate reads source rows on the caller's behalf, and a read its entry omits is one no owner can account for*
- `reads-view` — `audit query` runs one statement over `audit_reads`, one row per table a read entry names — `read_at`, `on_behalf_of`, `agent`, `table_name`, `policy`, `outcome`, `row_count`, `claims` — computed from the chain's segments on each call.
  *A-disclosure#audit-reads-view*
- `reads-view-types` — In `audit_reads`, `row_count` is an integer, and `claims` lists the claim ids a read entry records, null where it records none.
  *because a count compared as text sorts wrong, and an empty list beside null gives one absence two spellings*
- `erasures-view` — `audit query` exposes `audit_erasures` from canonical erasure entries: `transaction_id`, `subject_hash`, `affected_counts` and `executed_at`; the projection retains no state and exposes no selector or row payload.
  *A-disclosure#audit-reads-view*
- `bound-entry` — A bounded read's {{disclosure.record.read-attributes}} entry adds `contextful.bounds` as {{store.bound-time.echo}} echoes it, the snapshot id the read resolved, the query text and the response's SHA-256 digest.
  *because a backtest judges the path a case took, and bounds absent from the trajectory prove only the configuration*
- `refused-read` — A read enforcement refuses appends one entry with `outcome` `refused`, naming the relations the guard parsed, and releases no rows.
  *A-disclosure#audit-reads-view*
- `projection` — The projection answers which agent read which table under which policy across a rolling 24 h window, within 1 s.

### custody

One append group holds the log at a time, under unanchored or signed custody.

- `read-chain` — `contextful serve`, `contextful mcp` and {{read.reference.call}} open `.contextful/audit/` before answering; without an explicit startup signing port they use unanchored custody. A chain that does not open stops the process before any row reads.
  *A-disclosure#hash-chain-attestable*
- `read-signer` — An explicit startup read signing port matches independently configured current public pins before opening and before each append; missing, wrong or retired signing authority refuses held-chain reads through {{disclosure.record.unpersisted-entry}} without inferring keys from requests or receipts.
  *A-disclosure#audit-log-opens*
- `single-writer` — One append group holds a directory's audit log at a time: it takes `audit.lock`, links after the chain end, syncs, then releases the lock, so several processes append to one linear chain.
  *A-disclosure#append-group-holds*
- `audit-key` — Explicit erasure resolves one independently random, durable 32-byte project audit key at `.contextful/audit.key`; concurrent creation preserves one key. Malformed, substituted or unreadable key files follow {{disclosure.erase.recovery}}; Unix creation excludes group and other permissions.
  *A-disclosure#append-group-holds*
- `audit-key-loss` — A published erasure frontier with no persisted project audit key follows {{disclosure.erase.recovery}} before key creation; erasure never regenerates a lost pseudonym key.
  *A-disclosure#audit-log-opens*
- `foreign-tail` — An append group finding the last segment file changed since its handle's last write reads that segment's last entry before linking, and issues no sync beyond {{disclosure.record.group-commit}}.
  *A-disclosure#append-group-holds*
- `read-only` — A read-only audit handle verifies the chain without the writer lock; an append through it raises `AuditLogReadOnly`.
  *A-disclosure#append-group-holds*
- `unanchored-over-signed` — An unanchored handle links entries under an unsigned tip and writes no root; opening one over a signed tip, a signed root or the signed `chain.held` a held open writes raises `AuditLogAnchored`.
  *A-disclosure#audit-log-opens*
- `held-under-append` — An unanchored handle's append group or tip write finding `chain.held` written since its open raises {{disclosure.record.unanchored-over-signed}} and writes nothing, so a held open never meets an unsigned tip or an unrooted segment.
  *because the lock is held per append group, so a held open can land between two groups of a running unanchored handle*
- `unsigned-tip` — A held open or signed check over a chain carrying no `chain.held` or signed root, whose tip is unsigned, raises `AuditLogUnanchored`; anchoring through the signing port signs that chain's missing roots and its tip.
  *A-disclosure#audit-log-opens*
- `signed-read` — A read verb's `--issuer-key` opens the chain held without anchoring it; over a chain carrying no `chain.held` it refuses as {{disclosure.record.unsigned-tip}} does, naming {{disclosure.attest.anchor-verb}}, and appends nothing.
  *because anchoring is one-way and {{disclosure.record.unanchored-over-signed}} then refuses every unsigned reader, so only the owner's explicit verb performs it*

### telemetry

Spans leave the process over OTLP only when an endpoint is configured.

- `trace-carrier` — A well-formed W3C `traceparent` names the trace and the parent of the operation's span; a malformed one is ignored and the operation starts a fresh trace.
  *because a caller's broken header must cost it the link to its trace, never the operation*
- `face-carrier` — A read tool call takes {{disclosure.record.trace-carrier}} from its HTTP request's `traceparent` and `tracestate` headers, or from `params._meta.traceparent` and `params._meta.tracestate`; a `_meta` `traceparent` wins over the header.
  *because `_meta` travels with the one message it traces, while a header can belong to a proxy's connection*
- `read-span` — With an exporter configured, each read tool call exports one `contextful.read` span carrying `contextful.tool`, `contextful.tables`, `contextful.result.rows`, `contextful.read.outcome`, a refused or failed read's error identifier as `error.type`, and the statement's SHA-256 as `contextful.query.hash`.
  *because a host tracing its own requests sees the substrate's read as a child of its span, and the digest joins it to the audit entry*
- `read-span-content` — A read span carries no statement text, subject member or row value.
  *because a collector sits outside the audit chain's custody, so a span discloses only digests, counts and identifiers*
- `span-export` — With `OTEL_EXPORTER_OTLP_ENDPOINT` set, spans leave as OTLP/HTTP JSON to its `/v1/traces` path, or to `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` as given, carrying the `OTEL_EXPORTER_OTLP_HEADERS` headers and `OTEL_SERVICE_NAME` as `service.name`.
- `span-unconfigured` — With neither endpoint set, no exporter exists: no span leaves the process and telemetry opens no socket.
  *because egress reachable under a default configuration is telemetry the operator never chose*
- `span-queue` — Spans leave off the operation's path through a queue of 2048 spans; a span past the cap, or lost to a failed export, is dropped and counted.
  *because an unreachable collector must add no latency to the read or run that made the span*
- `stage-span` — With an exporter configured, every run, each seed chunk run included, exports one child span of its {{run.record.run-span}} per pull, landing, commit and journaled step, named `contextful.pull`, `contextful.land`, `contextful.commit` or `contextful.step`.
  *because a slow or failed run shows the stage that held it on the trace its caller already reads*
- `stage-attributes` — A stage span carries `contextful.stage`, the run's `contextful.table`, a pull's or step's `contextful.step` label, `contextful.outcome`, a failed stage's error identifier as `error.type`, and the `contextful.rows` or `contextful.bytes` the stage counts.
  *because the stage, its outcome and its count locate a failure without opening the store*
- `stage-outcome` — A pull, landing or commit records `ok` or `failed` as its outcome; a journaled step records `recorded`, `replayed`, `unrecorded`, `waiting` or `failed`.
  *because a replayed step made no source call, and a trace reading it as a pull misstates the run's egress*
- `stage-content` — A stage span carries no row value and no statement text.
  *because a collector sits outside the store's custody, so a stage span discloses only names, counts and identifiers*
- `stage-unconfigured` — With no exporter configured, a run times no stage and builds no stage span.
  *because span work no collector receives is cost the operator never chose*
- `route-carrier` — Every HTTP route but `/mcp`, `/health` and `/auth/exchange` — receive, receive outcome, run stop, run stream, awake, control, claim write and connection routes — takes {{disclosure.record.trace-carrier}} from its request's `traceparent` and `tracestate` headers.
  *because a run or read a request starts belongs to its caller's trace whichever route carries it*
- `route-span` — With an exporter configured, each such request exports one server span named by its method and route template, carrying `http.request.method`, `http.route`, `http.response.status_code`, and for a 4xx or 5xx answer its error identifier, else its status, as `error.type`.
  *because the route a request took and its answer join the caller's trace as the tool call's read span does*
- `route-parent` — A route's handler receives a `traceparent` naming the route span, so a run the receive route lands takes the request's trace and parents its run span under the route span.
  *because the landing is the request's work, and a run rooted apart from it hides that delivery's cost*
- `queued-parent` — A delivery {{run.land.receive-queued-ask}} queues carries its route span's trace on its message, so each run a consumer lands for it parents its run span under that route span; a group run continues its first member's trace.
  *because a queued landing is the request's deferred work, and a run rooted in the consumer's trace hides that delivery's cost*
- `queued-content` — A queued message's trace is the route span's W3C `traceparent` and `tracestate` alone, never another request header, a path segment's value or the query.
  *because a managed queue's operator reads every message, and a trace needs nothing beyond its two W3C carriers*
- `route-content` — A route span records the route template and never a path segment's value, the query or the body.
  *because a path segment can hold an awakeable token or a batch key, which no collector is granted*
- `job-carrier` — An import or apply under a trace context, from `TRACEPARENT` and `TRACESTATE` or the `/control/apply` route span, records its W3C `traceparent` and `tracestate` alone with each job it adds or changes; an unchanged job keeps its carrier.
  *because a job's submitter is the apply that set its declaration, and the two W3C carriers are all a fire needs to reach that span*
- `job-link` — A fire of an applied version's job exports its execution's run span with one span link to the carrier that version records with the job; a job recorded with none links nothing.
  *because a link joins a fire to its submitter's trace without placing the fire's duration inside it*
- `scheduled-trace` — Serve dispatches each scheduled job fire without `TRACEPARENT` or `TRACESTATE`, so every fire starts a fresh trace that only {{disclosure.record.job-link}} joins to its submitter.
  *because one trace holding every fire of a recurring job grows without bound past a collector's trace window, while a fresh trace per fire keeps each fire whole*

## explain

The access decision and its path, replay over a window with its coverage, and the audience report.

- `decision` — `audit explain --table <t> --subject <principal>` answers `admit` when a grant the exchange policy issues for the `--role` names, else its `default_grants`, carries `read` over a pattern covering the table, and `deny` otherwise.
  *A-disclosure#explanation-reads-exchange*
- `path` — A decision's path names the role or default set, the covering grant, then each step the table declares in {{authority.compose.relation-order}}; a deny's path ends at {{authority.filter-rows.default-deny}}.
  *A-disclosure#explanation-reads-exchange*
- `replay` — `--window <from>..<to>` replays each chain entry naming the subject and the table whose read instant falls in the window: its seq, instant, outcome, agent and recorded `contextful.*` attributes.
  *A-disclosure#explanation-reads-exchange*
- `coverage` — An explanation carries a coverage block: segments and entries verified, entries carrying no read instant, window spans the chain does not reach, and each visibility binding's source with its sweep watermark.
  *A-disclosure#explanation-reads-exchange*
- `audience` — `audit explain --audience <t>` lists by name the roles whose grants read the table, whether `default_grants` do, and per `on_behalf_of` scheme the count of principals and served reads.
  *A-disclosure#explanation-reads-exchange*
- `no-row` — An explanation returning a row, field value or excerpt from the resource it decides about, a replayed `contextful.result.*` attribute beyond `contextful.result.rows` included, raises `VisibilityDiagnosticRow`.
  *P2*
- `empty-window` — A window holding no observations raises `VisibilityNoObservations` and states that no claim is available.
  *P2*
- `unqualified-assurance` — A negative assurance answer emitted without its coverage block raises `VisibilityUnqualifiedAssurance`.
  *P2*
- `groups-not-members` — A reader-facing explanation rendering the member identities of a group on the path raises `VisibilityIndividualNamed`.
  *P2*

## attest

Chain verification, signed segment roots, lineage attestations, and the reach of each guarantee.

- `broken-chain` — A disagreeing digest, entry format or Merkle root, a sequence gap, a failing signature, or, beside `chain.tip`, `chain.held` or a signed root, an absent chain or a missing or unsigned tip raises `AuditChainBroken` at the earliest failing index.
  *A-disclosure#audit-format-v1*
- `merkle-root` — A v1 segment root is the RFC 6962 Merkle tree hash, under the header's digest, over the segment's raw entry digests in `seq` order.
  *A-disclosure#audit-format-v1*
- `root-tag` — A v1 signed root carries its signing algorithm, `Ed25519` or `ES256`, and signs it with the header digest, segment number, entry count and root.
  *A-disclosure#audit-format-v1*
- `inclusion-proof` — `prove` returns one entry, its chain header, its RFC 6962 audit path and its segment's signed root; the proof verifies offline under the signer's public key alone.
  *A-disclosure#audit-format-v1*
- `proof-invalid` — A proof whose entry, audit path, header, root or root signature disagrees raises `AuditProofInvalid`.
  *A-disclosure#audit-format-v1*
- `proof-unavailable` — Proving an entry of a v0 chain, of a segment carrying no signed root, or outside the chain raises `AuditProofUnavailable`.
  *because an open segment has no signed root yet, and a v0 root commits to no tree*
- `root-replication` — `audit anchor` once it signs, and a held chain's replicator at start and every 10 min, copy each signed root the `[sync]` bucket lacks to `<prefix>/<project>/audit/roots/<segment>.json`; a failed copy waits for the next and fails no anchor, append or read.
  *A-disclosure#hash-chain-attestable*
- `replica-verify` — `audit verify --bucket` also holds the chain to each replicated root: a root failing its signature, closing a segment the chain does not reach, or absent or different locally breaks the chain as {{disclosure.attest.broken-chain}} does.
  *because deleting the roots, `chain.held` and the tip's signature leaves a chain no local check tells from an unanchored one*
- `replicate-verb` — `audit replicate` copies the roots {{disclosure.attest.root-replication}} copies, once, and prints the segments it sent.
- `lineage-elision` — A lineage attestation counts a claim's evidence references into tables the caller's session does not register as `withheld` in the `contextful.recall` block, and names no such table.
  *because naming a withheld table leaks its existence outside the caller's authority, while a count keeps the suppression explicable and never confirms a table exists*
- `verify-verb` — `audit verify` opens the chain read-only and prints its end when every check of {{disclosure.attest.broken-chain}} passes, each signature included under `--public-key`.
- `anchor-verb` — `audit anchor --issuer-key <seed>` signs the chain's missing roots and its tip as {{disclosure.record.unsigned-tip}} anchors them, and prints the chain end.
- `prove-verb` — `audit prove --seq <n>` prints the proof {{disclosure.attest.inclusion-proof}} returns, and `audit check-proof` verifies one under a public key, reading no store.
- `owner-only` — An audit verb reading a chain, presented with a capability credential, raises `AuditRequiresOwner` before it reads an audit file.
  *because the chain names every person's reads, and a credential scoped to some tables reaches no record of others*

## erase

The one erasure verb: subject tombstones, the bounded provenance cascade, and the tenant rewrite.

- `privilege` — Erasure sits outside the default grant set, and a caller without the forget grant raises `ErasureUngranted`.
  *A-read#memory-access-follows*
- `unsupported-scope` — An empty target set or erasure admitted only by narrowed grants raises `ErasureScopeUnsupported` before store access; supported erasure admits every requested table explicitly.
  *because ignoring a grant constraint widens destructive authority*
- `subject-column` — A table names its subject column as `subject_id = "<column>"` under `[[pipeline.tables]]`. A subject erasure against a table without one raises `ErasureSubjectUndeclared`, naming the table.
  *A-disclosure#forced-rewrite-measured*
- `key-set` — A key-set request binds its selectors to the declared keys of {{store.declare.erasure-metadata}} and carries an opaque subject hash instead of a raw subject identity.
  *A-disclosure#forced-rewrite-measured*
- `column-key-set` — A column-scoped {{disclosure.erase.key-set}} carries a declared column and non-null scalar keys; canonical declarations select every table naming that column, including tables without subject metadata. Caller table subsets and malformed selectors follow {{disclosure.erase.unsupported-scope}}.
  *because a caller table subset leaves declared copies of the erased key readable*
- `atomic-publication` — Subject and key-set erasure select one committed frontier across every affected table; a failed transaction exposes none of its staged replacements.
  *A-disclosure#re-synthesis-after*
- `reader-frontier` — Query, file, retrieval and cache responses resolve one erasure frontier and revalidate it before releasing bytes; an obsolete frontier releases no rows or files.
  *A-disclosure#re-synthesis-after, A-read#extraction-cache-keys*
- `shared-references` — A shared digest survives while any surviving declared reference names it; erasure collects a digest only after its last surviving reference disappears.
  *A-disclosure#re-synthesis-after*
- `surviving-citations` — A citing table carrying the survival declaration of {{store.declare.erasure-metadata}} retains its rows; its references resolve through {{read.reference.erased-verdict}}.
  *A-disclosure#forced-rewrite-measured*
- `subject-pseudonym` — Subject erasure computes its subject hash as a domain-separated HMAC-SHA-256 under the configured project audit key; no transaction or audit artifact carries raw subject identities or erased key values.
  *A-disclosure#re-synthesis-after*
- `signer-binding` — Erasure signs through the configured issuer or node's {{authority.issue.signing-port}}; readers verify the canonical audit chain under configured public-key pins, independently of transaction artifacts. Missing signing or verifier configuration follows {{disclosure.erase.recovery}}.
  *A-disclosure#audit-log-opens*
- `row-free-audit` — The signed audit chain binds the committed transaction, {{disclosure.erase.subject-pseudonym}} and affected counts to the exact replacement-file inventory.
  *A-disclosure#re-synthesis-after*
- `store-binding` — The signed erasure entry binds {{store.init.store-identity}}; readers reject a frontier whose signed store identity differs from the receiving store, following {{disclosure.erase.recovery}}.
  *A-disclosure#re-synthesis-after*
- `working-publication` — Erasure preserves an immutable signed survivor baseline and selects independent working copies under the ordinary table layout; normal append and fold advance working publication without changing the baseline or requiring another erasure signature.
  *A-disclosure#re-synthesis-after*
- `recovery` — Recovery completes collection for a committed frontier and discards uncommitted replacements; an incomplete or disagreeing transaction record raises `ErasureTransactionIncomplete` before releasing store content.
  *A-disclosure#erasure-forced-rewrite*
- `recovery-verb` — `context erase --recover --project <project>` resumes {{disclosure.erase.recovery}} under {{disclosure.erase.privilege}} and {{authority.verify.effect-boundary}} before collection; selectors and signing ports are absent.
  *A-disclosure#re-synthesis-after*
- `unsigned-recovery` — Uncommitted staging origins resolve against canonical stored tables and require Forget over the complete store scope; signed-only recovery admits the complete authenticated frontier. Uncommitted recovery returns no transaction identity.
  *A-disclosure#erasure-forced-rewrite*
- `retirement-inventory` — Signed retirement inventories bind regular paths through domain-separated HMAC under {{disclosure.record.audit-key}} and their file digests; recovery admits missing files as completed collection, validates all remaining files before deletion, and rejects changes through {{disclosure.erase.recovery}}.
  *A-disclosure#erasure-forced-rewrite*
- `canonical-audit-key` — Erasure and keyed retirement recovery require the persisted canonical {{disclosure.record.audit-key}}; caller-supplied keys equal it, and absent or mismatched keys follow {{disclosure.erase.recovery}} before content access, signed intent, staging or collection.
  *A-disclosure#erasure-forced-rewrite*
- `survivor-names` — Erasure survivor snapshots use row-independent part names, preserve admitted surviving rows, record rewritten part key versions, and rebuild declared indexes through {{store.index.vector-by-fold}} and {{store.index.fulltext-by-fold}}.
  *A-disclosure#forced-rewrite-measured*
- `cascade` — In the same operation the cascade invalidates every derived fact whose provenance reaches the subject key or a tombstoned identifier within 16 hops.
  *A-disclosure#forced-rewrite-measured*
- `cascade-unbounded` — A provenance chain deeper than the cascade bound raises `ErasureCascadeUnbounded`, and the erasure commits nothing.
  *A-disclosure#forced-rewrite-measured*
- `physical-removal` — Within 24 h of the erasure's commit, every file holding an erased row, superseded snapshots inside {{store.fold.retention}} included, is rewritten or collected.
  *A-disclosure#re-synthesis-after*
- `tenant-column-type` — A tenant column of a non-string type raises `PurgeTenantColumnType`.
  *A-disclosure#forced-rewrite-measured*
- `tenant-undeclared` — A purge finding no model declaring an outermost partition key raises `PurgeTenantUndeclared`.
  *A-disclosure#forced-rewrite-measured*
- `owner-only` — A purge presented with a capability token raises `PurgeRequiresOwner`, evaluated before anything reveals store contents.
  *A-disclosure#forced-rewrite-measured*
- `tenant-rewrite` — `context erase --tenant <id>` rewrites every row whose bare outermost partition column holds the identifier's exact bytes out of every table through {{disclosure.erase.atomic-publication}}, and prints a freshly signed receipt.
  *A-disclosure#forced-rewrite-measured*

unsettled: Is request-to-last-replica erasure within 72 h an engine bound or an operator objective, given the engine schedules no replica refresh? owner: disclosure affects: disclosure.erase

unsettled: Does an erasure refuse fact reads, the free-form statement face included, until re-synthesis restores the facts the cascade invalidated? owner: disclosure affects: disclosure.erase

unsettled: Does erasure rewrite each {{read.synthesize.dead-letter}} row holding an erased subject's content, and within which bound? owner: read-path affects: disclosure.erase

## receipt

The signed artifact a tenant purge returns: its claim, its coverage block, its pseudonym and its idempotence.

- `pseudonym` — `tenant_hash` is SHA-256 over a random per-receipt `salt` and the tenant identifier. A receipt carrying the identifier in cleartext raises `ReceiptIdentifierLeak` before signing.
  *A-disclosure#forced-rewrite-measured*
- `widened-claim` — A coverage block naming a scope broader than its `receipt_version` declares raises `ReceiptClaimWidened`.
  *A-disclosure#forced-rewrite-measured*
- `replayed` — Returning the byte-identical earlier artifact in place of a freshly signed one raises `ReceiptReplayed`.
  *A-disclosure#forced-rewrite-measured*
- `coverage` — A `receipt_version` 1 coverage block covers the canonical store and names replicas and the replication bucket as excluded.
  *because a purge deletes no bucket object, so a claim over the replication bucket names a removal that never ran*
- `chain-record` — A purge appends its receipt's `request_id` and SHA-256 digest to the audit chain after signing; a digest the chain already holds follows {{disclosure.receipt.replayed}}.
  *A-disclosure#forced-rewrite-measured*
- `prior-request` — A purge's receipt entry records the tenant's HMAC-SHA-256 under the project audit key beside its `request_id`; a later purge of that tenant signs the latest such `request_id` as `prior_request_id`.
  *because the audit chain already orders every receipt, and a keyed digest links repeat purges without a cleartext identifier or a second record*
- `zero-rows` — A purge whose tenant holds no retained row rewrites nothing and signs a receipt with a zero count for every declared table and an empty `transaction_id`.
  *because an owner proves a tenant's absence with one artifact whether or not rows existed*

## Shapes

The audit segment and the erasure transaction records:

```
.contextful/
  audit/
    header.json             {format, digest, segment_entries}; absent on a v0 chain
    segments/
      000001.jsonl          one entry per line, seq ascending
      000001.root.json      the signed Merkle root closing the segment
    chain.tip               last accepted {seq, entry_hash}
    chain.held              signed {seq, entry_hash} where the issuer first held the log
    audit.lock              held by the one append group writing
  audit.key                 the 32-byte project audit key
  context/<project>/
    _erasure_frontier.json  {transaction_id, audit_hash, audit_seq, tables}
    forget/
      <subject_hash>.stale  {subject_hash, executed_at}
    _erasure/
      staging/<id>/         uncommitted replacements, discarded on recovery
      committed/<id>/       signed survivor baselines and certificates
      working/<id>/         working copies that append and fold advance
```

A chain header:

```json
{ "format": 1, "digest": "sha256", "segment_entries": 4096 }
```

One v1 entry, its `prev_hash` the header's digest:

```json
{
  "format": 1,
  "seq": 1,
  "prev_hash": "sha256:a36fc99c...",
  "attributes": {
    "contextful.query.hash": "hmac-sha256:9f2c...",
    "contextful.subject.on_behalf_of": "user://ada",
    "contextful.subject.attestation.on_behalf_of": "verified",
    "contextful.tables": ["orders", "customers"],
    "contextful.row.phi_columns_returned": 0,
    "contextful.result.rows": 42
  },
  "entry_hash": "sha256:1b7e..."
}
```

A v1 root, and an inclusion proof of one entry:

```json
{
  "format": 1,
  "alg": "Ed25519",
  "header": "sha256:a36fc99c...",
  "segment": 1,
  "root": "sha256:a4f0...",
  "count": 4096,
  "signature": "9e41..."
}
```

```json
{ "header": { "format": 1, "digest": "sha256", "segment_entries": 4096 },
  "entry": { "format": 1, "seq": 1, "...": "..." },
  "path": ["3f9c...", "..."],
  "root": { "format": 1, "segment": 1, "...": "..." } }
```

A purge receipt:

```json
{
  "receipt_version": 1,
  "coverage": {
    "claim": "rewrite-and-exclude",
    "includes": ["run-files", "snapshots", "model-builds", "memory-mirror"],
    "excludes": ["replication-bucket", "replicas", "physical-destruction"]
  },
  "salt": "b64:Qm9n...",
  "tenant_hash": "sha256:4d1a...",
  "request_id": "prg_01HQ8F3K2M",
  "completed_at": "<rfc3339-utc>",
  "tables": [{ "name": "orders", "rows_removed": 118422, "files_rewritten": 37, "builds_rewritten": ["bld_7a21"] }],
  "cascade": [{ "name": "memory_facts", "rows_removed": 903 }],
  "prior_request_id": null,
  "signature": { "payload_hash": "sha256:c0ff...", "signature": "3045a1...", "public_key": "ed25519:9a7d..." }
}
```
