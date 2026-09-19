---
contract: run
owns:
  - select
  - bind
  - exec
  - fetch
  - emit
  - parse-cues
  - test-engine
---

# The derive tier

Deferred per-row work over rows that already landed. One unit of media becomes many citable
passages; one landed link becomes a document's head facts and the pictures it advertises.
The tier reads the store, calls an engine the operator defined, and appends its answer to a
table of its own through the ordinary write path.

The derive tier between two tables, and the contracts it reaches through:

```mermaid
flowchart LR
  PT[("landed parent table")] --> SEL["select · scan, eligibility, anti-join"]
  OUT[("derive output table")] -.->|"rows and markers"| SEL
  SEL --> BIND["bind · derive.name block in the local contextful.toml"]
  BIND -->|transcribe| EXEC["exec driver · preprocess steps + engine step"]
  BIND -->|link_preview| FETCH["fetch driver · head scan + picture probe"]
  EXEC --> PC["parse-cues · WebVTT, SubRip"]
  PC --> EMIT["emit · content rows, markers"]
  FETCH --> EMIT
  EMIT --> LAND["land path · 31-pipeline"]
  LAND --> OUT
  EXEC -.->|"credential references"| RES["resolver · connector contract"]
  FETCH -.->|"mediated client"| MED["host mediation · connector contract"]
```

## select

| Clause | Statement | Why |
| --- | --- | --- |
| `run.select.required-key` | An absent or blank `engine`, `source_table`, `media_column` or `parent_id_column` raises `DeriveConfigKeyMissing`, naming the key and the pipeline. | A-run |
| `run.select.no-store-root` | A source built without a store root or without its pipeline id raises `DeriveNoStoreRoot`, naming which is absent. | A-run |
| `run.select.foreign-output-table` | The anti-join's pipeline id comes from the build alone; a config key naming another pipeline's output raises `DeriveForeignOutputTable`. | A-run |
| `run.select.rows-per-run` | `max_rows_per_run` truncates the outstanding list after the anti-join, at 25 rows by default; zero or below resolves to that default. | — |
| `run.select.attempts-per-unit` | `max_attempts` allows 3 attempts per unit by default, and a non-positive spelling means the same figure. | — |
| `run.select.seconds-per-run` | `max_seconds_per_run` bounds the serial unit loop at 300 s by default for `link_preview` and carries no default for `transcribe`. | — |
| `run.select.incomplete-unit` | A parent row whose key or media column is null, empty or whitespace raises `DeriveUnitIncomplete`; the row is skipped and counted on the run record. | P4 |
| `run.select.journaled-pull` | A derive pipeline configured to journal its pulls raises `DeriveJournaledPull`. | A-authority |
| `run.select.unmetered-grant` | `task` alone decides whether a derive pipeline reaches vendors; a `transcribe` pipeline declaring a shared-quota grant raises `DeriveUnmeteredGrant`. | A-connector |
| `run.select.metered-client` | A `link_preview` pipeline opening a socket outside the mediated client raises `DeriveMeteredClient`; every request it makes enters the run's request ledger. | A-connector |

unsettled: At what parent-table size does the in-memory scan stop fitting, and what replaces it? owner: derive affects: run.select

unsettled: Does a dry run print eligible, already-derived and outstanding counts before a scheduled tick pays for them? owner: derive affects: run.select

## bind

| Clause | Statement | Why |
| --- | --- | --- |
| `run.bind.unbound-engine` | An engine with no `[derive.<name>]` block raises `DeriveEngineUnbound`, naming the pipeline, the engine, the file to edit and every bound engine. | A-run |
| `run.bind.command-in-manifest` | `command`, `preprocess`, `env` or `allow_hosts` inside `[pipeline.source.config]` raises `DeriveCommandInManifest`. | A-run |
| `run.bind.unknown-task` | `task` is `transcribe`, the default, or `link_preview`; any other value raises `DeriveUnknownTask`, printing both. | A-run |
| `run.bind.driver-mismatch` | `driver` is `exec` or `"none"` behind `transcribe` and `fetch` behind `link_preview`; any other pairing raises `DeriveDriverMismatch`. | A-run |
| `run.bind.endpoint-host-bare` | An `endpoint_host` carrying a path, query, port or scheme raises `DeriveEndpointHostNotBare`. | A-run |
| `run.bind.remote-url-unsupported` | An `http` or `https` media value for an engine declining remote addresses raises `DeriveRemoteUrlUnsupported`, naming the `when = "media_is_url"` preprocess step. | A-run |
| `run.bind.media-unreadable` | A media value that is neither an address nor a readable local file raises `DeriveMediaUnreadable`, failing that unit alone. | A-run |
| `run.bind.confidence-range` | A `confidence` outside `0.0..=1.0` after adapter normalization raises `DeriveConfidenceOutOfRange` and lands null. | A-run |
| `run.bind.upstream-excerpt` | An `Upstream` excerpt holds at most 4 KiB and no request body. | — |
| `run.bind.engine-unavailable` | `EngineUnavailable` from a transcriber ends the run; from a `LinkReader` it raises `DeriveLinkEngineUnavailable` and costs that one unit. | P6 |
| `run.bind.advisory-zone` | A row zone taken from the binding's advisory `zone` key raises `DeriveAdvisoryZone`. | A-run |

