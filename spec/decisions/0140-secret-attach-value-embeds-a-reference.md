# 0140 — An attach value embeds at least one reference

**Status:** accepted 2026-09-18
**Decides:** `secret.attach.refusal.literal-attach-value`

## Context

The `[attach]` block maps a header name to a value template in the reference grammar. The
host hydrates that template and writes the result onto a permitted request, overriding
whatever the guest supplied for that header name. The block exists for one reason: values the
host must hydrate and must be able to override, because guest code is not permitted to hold
the material they carry.

A template is literal text with `${secret://<name>}` placeholders, and a template with no
placeholder at all is still a valid template. That makes a constant header — `Accept`,
`User-Agent`, an API version — expressible in the block, and it is the natural place an
author reaches for when they want a header on every request.

Two things follow from accepting it. The block stops being readable as a list of hydrated
values, so a reviewer scanning it cannot tell at a glance which entries carry material. And a
typo becomes silent in the worst possible direction: an author who writes `Bearer
secret://vendor-token` without the brace-and-dollar wrapper has written a valid literal, and
the host dutifully sends the string `Bearer secret://vendor-token` to the vendor as a
credential. The request goes out. The vendor rejects it, or worse, the vendor logs it. Nothing
in the declaration path noticed that the author meant a reference.

The engine also has a nearby behavior that makes the literal case look safe: a template of
pure literal text hydrates into the same redacting wrapper as any other value, so handling
never depends on whether a reference was embedded. That protects the value in transit through
the process. It does not make the entry legible, and it does not catch the dropped
placeholder.

## Decision

An attach value embedding no reference raises `SecretLiteralAttachValue`. A constant header
belongs in the guest's own request construction, where a guest names the request and the host
adds nothing.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse a pure-literal attach value** *(chosen)* | Every entry in the block is a hydrated value, so a dropped placeholder is refused rather than sent | A vendor wanting a constant header beside a credential header needs both paths, one in the block and one in guest code |
| Accept a literal and attach it | The block expresses every header a source needs, in one place | Lost on legibility: a typo that drops the placeholder silently sends the placeholder text as a credential, and the block no longer reads as a list of hydrated values |
| Warn instead of refusing | Nothing breaks, and the author is told | Lost on the same failure reaching the vendor: a warning does not stop the request, and the malformed credential goes out exactly as before |
| Accept a literal unless it contains the scheme spelling | Catches the specific typo that motivates the rule | Lost on coverage: it catches one spelling of one mistake, leaves every other dropped-placeholder form accepted, and adds a rule a reader has to remember |

## Criteria

1. **Whether the block's purpose is legible** — whether a reader can tell, from the block
   alone, which entries carry material the host hydrates. *This is the criterion that decided
   it.* The block exists for values the host hydrates and overrides; a constant sitting there
   is indistinguishable from a forgotten reference, and the whole failure this refusal
   prevents is the reviewer who cannot tell the two apart.
2. **What reaches the vendor on a mistake** — whether a malformed entry results in a request
   or in a refusal. The warning option fails this directly.
3. **Coverage of the mistake class** — whether the rule catches dropped placeholders
   generally or one spelling of one case. Decides against the scheme-matching option.
4. **Declaration economy** — whether every header a source needs is expressible in one place.
   The chosen option loses here and accepts it.

## Consequences

Reading the `[attach]` block tells a reviewer exactly which headers carry credentials, which
is the same list the run records by header name. A dropped `${...}` wrapper is caught at
declaration time with the key named, rather than being sent to a vendor as a credential-shaped
string.

The accepted cost: a vendor wanting a constant header beside a credential header needs both
paths, one in the block and one in guest code. The two headers on one request are declared in
two artifacts, and a connector author has to know which belongs where. For a source whose only
non-credential header is an API version, that split is pure friction with no security content.

A smaller cost: the guest-side path is the only one for constants, so a constant header cannot
be changed by an operator editing a declaration — it moves with the connector version.

## Revisit triggers

- The block gains a way to mark an entry as a constant explicitly, at which point legibility
  is preserved without the refusal.
- Connector authors are observed duplicating credential headers into guest code to keep a
  source's headers in one artifact, indicating the split pushes work in the wrong direction.
- A vendor requires a constant header the host must override — one a guest may not be trusted
  to set — making guest-side construction the wrong home for it.
