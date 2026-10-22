---
contract: connector
owns:
  - drive-sheet
  - drive-changes
  - drive-domain-auth
---

# Drive extensions

The Drive extensions cover structured Sheets rows, change cursors and domain-approved access.

## drive-sheet

Structured capture of Google Sheets as source rows beside the existing PDF page path.

- `source-name` — The `gdrive-sheet` source serves `rows` and shares {{connector.source.drive-oauth}} and {{connector.source.drive-file-selection}} without changing the `gdrive` source's `files` and `pages` tables.
- `export-format` — A Google Sheet selected for structured capture exports as XLSX; CSV contains only the first sheet.
- `sheet-identity` — Each landed spreadsheet row carries `file_id`, sheet id, sheet name, source row number, connection identity, `tenant` and `principal`.
- `row-key` — A spreadsheet row key joins connection identity, file id, sheet id and source row number; a complete recapture tombstones keys absent from the new workbook.
- `cell-values` — The row preserves each cell's typed value and its column position; an empty cell is distinct from a missing row.
- `formula-value` — A formula cell lands its exported result and records that the source cell was a formula; the row does not evaluate the formula inside Contextful.
- `export-refusal` — A workbook whose export exceeds {{connector.source.drive-file-cap}} lands a named skipped file row and no partial spreadsheet rows.
  *because a truncated workbook cannot establish row identity or removal*

#### Scenarios

- `connector.drive-sheet.sheet-identity`: WHEN a workbook has two sheets with row 3 on each, THEN their landed keys differ by sheet id.
- `connector.drive-sheet.row-key`: WHEN a complete recapture removes a row, THEN the old row key receives a tombstone.

## drive-changes

The change cursor and full-walk handoff for a selected Drive corpus.

- `initial-token` — An initial full walk obtains a start page token before listing and commits that token with the complete walk, so changes during listing remain replayable.
- `cursor-scope` — The cursor binds the connection, credential principal, declared drive, selected roots or file ids, and capture mode; a changed binding starts a full walk.
- `page-commit` — A change read follows every `nextPageToken` and advances to `newStartPageToken` only after the resulting rows and removals commit.
- `selection-filter` — Each changed file is admitted only when its current ancestry or id satisfies the bound selection; a move outside it lands tombstones.
- `removed-file` — A removed change withdraws the file and its pages only after the source establishes that the selected file is gone or inaccessible.
- `invalid-token` — A rejected or expired change token starts a full selected walk; the source advances no removals from the incomplete change response.
  *because a missing change interval cannot prove the retained files still satisfy the selection*

#### Scenarios

- `connector.drive-changes.page-commit`: WHEN a change response has another page, THEN the committed cursor does not skip that page.
- `connector.drive-changes.invalid-token`: WHEN Drive rejects the cursor, THEN a complete walk reconciles removals before a new token commits.

## drive-domain-auth

Organization-approved Drive access and the principal whose authority each request carries.

- `delegation-grant` — Domain-wide delegation requires an operator-declared Workspace domain, authorized service account and approved scope set bound to the connection.
- `impersonated-principal` — Every delegated Drive request names one admitted Workspace user as its subject; the row and permission sweep retain that subject's identity.
- `domain-boundary` — A delegated request for a subject outside its declared domain raises `ConnectorDriveDomainRejected` before token minting.
  *because administrator approval for one domain conveys no authority over another*
- `organization-export` — A sanctioned organization-wide export declares its scope and disclosure posture explicitly under {{connector.source.twin-api}}; delegated user reads do not imply that export.

#### Scenarios

- `connector.drive-domain-auth.domain-boundary`: WHEN a connection names a user outside its approved domain, THEN `ConnectorDriveDomainRejected` precedes any Google request.
