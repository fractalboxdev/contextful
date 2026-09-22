---
contract: surface
owns:
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

# The analyst console

The console is the surface a non-technical reader reaches a store through: one composer, one
transcript, and the widgets a turn draws beside its prose. It holds no data and no privileged
path under the tables; everything it shows arrives from a governed read, in the reader's own
language. The same store registry feeds the operator's views and the client library that
embeds search and ask in a third-party page.

The console between a reader and a store, and where it meets the read contract and the model endpoint:

```mermaid
flowchart LR
  RD["reader"] --> PER["identity perimeter"]
  subgraph browser["browser"]
    PAGE["console page"]
  end
  PER --> PAGE
  subgraph server["console server"]
    REGY["store registry"]
    CRED["one credential resolver"]
    PACKS["capability packs"]
    TURN["turn: recall, plan, rounds, synthesis"]
    CH["render: grounding, view, internals"]
    RED["redactor, temporal humanizer"]
  end
  subgraph read["read contract"]
    STORE["store engine"]
    MEM["memory tables"]
  end
  subgraph topology["topology contract"]
    MODEL["model endpoint"]
  end
  subgraph thirdparty["third-party page"]
    LIB["client library: four shapes"]
  end
  PAGE -- "same-origin routes" --> TURN
  REGY --> CRED
  PACKS --> TURN
  TURN -- "admitted read tools" --> STORE
  CRED --> STORE
  STORE --> CH
  CH -- "grounding" --> MODEL
  MODEL --> RED
  RED --> PAGE
  CH -- "view" --> PAGE
  TURN -- "distilled conclusions" --> MEM
  LIB --> STORE
  OPS["operator surface"] --> REGY
  PAGE -- "deliberate share" --> AUD["surface with an audience"]
```

## register-store

Which stores a deployment serves, how each store's credential and binding names derive, and how a request reaches a store's origin.

- `registry-unreadable` — A `CONTEXTFUL_STORES_JSON` value that does not parse raises `StoreRegistryUnreadable` at startup, and no built-in store stands in for the configured set.
  *P3*
- `malformed-entry` — One entry that does not decode raises `StoreEntryMalformed` naming it, and is dropped while its siblings are served.
  *A-surface*
- `reserved-id` — A configured entry claiming either built-in id raises `StoreIdReserved`.
  *because the entry otherwise reads the fixture's rows under its own name*
- `authored-name` — An entry carrying its own credential or binding name raises `StoreNameAuthored`.
  *A-surface*

## visualize

The operations canvas, the pack file surface, and the learnings record a store publishes about itself.

- `listing-page` — One listing call answers at most 1000 entries, flags truncation, and counts the keys the read route declines to serve.

## package

The client library, its four deployment shapes and transports, and the credential each shape carries.

- `stdio-credential` — Over the process transport a credential is mandatory, a capability token or an explicit owner flag; an unset one raises `StdioCredentialMissing` and does not resolve to the owner context.
  *A-read*
- `store-selector` — The child's working directory selects the store by walking up to the project manifest; finding none raises `StoreSelectorAbsent` and exits before writing any protocol framing.
  *A-topology*

The client library's four shapes, and who holds the credential in each:

```mermaid
flowchart LR
  LIB["client library: search, query, retrieve, recall, ask and kin"]
  subgraph host["host backend"]
    S1["same-origin proxy, injects the token"]
    S2["same-account: service binding"]
  end
  subgraph browser["browser"]
    S3["engine-direct embed: per-viewer scoped token"]
  end
  subgraph local["local consumer"]
    S4["spawned engine, newline-framed JSON-RPC"]
    CK{"credential set and project manifest found"}
    E1["StdioCredentialMissing"]
    E2["StoreSelectorAbsent"]
    CHILD["engine child: one request in flight"]
  end
  subgraph engine["engine"]
    GW["gateway entrypoint: query, mcp, health"]
    subgraph container["engine container"]
      ENG["HTTP face: grants of the presented token"]
    end
  end
  LIB --> S1
  LIB --> S2
  LIB --> S3
  LIB --> S4
  S1 --> ENG
  S2 --> GW
  GW -- "injects the container-side credential" --> ENG
  S3 --> ENG
  S4 --> CK
  CK -- "no credential" --> E1
  CK -- "no manifest" --> E2
  CK -- "yes" --> CHILD
```