unsettled: Does a build-time check refuse an engine whose declared locality is wider than the source table's admitted zones? owner: derive affects: run.bind

## exec

| Clause | Statement | Why |
| --- | --- | --- |
| `run.exec.shell-command` | `command` is an argument array run with no shell; a `command` given as one string raises `DeriveShellCommand`. | A-run |
| `run.exec.env-name` | An allowlist name that is not ASCII alphanumeric or underscore, or that leads with a digit, raises `DeriveEnvNameInvalid`. | A-run |
| `run.exec.chain-deadline` | `timeout_secs` bounds one unit's whole chain at 1800 s by default. | — |
| `run.exec.captured-output` | `max_output_bytes` bounds each step's captured standard output and error at 8 MiB by default. | — |
| `run.exec.audit-entries` | A chain contributes at most 64 entries of captured output to the run record. | — |
| `run.exec.step-error-excerpt` | A failing step's error text carries at most 4 KiB of its standard error. | — |
| `run.exec.non-zero-exit` | A step exiting non-zero raises `DeriveStepExit` carrying its bounded error text into the run audit. | A-run |
| `run.exec.deadline-elapsed` | A chain outrunning its deadline raises `DeriveStepTimeout`, naming the running step. | A-run |
| `run.exec.output-cap` | Output past the bound is counted and discarded, never buffered; crossing it kills the step and raises `DeriveOutputCap` with the byte count, draining continuing so the child never blocks. | A-run |
| `run.exec.silent-step` | A preprocess step exiting zero without writing its output file raises `DeriveStepProducedNothing`. | A-run |
| `run.exec.missing-binary` | A `command[0]` that is not an executable file on this machine raises `DeriveBinaryMissing`, naming the binary and the step. | A-connector |
| `run.exec.unpinned-path` | A path-form `command[0]` without `sha256` raises `DeriveUnpinnedPath`, quoting the digest just computed; a bare search-path name carries none. | A-connector |
| `run.exec.digest-mismatch` | A pinned file whose bytes differ from its recorded digest raises `DeriveDigestMismatch`. | A-connector |
| `run.exec.engine-id` | An `exec` engine id reads `exec:<name>@<prefix>`, the prefix being 12 chars of lowercase hex over every step's binary digest and arguments. | — |

```mermaid
sequenceDiagram
  participant T as derive tier
  participant P as preprocess step
  participant E as engine step
  T->>T: scratch directory, cleared environment + allowlist
  loop each preprocess step whose when condition holds
    T->>P: argument array, no shell, own process group
    P-->>T: exit 0 and an output file
  end
  T->>E: argument array over the last media
  E-->>T: cues as vtt, srt or contextful-json
  Note over T,E: non-zero exit raises DeriveStepExit · no output file DeriveStepProducedNothing · past 8 MiB DeriveOutputCap
  Note over T,E: past 1800 s DeriveStepTimeout · deadline or run stop signals the group, and the unit settles after the reap
  T->>T: remove the scratch directory
```

unsettled: Does a vendor engine reached over HTTP need a deadline of its own, separate from the chain deadline? owner: derive affects: run.exec

## fetch

| Clause | Statement | Why |
| --- | --- | --- |
| `run.fetch.scheme` | An address whose scheme is neither `http` nor `https` raises `DeriveSchemeUnsupported` before any socket opens. | A-connector |
| `run.fetch.address-literal` | A host written as an address literal raises `DeriveAddressLiteral`. | A-connector |
| `run.fetch.redirect-chain` | A redirect chain runs to at most 5 hops. | — |
| `run.fetch.binding-key` | `env`, `preprocess`, `engine` or `max_output_bytes` on a fetch binding raises `DeriveFetchBindingKey`, naming the key. | because a silently ignored key leaves an operator believing a fetch carries a credential or runs a process |
| `run.fetch.document-prefix` | `max_document_bytes` bounds the scanned document prefix at 1 MiB by default, dropping the remainder. | — |
| `run.fetch.probe-prefix` | `max_probe_bytes` bounds one picture range request at 64 KiB by default. | — |
| `run.fetch.hop-timeout` | `request_timeout_secs` bounds one hop at 20 s by default. | — |
| `run.fetch.retry-after-default` | A `429` without a usable `Retry-After` maps to `RateLimited` carrying 60 s. | — |
| `run.fetch.charset` | A declared character set other than UTF-8 raises `DeriveCharsetUnsupported`, naming the value. | A-connector |
| `run.fetch.not-utf8` | A document declaring no character set and failing UTF-8 validation raises `DeriveBytesNotUtf8`; a character split at the byte bound is tolerated. | A-connector |

