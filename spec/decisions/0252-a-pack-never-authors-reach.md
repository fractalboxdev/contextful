# 0252 — A pack declares a projection and asserts no audience

**Status:** accepted 2026-09-18
**Decides:** `visibility.pack.refusal.mapping-absent`, `visibility.pack.refusal.pack-asserts-access`

## Context

A pack is the reviewed unit that brings one source into a deployment: a manifest fragment
plus a connector pin, declaring content tables with their cursors and transforms, the
access mapping, fidelity and budget defaults per table, the sweep jobs with their
cadences, and the surface templates. It carries configuration read as a diff and carries
no executable logic of its own.

Everything a pack says about authorization is a projection: which column names the
governed object, what the source's grain is called, how source principals map onto
`principal_kind`, how source roles map onto `level`, and which family the source belongs
to. Who reaches what is then decided by a join over rows a sweep observed. A pack author
is describing a translation; they are not in a position to know any organization's
audience, because they are writing against a source rather than against a deployment.

Two ways of getting this wrong are cheap and inviting.

The first is landing a table with no mapping at all. Adding a source is mostly about
content tables, cursors and transforms; the access mapping is extra work, and a table that
lands without one is a table whose governing regime is whatever the absence defaults to. On
an organization-wide face there is no defensible default, since the alternatives are a
permissive one, which discloses, or a silent exclusion, which looks like a broken
integration.

The second is a field that sounds like authorization. A manifest is deliberately tolerant
of unknown keys — fields outside the access mapping are inputs to the fetch step — so a
pack author writing `audience = "team"` or `team_visible = true` gets no complaint. Nothing
at request time reads such a field, because no read-path step consults a pack author's
claim; the join reads the access tables and nothing else. The field is enforcement-shaped
text that enforces nothing, and everyone downstream who reads the manifest — the operator
reviewing the diff, the next author copying the pack — takes it for a control. It can only
drift from the decision the join makes, and the drift is silent in both directions.

## Decision

Every table a pack lands carries an access mapping or the `excluded` declaration; there is
no third state and no permissive default. A pack landing a table with neither raises
`VisibilityMappingAbsent` at diagnose, naming the pack and the table. A mapping states how
a permission model projects onto the access tables and states no audience: no field within
it carries the sense of visible to the team or visible to everyone. An access-shaped key
the mapping schema does not define raises `VisibilityPackAssertsAccess` by name, rather
than being tolerated the way an unknown key elsewhere in a manifest is. Fields outside the
access mapping are inputs to the fetch step and reach no authorization decision.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Mapping or exclusion on every landed table, and access-shaped keys refused by name** *(chosen)* | Authorization has one source — the join over observed rows — and a pack's manifest contains nothing that reads as a control while controlling nothing. | Adding a source with no mirrorable access list requires a federated leg or an explicit exclusion; there is no quick path onto an organization-wide surface. |
| An audience or team-visible field for sources with no mirrorable access list | A source lands immediately, with an intent stated in the place a reviewer reads, and no federation to build. | Loses on enforceability: no read-path step consults it, so it is enforcement-shaped text that enforces nothing, and a reviewer approving the diff approves a control that does not exist. |
| Silently ignore unknown access-shaped keys, as with any other unknown key | Consistent manifest semantics, no schema of forbidden spellings to maintain, and tolerance for pack evolution. | Loses on the same criterion: manifest parsing is deliberately tolerant, so silence would let the field look effective indefinitely, and its only possible relationship to the join's decision is drift. |
| A permissive default for an unmapped table | Adding a source is one step shorter, and the common case of a broadly readable corpus works immediately. | Loses on enforceability's other side: the convenient path becomes the unbound one, so the tables that land without attention are the tables that serve without restriction. |
| Silently exclude an unmapped table | Fails closed, with no refusal to handle and no diagnose step. | Loses on where the failure surfaces: a silently excluded table reads as a broken connector, so the author debugs the fetch path rather than writing the mapping that is missing. |
| Warn on an access-shaped key rather than refusing | The pack still loads; the disagreement is recorded for whoever reads the log. | Loses on enforceability once more: a warning read once at load does not stop the field from being copied forward into the next pack as though it worked. |

## Criteria

1. **Whether an assertion can be enforced** — whether anything at request time consults
   what the pack says. *This criterion decides.* A configuration field that no code path
   reads is worse than an absent one, because it converts a reviewer's attention into
   false assurance and propagates by being copied. Convenience of landing a source is a
   real cost this decision spends, and it is recoverable by building the federated leg;
   an unenforceable control is not recoverable, because nobody knows it is not working.
2. **Where the failure surfaces, and to whom** — at diagnose, naming the pack and the
   table, versus at a reader or in a log.
3. **Speed of landing a new source** — the cost this decision accepts.
4. **Reviewability of the diff** — whether an operator reading a pack's manifest can tell
   what governs each table.

## Consequences

An operator reviewing a pack diff sees, per table, either a projection they can check
against the source's documentation or an explicit exclusion. Nothing in between exists,
and nothing in the manifest describes an audience, so the review question is always the
same one: is this translation right.

The accepted cost is friction exactly where enthusiasm is highest. A source with no
mirrorable access list cannot be made useful on an organization-wide face by writing a
line of configuration; it needs a federated leg built, or an exclusion accepted, and both
of those are slower than the path this decision closes. Packs for such sources will sit
unfinished, and the pressure to reopen the audience field will recur each time.

Refusing unknown access-shaped keys means maintaining a notion of which spellings are
access-shaped, inside a parser that is otherwise tolerant — a small ongoing inconsistency
accepted deliberately.

## Revisit triggers

- A pack-supplied declaration is proposed that a read-path step genuinely consults, which
  would make it an enforceable control rather than an assertion.
- The set of spellings treated as access-shaped is observed refusing legitimate fetch
  inputs, which would make the heuristic's breadth a cost rather than a safeguard.
- Building a federated leg becomes cheap enough that the friction this decision accepts
  stops determining whether a source gets integrated.
