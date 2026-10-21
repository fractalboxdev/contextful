# Google Drive API practice

| Operation | External evidence | Design consequence |
| --- | --- | --- |
| `connector.source` | [Choose Drive API scopes](https://developers.google.com/workspace/drive/api/guides/api-specific-auth) | `drive.file` admits files selected through Picker; folder descendant access requires a live consent test. |
| `disclosure.drive-sweep` | [Permissions list](https://developers.google.com/workspace/drive/api/reference/rest/v3/permissions/list) | Permission reads paginate and accept `drive.file` for files the app can access. |
| `connector.drive-sheet` | [Export MIME types](https://developers.google.com/workspace/drive/api/guides/ref-export-formats) | CSV covers only the first sheet; XLSX represents the workbook. |
| `connector.drive-changes` | [Retrieve changes](https://developers.google.com/workspace/drive/api/guides/manage-changes), [shared-drive support](https://developers.google.com/workspace/drive/api/guides/enable-shareddrives) | A start token and all change pages precede the new start token; shared-drive requests bind their drive id. |
| `connector.drive-domain-auth` | [When verification is not needed](https://support.google.com/cloud/answer/13464323), [Workspace app access](https://support.google.com/a/answer/7281227) | Admin trust admits a client in a domain; a request still carries a user or expressly sanctioned organization export identity. |
