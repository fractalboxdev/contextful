---
contract: surface
owns:
  - open-console
  - register-store
  - visualize
  - package
  - speak
  - ground
  - plan-turn
  - set-vantage
  - browse
  - learn
  - render
  - brief
  - publish-answer
---

# The operator console

The operator console turns governed store reads into answers and store-published state
into administrative views. Its hosted server and embeddable client library expose those
paths to operators and third-party pages.

The Query page between an operator and a store, and where it meets the read contract and the model endpoint:

```mermaid
flowchart LR
  OP(["operator"]) -->|"request"| PER(["identity perimeter"])
  PER -->|"authenticated session"| PAGE
  OP -->|"register stores"| CRED
  subgraph browser["browser"]
    PAGE["Query page"]
  end
  subgraph server["console server"]
    CRED["credential resolver"]
    TURN["turn loop"]
    RED["redactor"]
  end
  subgraph read["read contract"]
    STORE["store engine"]
    MEM[("memory tables")]
  end
  subgraph thirdparty["third-party page"]
    LIB["client library"]
  end
  PAGE -->|"question"| TURN
  CRED -->|"per-reader credential"| STORE
  TURN -->|"admitted read tools"| STORE
  STORE -->|"governed rows"| TURN
  TURN -->|"grounding"| MODEL(["model endpoint"])
  MODEL -->|"draft prose"| RED
  RED -->|"redacted answer"| PAGE
  TURN -->|"distilled conclusions"| MEM
  LIB -->|"search and ask"| STORE
  PAGE -->|"deliberate share"| AUD(["audience"])
```

## open-console

The hosted console's two pages, their API routes, identity gates, page grants and engine capabilities.

- `page-routes` — The console serves Query at `/query` with `/query/api/*` and Admin at `/admin` with `/admin/api/*`.
  *A-surface#console-page-ships*
- `identity-gate` — Cloudflare Access gates hosted pages by default; a deployment without Access uses Cognito managed password login and first-party sessions.
  *A-surface#query-admin-use*
- `page-grants` — The server maps each verified Access application audience or Cognito group to Query and Admin grants and verifies the matching grant on every page and API request.
  *A-surface#query-admin-use*
- `query-surface` — Query presents one composer, one transcript and widgets for returned rows.
  *A-surface#console-page-ships*
- `saved-transcript` — Query restores a saved transcript only under the verified operator and reading session that wrote it, and only for a store the stores route still lists.
  *because a shared browser tab otherwise hands one operator's answer rows to the next*
- `narrow-sidebar` — On a viewport too narrow for the sidebar, a header control opens it as a drawer, keeping chat creation, switching and deletion reachable.
  *because a hidden sidebar with no replacement strands every chat control on a phone*
- `engine-faces` — Query reaches the governed read face; Admin reaches workflow state and sends edit or apply through a server-held control capability.
  *A-surface#query-admin-use, A-surface#console-lives-beside*
- `ungated-route` — An anonymous hosted console route serving page or API content instead of the declared identity gate raises `ConsoleRouteUngated` at deploy probe.
  *A-surface#query-admin-use*
- `wrong-page` — A page or API request without a verified Access assertion or Cognito session granting that page raises `ConsolePageForbidden` and dispatches nothing.
  *A-surface#query-admin-use*
- `admin-grant` — An Admin edit or apply without a server-held admin capability raises `ConsoleAdminGrantMissing` and reaches no control call.
  *A-surface#query-admin-use*
- `source-list` — Admin lists ingest sources from the applied control document, naming each connector, destination, schedule or trigger, applied version and latest run outcome.
  *because the applied document owns source identity and configuration*
- `source-draft` — Admin adds a source through the version-bound edit and apply path of {{surface.open-console.engine-faces}}, with a source form and a raw TOML fallback.
  *A-surface#console-lives-beside*