```mermaid
flowchart TD
  A["address from the row"] --> S{"http or https?"}
  S -->|no| R1["DeriveSchemeUnsupported"]
  S -->|yes| L{"address literal?"}
  L -->|yes| R2["DeriveAddressLiteral"]
  L -->|no| H{"host in allow_hosts?"}
  H -->|no| R3["SecretUnpermittedRequest"]
  H -->|yes| P{"resolves to a public address?"}
  P -->|no| R4["ConnectorPrivateAddress"]
  P -->|yes| G["GET · 20 s per hop"]
  G -->|"redirect, up to 5 hops"| H
  G --> D["scan the head within a 1 MiB prefix"]
  D --> C{"UTF-8?"}
  C -->|no| R5["DeriveCharsetUnsupported or DeriveBytesNotUtf8"]
  C -->|yes| F["head facts + picture candidates"]
  F --> PR["probe each picture · 64 KiB range"]
  PR --> ROW["link rows · probe_status per candidate"]
```

## emit

| Clause | Statement | Why |
| --- | --- | --- |
| `run.emit.failure-off-table` | Per-unit failure state recorded outside the output table raises `DeriveFailureOffTable`. | A-run |
| `run.emit.unit-status` | `unit_status` is `ok`, `empty` where the engine established nothing to derive, `unavailable` where it returned nothing and stated no reason, or `failed` on a typed error; any other value raises `DeriveUnitStatusUnknown`. | A-run |
| `run.emit.reserved-discriminator` | A derive table declaring a column named `kind` raises `DeriveReservedDiscriminator`. | A-run |
| `run.emit.settled-revived` | A settled unit re-entering the outstanding set raises `DeriveSettledUnitRevived`. | A-run |
| `run.emit.attempts` | `attempts` is the prior count plus one; `unavailable` and `failed` stop at {{run.select.attempts-per-unit}}, and `empty` receives 1 attempts in total. | — |
| `run.emit.unredacted-error` | A non-null `last_error` write that has not passed address redaction raises `DeriveUnredactedError`. | A-authority |
| `run.emit.primary-key` | A derive output table without `primary_key` `["unit_ref", "cue_seq"]` raises `DerivePrimaryKeyMissing`. | because a re-derived unit otherwise lands duplicate passages beside the originals |

unsettled: What validated domain does `_modality` carry, and which value does a passage derived from a video row take? owner: derive affects: run.emit

unsettled: What signal re-derives a unit whose parent row changed after it settled? owner: derive affects: run.emit

unsettled: What reaps derived rows whose parent row is deleted upstream? owner: derive affects: run.emit

## parse-cues

| Clause | Statement | Why |
| --- | --- | --- |
| `run.parse-cues.backward-cue` | A block starting before the last accepted block raises `DeriveCueOutOfOrder` and is dropped. | P4 |
| `run.parse-cues.passage-chars` | A passage stays open while it holds fewer than 600 chars. | — |
| `run.parse-cues.passage-span` | A passage stays open while it spans less than 60 s. | — |
| `run.parse-cues.passage-bytes` | One passage's text holds at most 8 KiB. | — |
| `run.parse-cues.passages-per-document` | One document yields at most 2000 rows of passages. | — |

## test-engine

| Clause | Statement | Why |
| --- | --- | --- |
| `run.test-engine.unsupported-driver` | A `fetch` engine named to the verb raises `DeriveTestEngineUnsupported`. | because the verb drives a transcriber over media, and a link engine takes no media |

## Shapes

The machine half of two bindings, at the top level of `contextful.toml`:

```toml
[derive.local-asr]
driver           = "exec"
timeout_secs     = 1800
max_output_bytes = 8388608
zone             = "local:device"          # advisory; the adapter writes the row value

[[derive.local-asr.preprocess]]
when        = "engine_requires_pcm16_wav"
command     = ["ffmpeg", "-i", "{input}", "-ar", "16000", "-ac", "1", "{output}"]
output_path = "{output_stem}.wav"

[derive.local-asr.engine]
command       = ["speech-cli", "--model", "base", "--srt", "{input}"]
output_format = "srt"
output_path   = "{output_stem}.srt"

[derive.local-asr.env]
SPEECH_API_KEY = "${secret://speech-vendor}"

[derive.card-reader]
driver               = "fetch"
allow_hosts          = ["example.com", "*.example.com"]
allow_image_hosts    = ["*.cdn.example.net"]
request_timeout_secs = 20
```

One tick:

```mermaid
flowchart TD
  A["scan parent table, newest-wins"] --> B["eligibility: select, require_absent"]
  B --> C["anti-join own output, markers included"]
  C --> D["truncate to max_rows_per_run"]
  D --> E{"per unit"}
  E --> F["engine call"]
  F -->|passages| G["content rows, cue_seq 0..N"]
  F -->|typed error or silence| H["marker, cue_seq -1"]
  G --> I["land path"]
  H --> I
  E -->|wall clock elapsed| I
```
