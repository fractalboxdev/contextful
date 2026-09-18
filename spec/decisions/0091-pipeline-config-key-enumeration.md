# 0091 — A source config key outside the enumerated set refuses before any I/O

**Status:** accepted 2026-09-18
**Decides:** `pipeline.declare.refusal.config-key`

## Context

A source is a connector name beside a free-form JSON config object. Free-form is what lets one
manifest parse identically from TOML and from JSON, and what lets a connector carry vendor
options the engine knows nothing about. It is also what makes a misspelled key indistinguishable
from a key the engine simply does not read.

The consequence of ignoring an unknown key is not a missing feature. It is a manifest that reads
as configured and runs on defaults. An author who writes `page_size` where the source reads
`per_page` has a file that states a bound, a review that approved that bound, and a pipeline
paging at the default. The sharpest version is a key that names a limit: when the run eventually
fails on the vendor's rate limiter, the failure advertises the very setting the operator
believes they already wrote.

Timing matters as much as the check itself. A source's first act is usually to resolve a
credential and open a connection. A key check performed when the reader first consults the key
happens after the credential has moved and possibly after a request has left the host, which
turns a typo into an authenticated failed call.

Two config shapes are genuinely open. Request headers are keyed by the operator because the
header names belong to the vendor, and the forwarded guest table is keyed by identifiers from
outside the manifest. Neither can be enumerated by the connector without enumerating the
internet.

## Decision

A source config key outside the set that source enumerates raises `PipelineUnknownConfigKey`
ahead of any I/O, naming the key alongside the keys the source reads. The check holds at entry
depth, so a nested block and the entries inside it answer a typo alike. Request headers and the
forwarded guest table are operator-keyed and sit outside the check.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse an unenumerated key at parse, printing the keys the source reads** *(chosen)* | A manifest that reads as bound is bound; the error names the correct spelling, so the fix is mechanical. | Every source maintains an enumeration beside its reader, and the operator-keyed tables need an explicit carve-out that is itself a hole in the check. |
| Ignore unknown keys | Forward and backward compatibility for free; a manifest written for a newer connector loads against an older one. | Loses on silent divergence: the pipeline runs on defaults while the file states otherwise, and nothing surfaces until a downstream number is wrong. |
| Warn and continue | The information is emitted; nothing breaks. | Loses on the same criterion in practice — a warning in a scheduled run's log is not read, and the pipeline whose manifest most needs the warning is the one nobody is watching. |
| Refuse at first use of the key rather than at parse | No enumeration to maintain; the reader is the only place that knows its keys. | Loses on timing: the refusal arrives after a credential has already been resolved and moved, and a key that is read only on some paths is never checked at all. |
| Check only top-level keys | A simpler rule; nested vendor blocks stay open. | Loses on silent divergence at exactly the place authors make the mistake, since the interesting bounds live inside the nested blocks. |

## Criteria

1. **Whether a manifest that reads as bound can silently run on defaults.** *This is the
   criterion that decided it.* Every rejected option leaves that state reachable. The
   compatibility the ignoring option buys is real, but it is a convenience for upgrades, while
   silent divergence is a wrong answer in the data — and the wrong answer is discovered by
   noticing a number is off, not by reading a log.
2. **When the refusal arrives relative to credential movement and network I/O.**
3. **Actionability of the error** — whether the message contains the spelling the author meant.
   Printing the source's key set satisfies this.
4. **Maintenance burden on each connector** — one enumeration per source, kept in step with its
   reader. This is the criterion the chosen option loses on.

## Consequences

A typo is a parse-time failure with both spellings in front of the author, and no credential has
moved. Reviewing a manifest becomes meaningful: a key present in the file is a key the source
reads.

The cost accepted is duplication inside every connector — an enumeration beside the reader that
can drift from it, so a key the reader consults but the enumeration omits is refused wrongly.
That drift is a new failure mode this decision creates, and its frequency is unmeasured. The
carve-outs are the second cost: request headers and the forwarded guest table are open maps, so
a typo there behaves exactly as it would have under the ignoring option, and the check's
guarantee has two named holes in it rather than none.

## Revisit triggers

- The enumeration drifts from the reader in practice, refusing keys a source genuinely consults,
  which makes the duplication a real cost rather than a theoretical one.
- Connector config gains a derived schema, so the enumeration is generated from the reader's own
  type rather than written beside it, which removes the drift without changing the rule.
- The operator-keyed carve-outs are shown to be where the mistakes actually happen, which would
  argue for a narrower structure in those blocks rather than an exemption.