- `source-preservation` — Adding a source through {{surface.open-console.source-draft}} preserves the applied jobs, models and external steps.
- `source-validation` — Saving and applying a source validates connector availability, required configuration, destination compatibility and duplicate identity under {{surface.apply.validation}}; refusals name the affected field and a correction, and leave the applied version unchanged.
- `source-first-run` — Admin shows the applied version's first and latest source fires, aggregating rows and failures across destinations; absent matching run evidence, the source remains pending after apply.
  *because claiming configuration establishes no execution outcome*
- `source-run-evidence` — A source fire binds its loaded applied version, source identity, configuration digest and destination run identities in the operational record; first and latest summaries travel through {{store.push.run-state}}.
  *because a pipeline name or start time alone can associate an older configuration's run*

#### Scenarios

- `surface.open-console.ungated-route`: WHEN an anonymous deploy probe receives `/query` or `/admin/api/workflows` content, THEN it raises `ConsoleRouteUngated`.
- `surface.open-console.wrong-page`: WHEN a Query-only operator requests `/admin/api/workflows`, THEN it raises `ConsolePageForbidden` without dispatching.
- `surface.open-console.admin-grant`: WHEN an Admin operator applies a document without the server-held admin capability, THEN it raises `ConsoleAdminGrantMissing` without a control call.
- `surface.open-console.page-grants`: WHEN a verified operator holds only a Query grant, THEN Query admits the session and Admin refuses its page and API requests.
- `surface.open-console.query-surface`: WHEN an operator asks on Query, THEN the question and grounded answer occupy one transcript beside its widgets.
- `surface.open-console.saved-transcript`: WHEN a second operator opens Query in the tab where another saved a transcript, THEN Query restores none of its answers or rows.
- `surface.open-console.narrow-sidebar`: WHEN Query renders on a phone-width viewport, THEN a header control opens the chat list with its create, switch and delete controls.
- `surface.open-console.source-validation`: WHEN an HTTP source draft lacks its endpoint, THEN Admin identifies `source.config.endpoint` and the applied document retains its version.
- `surface.open-console.source-first-run`: WHEN an identical source configuration claims another version, THEN older-version runs leave its first-run state pending.

```mermaid
flowchart LR
  OP(["operator"])
  subgraph access["identity gate"]
    QG["Query policy"]
    AG["Admin policy"]
  end
  subgraph browser["browser"]
    QP["Query page"]
    AP["Admin page"]
  end
  subgraph server["console server"]
    QA["Query API"]
    AA["Admin API"]
  end
  subgraph engine["engine"]
    READ["read face"]
    CTRL["control face"]
  end
  OP -->|"query session"| QG
  OP -->|"admin session"| AG
  QG -->|"Query grant"| QP
  AG -->|"Admin grant"| AP
  QP -->|"question"| QA
  AP -->|"workflow request"| AA
  QA -->|"governed read"| READ
  AA -->|"admin capability"| CTRL
  READ -->|"rows"| QA
  CTRL -->|"workflow state"| AA
```

## register-store

Which stores a deployment serves, how each store's credential and binding names derive, and how a request reaches a store's origin.

- `registry-unreadable` — A `CONTEXTFUL_STORES_JSON` value that does not parse raises `StoreRegistryUnreadable` at startup, and no built-in store stands in for the configured set.
  *P3*
- `malformed-entry` — One entry that does not decode raises `StoreEntryMalformed` naming it, and is dropped while its siblings are served.
  *A-surface#configuration-reaches-engine*
- `reserved-id` — A configured entry claiming either built-in id raises `StoreIdReserved`.
  *because the entry otherwise reads the fixture's rows under its own name*
- `authored-name` — An entry carrying its own credential or binding name raises `StoreNameAuthored`.
  *A-surface#configuration-reaches-engine*
- `shared-registry` — Query, Admin and the embeddable client library resolve store identities from the same deployment registry.
  *A-surface#configuration-reaches-engine*

## visualize

The Admin page's operational views.

