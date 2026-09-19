# D38 — Redistribution ships an allowlist and never reaches backwards

**Status:** accepted

## Context

A table can be withheld from replication under a licence bound. Root-level objects beside the tables — the catalog, the durable-record blobs, the memory database — can carry a withheld table's rows, and objects already uploaded stay fetchable by key.

## Decision

`authority.bound-redistribution` fails in the direction of not shipping, and names what an earlier push uploaded.

- While any table is withheld, a root object ships only when it is on the allowlist of objects proven free of row data: the mirrored permission tables and the stale-memory index. Any other root object raises `EnforceRootObjectNotAllowlisted` at push.
- Clearing the redistribution flag over a table an earlier push uploaded raises `EnforceStaleRedistributedObjects`, naming each implicated object and its remedy, up to 100 files plus the total, and removes nothing. The check runs before and after the upload loop.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Enumerate what ships; refuse and name left-behind objects *(chosen)* | — | A new root artifact does not replicate until listed; a retroactive bound needs an operator to clean the bucket by hand. |
| Name the objects that stay behind | Failure direction | A new object would ship by default carrying withheld rows, unreported. |
| Inspect each root object for row values at push | Soundness | A record blob's contents are not statically decidable, and every push would pay for the scan. |
| Delete left-behind objects automatically | Authority over the bucket | The engine is not the arbiter of retention; the deletion would be irreversible and could cut a replica from under its readers. |
| Proceed silently and drop the table from the manifest | Operator knowledge | The operator would believe the bound reached backwards while every key stays fetchable. |

## Consequences

- An object added by someone unaware of this rule stays behind.
- A push refuses until the bucket is cleaned, for as long as an uploaded record may carry the table's rows.
- A replica keeps resolving permissions and staleness while tables are withheld.
