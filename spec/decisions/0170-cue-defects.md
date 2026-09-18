# 0170 — A malformed caption block is skipped and counted, and a backwards block is refused

**Status:** accepted 2026-09-18
**Decides:** `derive.parse-cues.refusal.backward-cue`

## Context

A caption document is written by a third party. It arrives from a publisher's feed, a
speech engine, a volunteer transcript or an editor that last ran a decade ago, and the
parser has no way to send it back. Real documents carry blocks whose timing line does
not parse, blocks whose end precedes their start, blocks whose text is empty after the
settings tail is stripped, and blocks that arrive out of order because two editing
passes were concatenated.

The parser's output is not free-standing text. Each passage lands with a citation key
derived from its start second, and that key is the address a reader is sent to. Two
passages computing the same key are two rows competing for one identity under the
table's declared key, and the later one wins by replacing the earlier.

That splits the defect population in two. A block the parser cannot read yields no
passage and takes nothing with it. A block whose start precedes the block already
accepted yields a perfectly well-formed passage carrying a key an earlier passage
already holds — it does not fail, it overwrites. The first kind of defect costs its own
content; the second costs someone else's.

## Decision

A block whose timing does not parse, whose interval inverts, or whose text is empty is
skipped and counted, and the count returns to the caller, which warns once when the
document is finished. A block starting before the block already accepted raises
`DeriveCueOutOfOrder` and is dropped. A document with defects yields a transcript;
the count is what says how partial it is.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Skip and count the unreadable, refuse the backwards block** *(chosen)* | Ninety good passages survive one bad block, and the one defect class that can destroy a good passage is the one that refuses | The count is the only signal of partiality, and nothing states which blocks were lost |
| Fail the whole document on any defect | One rule, and a transcript is either complete or absent | Lost on cost: a third party writes the document and one malformed block discards every passage in it, for a defect the operator cannot fix at the source |
| Skip every defect including the backwards block | Uniform treatment, no error identifier to carry | Lost on collision: a backwards block parses cleanly, so skipping is not what happens — it lands on a citation key a real passage already holds and silently replaces it |
| Keep the backwards block under a distinct key | No content is lost at all | Lost on citation integrity: the key is the address a reader is sent to, and a synthesized second key points at a moment the media does not have |
| Sort the document by start time before parsing | Recovers order without dropping anything | Lost on fidelity: rolling-overlap stripping reads contiguity between adjacent cues, and a sort fabricates adjacency between blocks the display never showed together |

## Criteria

1. **Cost of one defect** — how much good content a single bad block destroys.
2. **Citation integrity** — whether a landed passage's key addresses the moment its text
   was actually spoken.
3. **Silence** — whether a defect can consume a real passage without any signal.
4. **Uniformity** — how many rules the parser carries.

Criterion 3 decided the split. Criteria 1 and 2 point the same way for the unreadable
block, where skipping loses only what was already unreadable. They pull apart on the
backwards block, which is readable and whose retention is therefore free by criterion 1.
What settles it is that keeping it is not a smaller loss — it is the same loss, moved
onto a passage that parsed correctly, and moved out of the defect count that reports it.
A defect that consumes something outside itself, invisibly, refuses; a defect that
consumes only itself is counted.

## Consequences

A transcript from a rough document is usable, and the caller learns how rough it was in
one warning rather than one per block. The one path by which a defect could replace good
content is closed, so the citation key stays trustworthy without anyone inspecting the
source document.

The cost accepted is that a document with many defects yields a partial transcript, and
the count is the only signal of how partial. A reader is not told that the passage
between two others is missing, and a document that is ninety percent defective produces
a transcript that reads as complete to anyone who does not check the number.

Reversing toward failing the document is cheap. Reversing toward keeping backwards
blocks is not: it requires a key shape that tolerates two passages at one moment, which
reaches the citation key, the parent's link fragment and the union with feed-published
transcripts.

## Revisit triggers

- Documents in the corpus routinely interleave two correctly-ordered tracks, so a
  backwards block is a second speaker rather than a concatenation artifact.
- The defect count on real documents rises high enough that a per-block record of what
  was dropped is worth landing beside the passages.
- The citation key gains a component that distinguishes two passages at one start
  second, which would remove the collision that makes this a refusal.