- `workflow-canvas` — Admin projects the store-published pipelines, jobs, models, publications and external steps as one node–edge graph under {{surface.visualize.dataflow-edges}}, with each pipeline's schedule, steps and latest run outcome.
  *A-surface#workers-reach-orchestrator*
- `dataflow-edges` — The published workflow lists edges `{from, to, via}` between typed node ids: pipeline and build-job `after`, pipeline to produced table, table or model to its reading model or job, build job to model, job to written table and publication.
  *because a graph the client infers from names draws edges the store never declared*
- `external-step` — Admin draws each `[[external]]` declaration as a node marked as run outside the engine, joined by its inputs and outputs; the engine runs, schedules and records nothing for it.
  *because work around the store belongs on the canvas without becoming a job kind {{surface.fire.job-kind-unknown}} refuses*
- `external-shape` — An `[[external]]` declares `name`, optional `label`, and `inputs` and `outputs` of typed references `table:<name>`, `pipeline:<id>` or `job:<name>`; any other key fails {{surface.apply.validation}}.
  *because a typed reference keeps a table and a pipeline sharing a name distinct, and no command key makes it runnable*
- `external-unbound` — An `[[external]]` input or output lacking a `table:`, `pipeline:` or `job:` prefix, or naming no declared table, model, pipeline or job, raises `ExternalStepUnbound` at validation.
  *because an edge to nothing draws a line the operator cannot trace*
- `external-collision` — An `[[external]]` whose `name` repeats another external's name, a job's name or a pipeline's id raises `ExternalStepCollision` at validation.
  *because one name labelling two graph nodes leaves the reader to guess which one ran*
- `operational-record` — Admin's inspector, pack file surface and learnings view present the store-published operational record.
  *A-surface#connections-claim-version*
- `listing-page` — One listing call answers at most 1000 entries, flags truncation, and counts the keys the read route declines to serve.

## package

The client library, its four deployment shapes and transports, and the credential each shape carries.

- `stdio-credential` — The process transport requires a verified capability token; `--owner` selects its signed owner claim. An absent token raises `StdioCredentialMissing` before protocol framing and never resolves to owner context.
  *A-read#read-surface-admits*
- `store-selector` — The child's working directory selects the store by walking up to the project manifest; finding none raises `StoreSelectorAbsent` and exits before writing any protocol framing.
  *A-topology#public-surfaces*
- `owner-flag` — A process `--owner` admits the local owner context only with a verified signed claim bound to the selected store and an unrestricted read grant; any other credential raises `OwnerCredentialInvalid` before protocol framing.
  *A-surface#turn-compatibility-preserves*

The client library's four shapes, and who holds the credential in each:

```mermaid
flowchart LR
  LIB["client library"]
  subgraph host["host backend"]
    S1["same-origin proxy"]
    S2["service binding"]
  end
  subgraph browser["browser"]
    S3["engine-direct embed"]
  end
  subgraph local["local consumer"]
    S4["spawned engine child"]
  end
  subgraph engine["engine"]
    GW["gateway entrypoint"]
    ENG["engine HTTP face"]
  end
  LIB -->|"host request"| S1
  LIB -->|"bound call"| S2
  LIB -->|"viewer request"| S3
  LIB -->|"newline-framed JSON-RPC"| S4
  S1 -->|"injects the token"| ENG
  S2 -->|"forwarded call"| GW
  GW -->|"container-side credential"| ENG
  S3 -->|"per-viewer scoped token"| ENG
  S4 -->|"on spawn"| CK{"credential and manifest found?"}
  CK -->|"yes, answers"| LIB
  CK -->|"no credential: StdioCredentialMissing"| LIB
  CK -->|"no manifest: StoreSelectorAbsent"| LIB
```

## speak

The language every Query-visible string carries, the audience contract, and the deterministic backstops beneath it.

