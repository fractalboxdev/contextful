# 0223 — An out-of-scope read on a granted table returns a typed refusal rather than an empty result

**Status:** accepted 2026-09-18
**Decides:** `enforcement.refuse.refusal.out-of-scope-read`

## Context

A tenant-scoped grant compiles a bound-parameter byte equality on the tenant-scope column,
conjoined ahead of any table-level policy. A caller holding such a grant and asking for another
tenant's rows is asking a question the engine can answer precisely: the table is granted, the
scope is known, the requested scope differs. Both values are in hand at the moment the
statement is admitted.

The tenant equality alone produces an empty result for that read, and an empty result is
ambiguous in a way that matters. It is the same answer a caller gets from a correctly-scoped
table that happens to hold no rows, from a tenant that has never written, and from a scope
value that drifted somewhere upstream and now matches no partition. Those are different
situations with different remedies, and a caller who cannot tell them apart will read the empty
result as data.

That ambiguity converts a configuration mistake into a silent wrong answer. An agent
provisioned with the wrong tenant scope queries a populated table, receives nothing, and
reports that there is nothing — confidently, and with no signal anywhere that a boundary was
crossed. Nobody investigates an empty table.

The countervailing concern is disclosure. Any answer more specific than silence says something
about the other side of the boundary, and the refusal has to be built so that what it says is
bounded to what the caller already holds.

## Decision

A credential scoped to one tenant reading another tenant's rows on a granted table raises
`EnforceScopeDenied`, carried as wire code `scope_denied`, HTTP 403, and an in-band tool error
on the tool protocol, naming the table, the granted scope and the requested one. That case
produces the typed error and never an empty result set. The requested value in the error echoes
what the statement asked for, never a value read from storage.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A typed refusal naming the table, the granted scope and the requested one** *(chosen)* | A boundary is distinguishable from a quiet table, so a misconfigured scope fails loudly instead of returning a confident nothing. | The refusal confirms the named table exists and that the caller's scope differs from the requested one. |
| Return an empty result | Discloses nothing at all; the tenant equality already produces it with no extra machinery. | Loses on distinguishability: a scope error is indistinguishable from absent data, and the caller reports the empty answer as the truth. |
| Return the rows the caller's own scope holds | Never fails, always useful, no error to handle. | Loses on correctness of the answer: it responds to a question nobody asked, and the caller cannot tell it was rewritten. |
| Echo the closest stored value in the refusal | The most diagnosable error of the four. | Loses on disclosure: the echoed value comes from another tenant's rows, which is exactly what the boundary withholds. |

## Criteria

1. **Whether the caller can distinguish a boundary from a quiet table** — the difference
   between a failed read and a true empty answer. This is the criterion that decided it.
2. **What the answer discloses about the other side** — how much a refusal says about data the
   caller may not see. The echoed-value option fails this.
3. **Fidelity of a successful answer** — whether rows returned are the rows asked for. The
   rewrite option fails this.
4. **Existence disclosure** — whether the response confirms the table is there. This is the
   criterion the chosen option loses on.

Distinguishability decides it. Disclosure is the real competing concern, and it is answered
rather than overridden: the refusal carries only the table the caller was granted, the scope
the caller's own credential states, and the value the caller's own statement asked for. All
three are already in the caller's hands. A silent empty result, by contrast, has a failure mode
with no upper bound — every downstream consumer of that answer inherits the mistake.

## Consequences

A scope misconfiguration surfaces as a refusal at the first read rather than as a series of
plausible empty answers, and the error carries enough to name the fix without a round trip to
whoever minted the credential. The refusal appears identically across the wire code, the HTTP
status and the tool protocol, so a caller on any surface handles one case.

The accepted cost: the refusal confirms that the named table exists and that the caller's own
scope differs from the requested one. That is information a grant already carries — the caller
was granted this table by name — so the refusal widens nothing, but it does mean the two
refusal shapes disclose different things, and which shape a caller receives is itself a signal.
An ungranted table therefore answers differently, so that holding one grant never becomes a way
to enumerate what lies beyond it.

Reversing this toward an empty result is trivially cheap and silently harmful: every caller
already written to handle the refusal would start receiving data-shaped answers for boundary
crossings.

## Revisit triggers

- A deployment appears where the existence of a granted table is itself sensitive to the
  grantee, which would make the confirmation a real disclosure rather than a restatement.
- The scope value is observed drifting upstream often enough that refusals fire for reasons the
  caller cannot act on, which would argue for a distinct signal for drift.
- A caller surface is added that cannot carry a typed refusal, forcing the boundary to be
  expressed some other way.