## speak

The language every visitor-visible string carries, the audience contract, and the deterministic backstops beneath it.

- `redactor-lookahead` — The streaming redactor buffers 128 chars across each chunk boundary, and no denylist entry is longer than the buffer.
  *because an identifier split between two chunks otherwise passes unredacted*
- `unanswerable-suggestion` — A suggested prompt whose answer under these rules is a decline raises `ConsoleSuggestionUnanswerable` when the suggestion set is built.
  *A-surface*

## ground

Where an answer's material comes from: the closed tool set, the two trust layers, the per-reader credential, the web supplement and the sources block.

- `ungrounded-answer` — A path composing answer prose in a turn holding no tool result raises `ConsoleUngroundedAnswer`.
  *P2*
- `unadmitted-tool` — A call naming a tool the turn's packs do not admit raises `ConsoleToolNotAdmitted` and dispatches nothing.
  *A-surface*
- `mutating-tool` — The visitor-facing endpoint admits a read subset: a client-reachable path naming a write raises `ConsoleMutatingToolRequested`. The one write a turn performs is authored on the server.
  *A-surface*
- `org-face-read-only` — A pack registering a write tool on an organization-wide face raises `ConsoleWriteToolOnOrgFace` at startup, naming the pack and the tool, and the face serves nothing.
  *A-surface*
- `direct-file-read` — A table function resolving a path straight against stored bytes raises `ConsoleFileAccessDirect` on this surface.
  *P5*
- `mint-refused` — A refused mint raises `ConsoleTokenExchangeRefused`. A store carrying a shared credential falls back to it; one carrying none surfaces the refusal to the reader.
  *P2*
- `sources-per-turn` — A source list carries at most 8 entries.

The two trust layers on one grounded turn, for a store on `exchange` authentication:

```mermaid
sequenceDiagram
  box reader
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
  S->>S: re-verify the assertion, take the reader's address
  S->>X: post the verified assertion
  alt mint refused
    X-->>S: ConsoleTokenExchangeRefused, shared credential or refusal shown
  else minted
    X-->>S: per-reader credential, cached within its lifetime
  end
  S->>S: pick tool and arguments from the admitted packs
  S->>E: read tool call under the reader's credential
  E-->>S: rows through enforced relations
  S-->>B: grounded prose, then a source list of 8 entries or fewer
```

## plan-turn

The shape of one turn: planner scaffolding, the replanning round, the answerability test, the code path, and the store's prompt overlay.

- `planner-reached-memory` — Scaffolding and every deterministic fallback cover data tables; scaffolding naming a memory relation raises `ConsolePlannerReachedMemory`.
  *A-surface*
- `code-path-bounds` — The code path reads at most 5000 rows per data table and stops after 10 s.
- `overlay-cache` — The overlay caches for 5 min, a miss included.
- `overlay-length` — An overlay is truncated at 8000 chars.

unsettled: Does a store's overlay reach the planner's text as well as the analyst's? owner: console affects: surface.plan-turn

## set-vantage

A session's time basis: its vantage, the bound each leg carries, the snapshot timeline and the arrivals strip.

- `unparseable` — A vantage that is neither a calendar day nor an instant raises `ConsoleVantageUnparseable`, answered `400`.
  *P2*
- `web-bound-unparseable` — A web leg whose bound fails to parse fails that leg and raises `ConsoleWebBoundUnparseable`.
  *P2*
- `sample-labels` — A table's arrivals contribute at most 3 entries of sampled label.

## browse

What a store advertises before a question: discovered chips, humanized labels, the insights panel and the file gallery.

## learn

The reading loop's memory: recall ahead of planning, the per-turn distillation and the labels its conclusions inherit.

- `distillation` — After the answer streams, a second pass distils the exchange into at most 3 entries shaped `{subject, key, learning}`, zero included.
- `unscoped` — A distilled conclusion landing without the reading-session scope raises `ConsoleLearningUnscoped`.
  *A-surface*

