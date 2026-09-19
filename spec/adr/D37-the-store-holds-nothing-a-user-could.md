# D37 — The store holds nothing a user could not see

**Status:** accepted

## Context

Query-time enforcement bounds what a given read returns, not what the store contains. An operator credential, a bypassed view or a future surface reaches the stored bytes directly, so the ceiling on harm is set at the write path.

## Decision

The store's ceiling is at or below what people in the organization can see, and nothing a person cannot see enters it.

- `authority.refuse` rejects a credential-shaped value at the write boundary with `EnforceCredentialShapedValue`, per value, so the surrounding rows land. A source legitimately carrying such text lands under an operator-declared exemption.
- An organization-twin API — security, eDiscovery, legal-hold export, licensed to see what no individual sees — raises `ConnectorTwinApiSource` at `connector.source` for every source. A sanctioned, paid, disclosed organization-wide export path is admitted.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse at the write boundary; refuse twin APIs everywhere *(chosen)* | — | A security corpus needs an explicit exemption; complete workspace coverage means per-user ingestion under per-user authority. |
| Store the value and flag it, or withhold it at query time | Reach | The value would already sit in the durable record and every replica, readable with an object-store credential. |
| Refuse the whole batch containing the value | Proportion | One pasted key would stop the pull on every retry. |
| Ingest through the twin API and restrict at query time | The ceiling | No query-time restriction lowers what the store contains. |
| Permit the twin API behind an organization-signed disclosure | Consent of the people ingested | The organization's signature is not consent from the people whose private content lands. |

## Consequences

- Query-time enforcement bounds harm rather than being the only thing preventing it.
- Removing a credential never needs a rewrite across replicas, because it never landed.
- Per-user ingestion costs more connections and more rate-limit budget.
