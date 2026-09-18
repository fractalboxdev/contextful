# 0164 — A link engine reads UTF-8 and refuses a document that is anything else

**Status:** accepted 2026-09-18
**Decides:** `derive.fetch.refusal.declared-charset`, `derive.fetch.refusal.invalid-bytes`

## Context

A `link_preview` unit lands the document's head facts — title, description, site name — into
columns a reader reaches directly. `link_description` lands in the body column, so it retrieves
like any other text in the store and can be returned as a passage with a citation. These are not
diagnostic strings; they are content, and a reader encountering one has no way to tell how it was
decoded.

That makes lossy decoding a content defect rather than a display defect. A document in a legacy
encoding, read as UTF-8 with invalid sequences replaced, produces a title containing replacement
characters. Nothing downstream marks it as a decoding artifact. It lands in a title column, is
indexed, is retrieved, and reads as a fact about the publication — as though the publisher titled
their article that way.

Declaration is unreliable in both directions. Character set declaration is opt-in: a publisher may
omit it from the content type header and from the document entirely, and many do. It can also
arrive late — a document whose first several hundred bytes are ASCII markup and whose charset
element appears after them passes any check that reads only the header, and the scanner reads a
bounded prefix by construction, so "inside the scanned prefix" is the honest scope of a document
check.

The byte bound interacts with validation. The scanner reads a document prefix bounded at 1 MiB by
default and drops the remainder. A read truncated at a byte offset ends mid-character whenever the
document's last characters are multi-byte, which is a property of the truncation and says nothing
about the document's encoding.

## Decision

A character set other than UTF-8 declared in the content type header, or in a charset element
inside the scanned prefix, raises `DeriveCharsetUnsupported` naming the declared value. A document
declaring nothing and failing UTF-8 validation raises `DeriveBytesNotUtf8`. A character split at
the byte bound is tolerated, since a truncated read ends mid-character by construction. Both
refusals land the unit failed and settled.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse a declared non-UTF-8 set and refuse undeclared bytes that fail validation** *(chosen)* | No wrong value lands in a column a reader trusts; the marker names the declared set | Non-UTF-8 publications are unreadable by this task |
| Decode lossily, substituting replacement characters | Every document produces a row; no coverage gap | Loses on exactly the deciding criterion: replacement characters in a title column read as a fact about the publication, and nothing distinguishes them from a publisher's own text |
| Trust the declared character set alone and skip byte validation | One check, no scanning cost, honours the publisher's statement | Loses on coverage: declaring is opt-in, and a publisher declaring a legacy set after several hundred bytes of ASCII passes a header check |
| Transcode from the declared set into UTF-8 | Full coverage of the declared population, correct text | Loses on effort and on trust: a transcoding table per legacy set, and a mis-declared document transcodes into confident nonsense rather than a refusal |
| Detect the encoding statistically and transcode | Covers the undeclared population too | Loses on the deciding criterion again: a detector's confidence is not a guarantee, and its wrong answers are silent and land as content |

## Criteria

1. **Whether a wrong value can land in a column a reader trusts.** Measured by asking what a
   consumer of `link_title` would believe.
2. **Coverage** — how much of the publisher population produces a usable row.
3. **Legibility of a refusal** — whether the marker says what was wrong with the document.
4. **Implementation surface** — how much encoding machinery the engine carries.

Criterion 1 decides alone. Coverage is the only criterion that argues the other way, and it argues
for producing rows whose defect is undetectable downstream; a missing row is a visible absence the
anti-join counts, while a corrupted title is an invisible falsehood. The refusals were split in
two — declared and undeclared — so criterion 3 is satisfied in the case where the publisher told
us something, which is also the case an operator can act on.

## Consequences

Easier: every string a link row carries is valid UTF-8 with no decoding provenance to track. The
write guard, the redaction pass and the retrieval path all operate on one text representation.

Harder: a deployment reading a non-Latin publisher population loses those documents entirely, and
the loss is recorded as settled markers rather than as anything that suggests an encoding fix. An
operator diagnosing it reads `DeriveCharsetUnsupported` and the declared value, which is the right
evidence but arrives one publisher at a time.

Accepted cost: non-UTF-8 publications are unreadable by this task. This is a real coverage gap
outside the Latin web, and its size is unmeasured — it depends entirely on the link population a
deployment scans, and no figure here would be anything but invented.

Expensive to reverse: the refusals settle the unit, so units already marked are not revisited if
transcoding is added later. Introducing decoding would recover the future population and not the
past one, absent a re-derive signal.

## Revisit triggers

- `DeriveCharsetUnsupported` markers accumulate against a declared set that is stable and
  well-specified, making transcoding for that one set a bounded change rather than open-ended
  machinery.
- A deployment's target publisher population is predominantly outside UTF-8, which inverts the
  coverage criterion's weight.
- A re-derive signal for settled units is decided, which would make adding decoding retroactively
  useful and change what this record trades away.