unsettled: Does a subject normalize during distillation, or resolve through entity matching at recall? owner: console affects: surface.learn

## render

The three channels, the component union, deterministic component choice, view hints and the sanitizer walk.

- `client-authored-view` — A view specification arriving from a client, or composed by the model, raises `ConsoleViewNotServerBuilt`.
  *A-surface*
- `component-choice` — One row with one measure draws a metric; a date column with a measure over 3 rows on distinct days draws a line; anything else draws a table offering a bar view.

A tool return split into three channels, and the view channel's path to a widget:

```mermaid
flowchart TD
  subgraph server["console server"]
    TR["tool return"] --> G["grounding channel"]
    TR --> I["internals channel, on request"]
    TR --> V["view channel"]
    CV["view from a client or the model"] --> E1["ConsoleViewNotServerBuilt"]
    V --> H{"view hint binds only returned columns"}
    H -- "yes" --> HC["hinted component"]
    H -- "no" --> INF["inference: metric, line over 3 distinct days, else table"]
    HC --> VAL{"valid against the props schema"}
    INF --> VAL
    VAL -- "no" --> NONE["no widget"]
    VAL -- "yes" --> WALK["walk: identifier redactor, temporal rewriting"]
  end
  subgraph browser["browser"]
    TP["trace panel"]
    W["widget frame after the prose, one per shape"]
  end
  G --> M["model"]
  I --> TP
  WALK --> W
```

unsettled: What governs adding a member to the component union once transcripts saved under an older client exist? owner: console affects: surface.render

## brief

The proactive greeting card: its derivation, payload, matching tiers, client-side clock and the absence it earns.

- `absence-is-earned` — The card needs a session with no turns, at present time, a live conclusion, an arrived row matching an interest, and a derivation inside its budget. An error or timeout raises `ConsoleBriefUnavailable`, and no card renders.
  *A-surface*
- `topic-tier` — The topic tier needs at least 2 tokens shared between the conclusion's subject and text and the row's label and topics, one of them naming the subject.
- `card-subjects` — A card carries at most 3 subjects.
- `articles-per-subject` — A subject carries at most 3 entries of matched article.
- `catchup-window` — The backend caps the requested window at 7 d.

## publish-answer

The moment an answer computed for one reader reaches a place other people read.

- `share-affordance` — A surface offering a share control on access-explanation output raises `VisibilityShareAffordance`.
  *A-surface*
- `askerless-audience` — A scheduled job posting to an audience under a service identity raises `VisibilityAskerlessAudience`, naming the destination; a scheduled post's corpus is narrowed in the reviewed manifest to what the destination reaches.
  *A-surface*

unsettled: Is there a principal shape for an answer computed at a room's intersection rather than one reader's scope? owner: console affects: surface.publish-answer

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

A tool return, split three ways:

```json
{
  "grounding": "3 filings landed on 2 Mar, the newest from Northwind at 09:14.",
  "view": {
    "component": "table.v1",
    "props": { "columns": ["Filed on", "Publisher"], "rows": [["2 Mar", "Northwind"]] }
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
series    = "instrument"
unit      = "index pts"
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
      "learning": "Northwind's filings lead the sector by about a week.",
      "tier": "derived",
      "articles": [
        { "title": "Northwind files early again", "source": "example-press.com",
          "url": "https://example-press.com/northwind-files-early", "published_on": "<day>" }
      ],
      "followUp": "What changed for Northwind this week?"
    }
  ]
}
```

One turn, from question to rendered answer:

```mermaid
flowchart TD
  Q[Question] --> R[Recall at the vantage]
  R --> T[Resolve temporal intent to a window]
  T --> P[Plan against prefetched columns]
  P --> G[Admitted read tools]
  G --> A{Answered, by the code test?}
  A -- no, first round --> P
  A -- no, second round --> C[Content-token retrieval in code]
  A -- yes --> S[Synthesis over grounding]
  C --> S
  S --> PR[Streamed prose, redacted and humanized]
  PR --> W[Widget frame]
  PR --> SB[Sources block]
  PR --> D[Distillation, labels from grounding rows]
```
