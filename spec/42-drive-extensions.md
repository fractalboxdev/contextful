---
contract: connector
owns:
  - drive-picks
  - drive-sheet
  - drive-changes
  - drive-domain-auth
---

# Drive extensions

Connector implementers use these extensions to capture Picker-selected files and structured Sheets rows, track Drive changes and enforce domain-approved access.

## drive-picks

Files a principal selects through Picker, the selection position, and the rows a tenant connection stores.

- `picker-source` — The `gdrive-picks` source serves `files` and `pages` with {{connector.source.drive-oauth}} and {{connector.source.drive-page-grain}} for files selected through Picker.
- `file-selection` — On `gdrive-picks` and `gdrive-sheet`, `file_ids` selects distinct individual files without a folder root; `folder_id` and `folder_ids` are absent.
  *A-connector#picker-files-bind*
- `file-selection-limit` — `file_ids` holds at most 128 entries.
- `file-selection-invalid` — An empty, repeated or mixed `file_ids` selection, or one exceeding {{connector.drive-picks.file-selection-limit}}, raises `ConnectorDriveFilesInvalid` before a Drive request.
  *because an ambiguous selection cannot establish which files a later tombstone withdraws*
- `file-access` — The source validates each selected file with the bound credential. An uncaptured inaccessible or trashed id, non-file id, or file outside configured `drive_id` raises `ConnectorDriveFileRejected` without landing rows or advancing position.
  *because an inaccessible picker id cannot count as a complete selection*
- `file-revoked` — When a captured file becomes inaccessible or trashed under a valid credential, the source tombstones its file and pages once. The position adds its id to the withdrawn set. Authentication and transient failures advance no position.
- `file-withdrawn` — A withdrawn id stays selected without refusing later fires. A fire finding the file inaccessible stores nothing for that file. When access returns, the source captures the file as new and removes its id from the withdrawn set.
  *A-connector#picker-files-bind*
- `export-limit` — If Drive answers a picked file's export with `403 exportSizeLimitExceeded`, the source applies {{connector.source.drive-export-limit}}. The file id stays outside the withdrawn set, and its `files` row receives no tombstone.
  *because Drive refuses that export on every fire while the principal still reads the file, and a tombstone withdraws a file nobody revoked*
- `file-position` — The position binds sorted `file_ids` and mode; changing either rereads retained files, and a complete read tombstones deselected files and their pages.
  *A-connector#picker-files-bind*
- `row-scope` — A connection-bound run stamps its Drive `files` and `pages` rows and tombstones with the admitted `tenant` and `principal`. The source supplies neither value.
- `row-identity` — A connection-bound Drive row and position key include connection identity beside `file_id`. Connections selecting the same file keep separate rows and positions.

unsettled: Does Picker selection of a folder under `drive.file` grant the app access to its descendant files? owner: connector affects: connector.drive-picks

#### Scenarios

- `connector.drive-picks.file-selection-invalid`: WHEN `file_ids` repeats one id or appears beside `folder_id`, THEN validation raises `ConnectorDriveFilesInvalid` before any request.
- `connector.drive-picks.file-access`: WHEN a picked id is inaccessible to the bound credential, THEN `ConnectorDriveFileRejected` commits neither rows nor position.
- `connector.drive-picks.file-access`: WHEN a picked file lies in another Shared Drive than the configured `drive_id`, THEN `ConnectorDriveFileRejected` commits neither rows nor position.
- `connector.drive-picks.file-revoked`: WHEN a captured file loses access under a valid credential, THEN its file and pages receive tombstones before the selection position advances.
- `connector.drive-picks.file-revoked`: WHEN two fires follow a captured file's revocation, THEN the first lands its tombstone, the second lands none, and both keep the other picked files in the position.
- `connector.drive-picks.file-withdrawn`: WHEN a withdrawn file becomes readable again, THEN the next fire lands its captured row and the position holds it outside the withdrawn set.
- `connector.drive-picks.export-limit`: WHEN a captured Sheet's new version exceeds Drive's export limit, THEN its `files` row lands `skipped` rather than `removed`, and the position keeps it.
- `connector.drive-picks.file-position`: WHEN a file id leaves a complete selection, THEN its file row and prior pages land tombstones.
- `connector.drive-picks.row-scope`: WHEN two tenants select the same file id, THEN each landed row carries only its admitted tenant and principal.

## drive-sheet

Google Sheets capture as structured rows, separate from PDF page capture.

- `source-name` — The `gdrive-sheet` source serves `rows` and shares {{connector.source.drive-oauth}} and {{connector.drive-picks.file-selection}} without changing the `gdrive` source's `files` and `pages` tables.
- `export-format` — A Google Sheet selected for structured capture exports as XLSX; CSV contains only the first sheet.
- `sheet-identity` — Each landed spreadsheet row carries `file_id`, sheet id, sheet name, source row number, connection identity, `tenant` and `principal`.
- `row-key` — A spreadsheet row key combines connection identity, file id, sheet id and source row number. A complete recapture tombstones keys absent from the captured workbook.
- `cell-values` — The row preserves each cell's typed value and its column position; an empty cell is distinct from a missing row.
- `formula-value` — The source stores a formula cell's exported result and records that the cell contains a formula. Contextful does not evaluate the formula.
- `export-refusal` — A workbook whose export exceeds {{connector.source.drive-file-cap}} lands a named skipped file row and no partial spreadsheet rows.
  *because a truncated workbook cannot establish row identity or removal*
- `export-limit` — When {{connector.drive-picks.export-limit}} rejects a workbook export, the source stores a named skipped file row and retains prior spreadsheet rows and keys. Only {{connector.drive-picks.file-revoked}} tombstones a selected workbook.
  *because an export over Drive's size limit leaves the workbook readable, so its rows still describe the principal's file*

#### Scenarios

- `connector.drive-sheet.sheet-identity`: WHEN a workbook has two sheets with row 3 on each, THEN their landed keys differ by sheet id.
- `connector.drive-sheet.row-key`: WHEN a complete recapture removes a row, THEN the old row key receives a tombstone.
- `connector.drive-sheet.export-limit`: WHEN a captured workbook's next export answers `403 exportSizeLimitExceeded`, THEN no spreadsheet row of it receives a tombstone.

## drive-changes

Change cursors and complete listings of a selected Drive corpus.

- `initial-token` — Before the initial full listing, the source obtains a start page token. The source commits the token with the complete listing, preserving replay of changes during listing.
- `cursor-scope` — The cursor binds the connection, credential principal, declared drive, selected roots or file ids, and capture mode. A changed binding starts a complete listing.
- `page-commit` — A change read follows every `nextPageToken` and advances to `newStartPageToken` only after the resulting rows and removals commit.
- `selection-filter` — The source admits a changed file only when its current ancestry or id satisfies the bound selection. Moving outside the selection produces tombstones.
- `removed-file` — For a removal change, the source withdraws the file and pages only after establishing that the selected file is absent or inaccessible.
- `invalid-token` — If Drive rejects or expires a change token, the source starts a complete selected listing. The incomplete change response advances no removals.
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