- `redactor-lookahead` — The streaming redactor buffers 128 chars across each chunk boundary, and no denylist entry is longer than the buffer.
  *because an identifier split between two chunks otherwise passes unredacted*
- `unanswerable-suggestion` — A suggested prompt whose answer under these rules is a decline raises `ConsoleSuggestionUnanswerable` when the suggestion set is built.
  *A-surface#answer-surfaces-read*
- `stream-redaction` — The server redacts each streamed model chunk before Query emits prose to the operator.
  *A-surface#answer-surfaces-read*

## ground

Where an answer's material comes from: the closed tool set, the two trust layers, the per-reader credential, the web supplement and the sources block.

- `ungrounded-answer` — A path composing answer prose in a turn holding no tool result raises `ConsoleUngroundedAnswer`.
  *P2*
- `unadmitted-tool` — A call naming a tool the turn's packs do not admit raises `ConsoleToolNotAdmitted` and dispatches nothing.
  *A-surface#answer-surfaces-read*
- `mutating-tool` — The console's query endpoint admits a read subset: a client-reachable path naming a write raises `ConsoleMutatingToolRequested`. The one write a turn performs is authored on the server.
  *A-surface#answer-surfaces-read*
- `org-face-read-only` — A pack registering a write tool on an organization-wide face raises `ConsoleWriteToolOnOrgFace` at startup, naming the pack and the tool, and the face serves nothing.
  *A-surface#answer-surfaces-read*
- `direct-file-read` — A table function resolving a path straight against stored bytes raises `ConsoleFileAccessDirect` on this surface.
  *P5*
- `mint-refused` — A refused mint raises `ConsoleTokenExchangeRefused`. A store carrying a shared credential falls back to it; one carrying none surfaces the refusal to the reader.
  *P2*
- `sources-per-turn` — A source list carries at most 8 entries.
- `sources-block` — Every grounded answer includes a sources block citing only the governed results its admitted read tools returned.
  *A-surface#answer-surfaces-read*

The two trust layers on one grounded turn, for a store on `exchange` authentication:

```mermaid
sequenceDiagram
  box operator
    participant B as browser
  end
  box perimeter
    participant P as identity perimeter
  end
  box console
    participant S as console server
  end
  box store
    participant X as exchange route
    participant E as store engine
  end
  B->>P: same-origin request
  P->>S: request and the perimeter's assertion
  S->>S: re-verify the assertion, take the operator's address
  S->>X: post the verified assertion
  alt mint refused
    X-->>S: ConsoleTokenExchangeRefused, shared credential or refusal shown
  else minted
    X-->>S: per-operator credential, cached within its lifetime
  end
  S->>S: pick tool and arguments from the admitted packs
  S->>E: read tool call under the operator's credential
  E-->>S: rows through enforced relations
  S-->>B: grounded prose and its source list
```

## plan-turn

The shape of one turn: planner scaffolding, the replanning round, the answerability test, the code path, and the store's prompt overlay.

- `planner-reached-memory` — Scaffolding and every deterministic fallback cover data tables; scaffolding naming a memory relation raises `ConsolePlannerReachedMemory`.
  *A-surface#answer-surfaces-read*
- `code-path-bounds` — The code path reads at most 5000 rows per data table and stops after 10 s.
- `overlay-cache` — The overlay caches for 5 min, a miss included.
- `overlay-length` — An overlay is truncated at 8000 chars.
- `turn-flow` — A Query turn plans over admitted read tools, replans once after an unanswered first round, and synthesizes from the resulting governed rows.
  *A-surface#answer-surfaces-read*
- `overlay-audience` — A store's prompt overlay reaches the operator-facing synthesis text and stays outside the planner's text.
  *A-surface#turn-compatibility-preserves*

## set-vantage

A session's time basis: its vantage, the bound each leg carries, the snapshot timeline and the arrivals strip.

- `unparseable` — A vantage that is neither a calendar day nor an instant raises `ConsoleVantageUnparseable`, answered `400`.
  *P2*
