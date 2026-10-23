---
contract: connector
owns:
  - drive-picks
  - drive-sheet
  - drive-changes
  - drive-domain-auth
---

# Drive extensions

The Drive extensions cover Picker-selected files, structured Sheets rows, change cursors and domain-approved access.

## drive-picks

Individual files a principal selects through Picker, their position, and the rows a tenant connection lands from them.

- `picker-source` — The `gdrive-picks` source serves `files` and `pages` with {{connector.source.drive-oauth}} and {{connector.source.drive-page-grain}} for files selected through Picker.
- `file-selection` — On `gdrive-picks` and `gdrive-sheet`, `file_ids` selects distinct individual files without a folder root; `folder_id` and `folder_ids` are absent.
  *A-connector*
- `file-selection-limit` — `file_ids` holds at most 128 entries.
- `file-selection-invalid` — An empty, repeated or mixed `file_ids` selection, or one exceeding {{connector.drive-picks.file-selection-limit}}, raises `ConnectorDriveFilesInvalid` before a Drive request.
  *because an ambiguous selection cannot establish which files a later tombstone withdraws*
- `file-access` — The source validates every selected file through the bound credential; a never-captured inaccessible or trashed id, a non-file id, or a file outside a configured `drive_id` raises `ConnectorDriveFileRejected` without landing rows or advancing position.
  *because an inaccessible picker id cannot count as a complete selection*
- `file-revoked` — A captured file that becomes inaccessible or trashed under a valid credential lands tombstones for its file and pages once and enters the position's withdrawn set; an authentication or transient failure advances no position.
- `file-withdrawn` — A withdrawn id stays selected without refusing later fires: a fire still finding it inaccessible lands nothing for it, and a fire reading it again captures it as a new file and drops it from the withdrawn set.
  *A-connector*
- `export-limit` — A picked file whose export Drive answers `403 exportSizeLimitExceeded` lands under {{connector.source.drive-export-limit}}, not as revoked: its id stays out of the withdrawn set and its `files` row lands no tombstone.
  *because Drive refuses that export on every fire while the principal still reads the file, and a tombstone withdraws a file nobody revoked*
- `file-position` — The position binds sorted `file_ids` and mode; changing either rereads retained files, and a complete read tombstones deselected files and their pages.
  *A-connector*
- `row-scope` — A connection-bound run stamps its Drive `files` and `pages` rows and tombstones with the admitted `tenant` and `principal`; the source cannot supply either value.
- `row-identity` — A connection-bound Drive row and position key include connection identity beside `file_id`, so two connections selecting one file cannot overwrite each other.

unsettled: Does Picker selection of a folder under `drive.file` grant the app access to its descendant files? owner: connector affects: connector.drive-picks

#### Scenarios

- `connector.drive-picks.file-selection-invalid`: WHEN `file_ids` repeats one id or appears beside `folder_id`, THEN validation raises `ConnectorDriveFilesInvalid` before any request.
- `connector.drive-picks.file-access`: WHEN a picked id is inaccessible to the bound credential, THEN `ConnectorDriveFileRejected` commits neither rows nor position.
- `connector.drive-picks.file-access`: WHEN a picked file lies in another Shared Drive than the configured `drive_id`, THEN `ConnectorDriveFileRejected` commits neither rows nor position.
- `connector.drive-picks.file-revoked`: WHEN a captured file loses access under a valid credential, THEN its file and pages receive tombstones before the selection position advances.
- `connector.drive-picks.file-revoked`: WHEN two fires follow a captured file's revocation, THEN the first lands its tombstone, the second lands none, and both keep the other picked files in the position.
- `connector.drive-picks.file-withdrawn`: WHEN a withdrawn file becomes readable again, THEN the next fire lands its captured row and the position holds it outside the withdrawn set.
- `connector.drive-picks.export-limit`: WHEN a captured Sheet's new version exceeds Drive's export limit, THEN its `files` row lands `skipped`, never `removed`, and the position keeps it.
- `connector.drive-picks.file-position`: WHEN a file id leaves a complete selection, THEN its file row and prior pages land tombstones.
- `connector.drive-picks.row-scope`: WHEN two tenants select the same file id, THEN each landed row carries only its admitted tenant and principal.

## drive-sheet

Structured capture of Google Sheets as source rows beside the existing PDF page path.

- `source-name` — The `gdrive-sheet` source serves `rows` and shares {{connector.source.drive-oauth}} and {{connector.drive-picks.file-selection}} without changing the `gdrive` source's `files` and `pages` tables.
- `export-format` — A Google Sheet selected for structured capture exports as XLSX; CSV contains only the first sheet.
- `sheet-identity` — Each landed spreadsheet row carries `file_id`, sheet id, sheet name, source row number, connection identity, `tenant` and `principal`.
- `row-key` — A spreadsheet row key joins connection identity, file id, sheet id and source row number; a complete recapture tombstones keys absent from the new workbook.
- `cell-values` — The row preserves each cell's typed value and its column position; an empty cell is distinct from a missing row.
- `formula-value` — A formula cell lands its exported result and records that the source cell was a formula; the row does not evaluate the formula inside Contextful.
- `export-refusal` — A workbook whose export exceeds {{connector.source.drive-file-cap}} lands a named skipped file row and no partial spreadsheet rows.
  *because a truncated workbook cannot establish row identity or removal*
- `export-limit` — A workbook export refused as {{connector.drive-picks.export-limit}} lands a named skipped file row and keeps the workbook's prior spreadsheet rows and keys; only {{connector.drive-picks.file-revoked}} tombstones a selected workbook.
  *because an export over Drive's size limit leaves the workbook readable, so its rows still describe the principal's file*

#### Scenarios

- `connector.drive-sheet.sheet-identity`: WHEN a workbook has two sheets with row 3 on each, THEN their landed keys differ by sheet id.
- `connector.drive-sheet.row-key`: WHEN a complete recapture removes a row, THEN the old row key receives a tombstone.
- `connector.drive-sheet.export-limit`: WHEN a captured workbook's next export answers `403 exportSizeLimitExceeded`, THEN no spreadsheet row of it receives a tombstone.

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
