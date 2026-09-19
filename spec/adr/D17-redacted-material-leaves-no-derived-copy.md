# D17 — Redacted material leaves no derived copy

**Status:** accepted

## Context

Redaction removes a value from what the store holds. Every structure built beside a column — an index, a journaled pull, an error string, a URL in a diagnostic — is a copy the redaction rule does not govern, and each copy travels to backups, replicas and developer machines.

## Decision

Redaction holds only when no derived artifact carries the pre-redaction value, so each path that could make such a copy is closed at its source.

- `store.encrypt` refuses an index declared over a column redacted at write time; no segment, graph, filter or zone map over it exists.
- `run.journal` refuses a pipeline pairing write-path redaction with a journaling source, at manifest validation, at reconcile and at run open.
- The derive tier sits on the journaling opt-out list; a crash re-pays for the run's units.
- `connector.attach` renders a URL in any diagnostic or landed column as scheme, host, port and path; configured userinfo is refused at validation.
- `run.land` passes every error string a derived row carries through address redaction at the row builder; an unredacted write refuses.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse each copy path at declaration or at the row builder *(chosen)* | — | A redacted column is reachable only by exact scan; redacting pipelines lose replay-without-refetch; a scrubbed error sometimes hides the part that explains it. |
| Encrypt the index or journal and permit it | Completeness of the at-rest claim | The guarantee would rest on key custody for content redaction exists to remove. |
| Build locally, strip at the sync edge | Number of failure points | The local tree is itself a copy target. |
| A second redaction rule over the raw copy | Drift | Two rules encoding one idea would disagree on the value that matters. |
| Redact in each adapter, or at read time | Uniformity | The guarantee would be as strong as the least careful adapter, and stored material outlives every reader that forgets. |

## Consequences

- Queries over redacted columns pay a full scan.
- A derive crash costs up to one run's worth of metered inference.
- Two failures at different pages of one walk render identically from a scrubbed URL.

## Revisit

- An index structure whose contents are provably independent of the indexed values.
- Redaction runs ahead of the journal, making a journaled payload covered by the same rule.