- `web-bound-unparseable` — A web leg whose bound fails to parse fails that leg and raises `ConsoleWebBoundUnparseable`.
  *P2*
- `sample-labels` — A table's arrivals contribute at most 3 entries of sampled label.

## browse

What a store advertises before a question: discovered chips, humanized labels, the insights panel and the file gallery.

- `discovered-chips` — Query builds pre-question chips from described tables admitted to the reader's zone and humanizes their table labels.
  *A-surface#answer-surfaces-read*
- `insights-panel` — The insights panel presents row counts returned by the reader's table descriptions.
  *because a row count from the read face already reflects the reader's relation*
- `file-gallery` — The file gallery presents only paths returned by the reader's governed file listing and previews them through the read face.
  *P5*
- `gallery-path-unlisted` — A gallery preview naming a path absent from the governed file listing under the request's `as_of` raises `ConsoleGalleryPathUnlisted` before preview dispatch.
  *because the browser must not turn an arbitrary path into a store read*

#### Scenarios

- `surface.browse.gallery-path-unlisted`: WHEN a Query client requests a path absent from the reader's bound file listing, THEN it raises `ConsoleGalleryPathUnlisted` without a preview call.

## learn

The reading loop's memory: recall ahead of planning, the per-turn distillation and the labels its conclusions inherit.

- `distillation` — After the answer streams, a second pass distils the exchange into at most 3 entries shaped `{subject, key, learning}`, zero included.
- `unscoped` — A distilled conclusion landing without the reading-session scope raises `ConsoleLearningUnscoped`.
  *A-surface#answer-surfaces-read*
- `write-refused` — A served claim write that refuses or returns no scope-bound receipt raises `ConsoleLearningWriteRefused` to Query before answer publication.
  *because a failed write cannot count as a durable learning*
- `subject-resolution` — Distillation preserves the subject it observes; recall resolves that subject through entity matching.
  *A-surface#answer-surfaces-read*

## render

The three channels, the component union, deterministic component choice, view hints and the sanitizer walk.

- `client-authored-view` — A view specification arriving from a client, or composed by the model, raises `ConsoleViewNotServerBuilt`.
  *A-surface#answer-surfaces-read*
- `component-choice` — One row with one measure draws a metric; a date column with a measure over 3 rows on distinct days draws a line; anything else draws a table offering a bar view.
- `older-transcript` — A client reading a transcript with an unknown component-union member renders its view as a table.
  *A-surface#answer-surfaces-read*

A tool return split into three channels, and the view channel's path to a widget:

```mermaid
flowchart LR
  subgraph server["console server"]
    TR["tool return"]
    SV{"built from a result?"}
    H{"hint binds returned columns?"}
    VAL{"props valid?"}
    WALK["redactor"]
    NONE["prose only"]
  end
  subgraph browser["browser"]
    TP["trace panel"]
    W["widget frame"]
  end
  TR -->|"grounding"| M(["model endpoint"])
  TR -->|"internals, on request"| TP
  TR -->|"view rows"| SV
  CV(["client or model"]) -->|"authored view"| SV
  SV -->|"yes"| H
  SV -->|"no: ConsoleViewNotServerBuilt"| CV
  H -->|"yes, hinted component"| VAL
  H -->|"no, inferred component"| VAL
  VAL -->|"no"| NONE
  VAL -->|"yes"| WALK
  WALK -->|"one frame per shape"| W
```

## brief

The proactive greeting card: its derivation, payload, matching tiers, client-side clock and the absence it earns.

- `absence-is-earned` — The card needs a session with no turns, at present time, a live conclusion, an arrived row matching an interest, and a derivation inside its budget. An error or timeout raises `ConsoleBriefUnavailable`, and no card renders.
  *A-surface#answer-surfaces-read*
