# 0143 — A URL carries scheme, host, port and path into any diagnostic, and userinfo is refused at validation

**Status:** accepted 2026-09-18
**Decides:** `secret.attach.refusal.credential-in-a-url`

## Context

A URL is a credential carrier in three places. A query parameter holds API keys and signed
tokens for a long tail of vendors. A fragment holds them for flows that never meant them to
reach a server. Userinfo holds a username and password inline, in the address itself.

Those URLs do not stay in the client. A failing request renders its URL into an error
message, an operator log line, and — through the error path — into a landed column, where it
becomes ordinary data that goes out to readers of the store. Redaction on the write path
masks what it matches, but a URL reaching a cell is a string among strings, and the cell is
read by people and by agents.

The narrower-looking approach is to scrub known credential shapes: match `api_key`,
`access_token`, `signature`, `sig`, and the handful of others. Every vendor spells its
parameter differently, and the list is not enumerable in advance — a new connector brings a
new spelling, and the first time anyone learns which one it was is when it appears in a
table.

Userinfo is a different case. Query and fragment arrive from the vendor's own URL
construction and the engine can only clean them up. Userinfo in a configured endpoint is
something an operator typed into a file that gets committed, and the credential sits in
version control regardless of what any diagnostic prints. Scrubbing it makes the output clean
and leaves the file exactly as wrong as it was.

## Decision

A URL reaching an error message, a log line or a landed column carries scheme, host, port and
path. Query, fragment and userinfo are dropped, each being a place an endpoint carries its
authorization. Metered vendor traffic and control-plane traffic each dispatch through one
client whose error mapping clears the URL as a typed edit ahead of rendering, and a URL
arriving by another route is scrubbed as text under the same rule. A configured endpoint
carrying userinfo raises `SecretCredentialInUrl` at validation, naming the source.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Drop query, fragment and userinfo wholesale; refuse configured userinfo** *(chosen)* | Coverage does not depend on knowing a vendor's parameter names, and an operator learns about a committed credential | Two failures at different pages of one walk render identically from a scrubbed URL |
| Scrub by matching known credential shapes | Diagnostics keep the query string, which is where page and filter state lives | Lost on coverage: a vendor's parameter name is not enumerable, so the first appearance of a new spelling is the one that lands in a table |
| Keep the query in operator-facing logs alone | An operator debugging a pull sees the full request; readers of the store see nothing | Lost on reach: the same string flows into a landed column through the error path, so "operator-facing" is not a boundary the string respects |
| Accept userinfo and scrub it | Diagnostics are clean, and an existing declaration keeps working | Lost on feedback: the operator keeps a credential in a committed URL and never learns, so the exposure that matters most is the one the scrub hides |
| Drop the path as well | Nothing vendor-specific survives at all | Lost on diagnosability with no matching gain: a path is part of the address an operator declared, and removing it leaves an error that names only a host |

## Criteria

1. **Coverage without enumeration** — whether the rule holds for a vendor nobody has
   integrated yet. *This is the criterion that decided the scrubbing rule.* Each dropped part
   is a place an endpoint carries its authorization, and a rule that depends on a list of
   parameter names fails silently and invisibly the first time the list is short.
2. **Where the string can reach** — that a URL flows from an error into a landed column, so
   any distinction between operator-facing and reader-facing output is not one the string
   observes. Rules out the log-only variant.
3. **Whether the operator learns** — whether a mistake is corrected or concealed. *This is the
   criterion that decided the userinfo refusal*, and it outranks compatibility because the
   credential's real exposure is the committed file, which no amount of output cleaning
   touches.
4. **Diagnosability** — how much an operator can tell from a rendered failure. It is what the
   accepted cost is paid in, and what keeps scheme, host, port and path.

## Consequences

A landed column and a log line carry the same scrubbed form, so no route through the system
produces a URL with authorization in it. Integrating a vendor requires no analysis of which
parameters are sensitive. An operator who put a password in a configured endpoint is told at
validation, with the source named, while the file is still being edited.

The accepted cost: two failures at different pages of one walk render identically from a
scrubbed URL. An operator debugging a paginated pull cannot tell from the message which page
failed, and the distinction lives in the run record's page fields instead — a second place to
look, and one that carries less than the URL did.

A second cost: the userinfo refusal breaks an existing declaration outright rather than
degrading it, and a deployment whose vendor only accepts inline credentials has to move them
into a header or a query the host does not construct.

## Revisit triggers

- Run-record page fields are observed insufficient for diagnosing paginated failures, so the
  scrubbed-URL ambiguity costs real debugging time rather than a click.
- A vendor class emerges that authenticates by query parameter and by nothing else, making the
  wholesale query drop remove the only way to tell two of its endpoints apart.
- The write-path redaction gains a typed URL value it can mask structurally, at which point a
  landed URL and a logged one can diverge safely.
