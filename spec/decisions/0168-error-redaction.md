# 0168 — Every error string a derived row carries passes address redaction

**Status:** accepted 2026-09-18
**Decides:** `derive.land.refusal.unredacted-error`

## Context

`last_error` is a column in a landed table. It is not a log line: it survives the run that
produced it, it is queried by consoles and by consumers joining derived output to its parent, and
it passes through the same retrieval surface as content. Whatever an engine puts in it is stored
for as long as the table exists.

The units this column reports on are row-supplied addresses. A transcribe unit's media column
holds an enclosure address that routinely carries a subscriber token in its query string. A
`link_preview` unit's address came from a third party's page. Pre-signed links — the common shape
for private media — carry their authorization entirely in the query string, which means the
address is the credential. Error messages are built by including the thing that failed, so a
client's transport error, a redirect diagnostic and an upstream excerpt all quote the address
verbatim by default. `Upstream::excerpt` is bounded at 4 KiB and carries no request body precisely
because an error string is where credential material most readily travels back out; the bound
limits the volume, not the kind.

The engines producing these strings are adapters, some of them third-party wrappers. Relying on
each of them to redact is relying on the least careful one, and a single unredacted write is
permanent in a way a log line is not.

Dropping the error entirely would remove the exposure and remove the diagnosis with it. The
operator reading a marker needs to distinguish a host that is unreachable from one that refused
them, a certificate failure from a timeout. Without it the marker says only that something went
wrong, and the fix — a host to add, a credential to rotate, a publisher to stop scanning — is
unguessable.

## Decision

Every non-null write of `last_error` passes address redaction; an unredacted write raises
`DeriveUnredactedError`. The same applies to `probe_error` on a link row. Redaction is applied at
the row builder rather than at each engine, so an adapter cannot omit it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Redact every error string at the row builder, refuse an unredacted write** *(chosen)* | Authorization material inside a row-supplied address cannot reach a reader; diagnosis survives | A redacted error sometimes hides the part of an address that explains the failure |
| Land the raw error | Complete diagnostic text; no redaction machinery | Loses on the deciding criterion: the units this column reports on are row-supplied addresses, where a pre-signed link carries its authorization in its query string, and the exposure is permanent |
| Drop the error entirely, keep only the status | Nothing to redact; the smallest possible column set | Loses on diagnosis: an operator cannot tell a dead host from a refused one, so no marker suggests a fix |
| Redact in each adapter | Adapter-specific knowledge of what its vendor puts where | Loses on uniformity: the guarantee is only as strong as the least careful adapter, and a third-party wrapper cannot be reviewed the way the builder can |
| Store the raw error and redact at read time | Full fidelity retained for a privileged reader | Loses on permanence: the material is in the table, so every future reader, export, replica and backup carries it, and one path that forgets to redact exposes all of it |

## Criteria

1. **Whether authorization material inside a row-supplied address reaches a reader.** Measured by
   what a console user querying the column can see.
2. **Diagnosability** — whether an operator can tell the failure modes apart well enough to act.
3. **Uniformity of the guarantee** — whether it holds regardless of which adapter produced the
   string.
4. **Fidelity** — how much of the original diagnostic survives.

Criterion 1 decides. The column is durable and broadly readable, so an exposure here is not an
incident with a window but a fact in the store; criterion 4 is what gets traded, and criterion 2
is preserved because redaction removes the query string rather than the message. Criterion 3 is
why the check lives at the builder and is a refusal rather than a convention — a guarantee that
depends on every adapter author is not a guarantee.

## Consequences

Easier: a console can show `last_error` to whoever can see the table, with no per-column
authorization reasoning. An adapter author writes ordinary error text and does not need to know
what a pre-signed address looks like.

Harder: redaction is applied to text an engine composed, so it operates on strings rather than on
structured fields, and a message that embeds an address in an unusual shape may pass through less
redacted than intended or more. The refusal catches a write that skipped redaction, not one whose
redaction was incomplete.

Accepted cost: a redacted error sometimes hides the very part of an address that explains the
failure — a token that expired, a path segment naming the wrong tenant, a query parameter the
publisher requires. An operator hitting that case reproduces the request by hand against the
parent row's own address, which is slower and is the only route left.

Expensive to reverse: rows already landed carry redacted text and the original is not kept
anywhere, so relaxing the rule recovers nothing historical. The column's readability posture — and
any console that shows it broadly on the strength of this decision — is built on the redaction
holding.

## Revisit triggers

- Operators are observed unable to diagnose a recurring failure class because redaction removed
  the distinguishing part, which would argue for a structured error field rather than for landing
  raw text.
- An unredacted address is found in the column despite the builder check, which means redaction is
  incomplete rather than absent and changes what the refusal needs to test.
- The column's readership narrows to operators alone under an authority rule, which would reduce
  the weight criterion 1 carries against criterion 4.