- `topic-tier` — The topic tier needs at least 2 tokens shared between the conclusion's subject and text and the row's label and topics, one of them naming the subject.
- `card-subjects` — A card carries at most 3 subjects.
- `articles-per-subject` — A subject carries at most 3 entries of matched article.
- `catchup-window` — The backend caps the requested window at 7 d.
- `greeting` — Query derives a greeting card from arrived rows matching live conclusions in a turnless present-time session.
  *A-surface#answer-surfaces-read*

## publish-answer

The moment an answer computed for one operator reaches a place other people read.

- `share-affordance` — A surface offering a share control on access-explanation output raises `VisibilityShareAffordance`.
  *A-surface#answer-surfaces-read*
- `askerless-audience` — A scheduled job posting to an audience under a service identity raises `VisibilityAskerlessAudience`, naming the destination; a scheduled post's corpus is narrowed in the reviewed manifest to what the destination reaches.
  *A-surface#answer-surfaces-read*
- `one-operator-principal` — An answer uses one operator's credential; the console offers no room-intersection principal.
  *A-surface#answer-surfaces-read*

## Shapes

A store registry entry, and the names derived from it:

```json
[
  {
    "id": "field-notes",
    "label": "Field notes",
    "endpoint": "https://field-notes.example.net",
    "auth": "exchange",
    "exchangeRoute": "/auth/exchange",
    "packPrefix": "packs/field-notes/"
  }
]
```

```
secret   FIELD_NOTES_QUERY_TOKEN
binding  FIELD_NOTES
```

An external step, and the edges it adds to the published workflow:

```toml
[[external]]
name    = "review"
label   = "Operator review"
inputs  = ["table:normalize_records"]
outputs = ["pipeline:enrich"]
```

```json
{
  "externals": [{ "name": "review", "label": "Operator review", "inputs": ["table:normalize_records"], "outputs": ["pipeline:enrich"] }],
  "edges": [
    { "from": "table:normalize_records", "to": "external:review", "via": "input" },
    { "from": "external:review", "to": "pipeline:enrich", "via": "output" }
  ]
}
```

A tool return, split three ways:

```json
{
  "grounding": "3 records landed on 2 Mar, the newest from Northwind at 09:14.",
  "view": {
    "component": "table.v1",
    "props": { "columns": ["Published on", "Publisher"], "rows": [["2 Mar", "Northwind"]] }
  },
  "internals": { "tool": "context.query", "rows": 3, "elapsed_ms": 41, "redactions": 0 }
}
```

A table's view hint:

```toml
[tables.daily_levels.view]
component = "band.v1"
x         = "observed_on"
measure   = "midpoint"
low       = "band_low"
high      = "band_high"
series    = "resource"
unit      = "units"
```

The catch-up payload one greeting renders from:

```json
{
  "since": "<instant>",
  "clamped": false,
  "totalNew": 214,
  "items": [
    {
      "subject": "northwind",
      "display": "Northwind",
      "learning": "Northwind has new records this week.",
      "tier": "derived",
      "articles": [
        { "title": "Northwind publishes new records", "source": "example-press.com",
          "url": "https://example-press.com/northwind-records", "published_on": "<day>" }
      ],
      "followUp": "What changed for Northwind this week?"
    }
  ]
}
```

One turn, from question to rendered answer:

```mermaid
flowchart LR
  RDR(["operator"]) -->|"question"| PLAN["planner"]
  MEM[("memory tables")] -->|"recall at vantage"| PLAN
  PLAN -->|"plan over time window"| TOOLS["admitted read tools"]
  TOOLS -->|"grounding rows"| A{"answered by the code test?"}
  A -->|"no, first round"| PLAN
  A -->|"no, second round"| RET["content-token retrieval"]
  A -->|"yes"| SYN["synthesizer"]
  RET -->|"retrieved passages"| SYN
  SYN -->|"redacted, humanized prose"| PAGE["console page"]
  PAGE -->|"prose, widget, sources"| RDR
  SYN -->|"distilled conclusions"| MEM
```
