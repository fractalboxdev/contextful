# 0226 — Clearing a table's redistribution flag refuses the push and names the objects an earlier push left behind

**Status:** accepted 2026-09-18
**Decides:** `enforcement.bound-redistribution.refusal.objects-left-behind`

## Context

The redistribution flag is per table and is applied at push: cleared, it holds back every
object of that table and keeps the table's name out of the manifest written to the bucket. That
is a forward statement. It says nothing about objects an earlier push, made while the flag was
set, already uploaded.

Those objects are still there, and still fetchable. A manifest indexes objects and authorizes
nothing — reachability in the bucket is by key. Dropping a table's name from the manifest
removes it from the index and leaves every previously-uploaded key exactly as reachable as it
was. The same applies to the verbatim record each pull writes, which is the second envelope
rows travel in and is keyed the same way.

So an operator who clears the flag has two beliefs available. One is that the bound now applies
to the table, including what was already sent. The other is that the bound applies from here.
Only the second is true, and nothing in the mechanism distinguishes them at the moment the flag
is cleared.

What the engine can do about it is constrained. It is the writer of those objects, not the
authority over the bucket's retention. Deleting them is irreversible, may violate a retention
policy the engine does not know about, and may remove objects a replica is presently serving
from. The engine's honest position is that it knows precisely which keys are implicated and can
say so.

## Decision

Clearing the flag over a table an earlier unrestricted push uploaded raises
`EnforceStaleRedistributedObjects`, naming each object and the remedy, and removing none of
them. The refusal persists while any uploaded record may carry that table's rows. The
left-behind report names at most 100 files and states the total it found. The check runs twice
per push, once ahead of the upload loop and once after it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the push, name each implicated object and its remedy, remove nothing** *(chosen)* | The operator learns the exact truth — which keys remain fetchable — at the moment the belief would otherwise form, and the engine deletes nothing it is not the authority over. | The bound cannot be applied retroactively without an operator cleaning the bucket by hand, and the refusal persists while any uploaded record may carry that table's rows. |
| Delete the objects automatically | The flag means what the operator assumed; no manual step. | Loses on authority over the bucket: the engine is not the arbiter of retention, and the deletion is irreversible and may cut a replica out from under its readers. |
| Proceed and stop naming the table in the manifest | The simplest behavior, and the manifest reads as though the bound applied. | Loses on whether the operator learns the truth: silence leaves them believing the bound reached backwards, which is precisely the wrong belief to hold about a licence bound. |
| Rewrite the manifest and treat the objects as unreachable | No refusal, no deletion, and the index tells a consistent story. | Loses on reachability: an object is fetchable by key, not by index, so the story is false for anyone holding a key. |

## Criteria

1. **Whether the operator learns the truth about what remains fetchable** — or forms the
   retroactive belief. Silent proceeding and manifest rewriting both fail this.
2. **Whether the engine acts as authority over data it did not create the policy for** —
   automatic deletion fails this, and fails irreversibly.
3. **Whether the claim made matches the reachability model** — index versus key. The rewrite
   option fails this.
4. **Operator effort to actually apply the bound** — how much manual work stands between
   clearing the flag and the bound holding over old objects. This is the criterion the chosen
   option loses on.

Whether the operator learns the truth decides it. A licence bound exists to be relied on when
someone asks where a table's rows went, and the answer "the flag is clear" is only usable if
clearing the flag never silently means "from now on". Automatic deletion would also produce a
true belief, but it buys that truth by taking an irreversible action in a bucket whose retention
rules the engine cannot see.

## Consequences

Clearing the flag is a decision with a visible consequence rather than a setting with a quiet
one, and the report hands the operator the exact key list the remedy applies to. Running the
check on both sides of the upload loop narrows the window against a second writer active under
a different configuration; it does not close it, and the guarantee rests on a deployment holding
one bucket and one pushing configuration rather than on arbitration between writers that
disagree.

The accepted cost: the bound cannot be applied to a table after the fact without an operator
cleaning the bucket by hand, and until they do, pushes for the whole store refuse. That is a
blocking failure for a change an operator may have made in a hurry, and the remedy is manual
work in a system the engine does not manage.

Reversing this toward automatic deletion is technically small and permanent in effect: the
first push after the change removes objects, and nothing brings them back.

## Revisit triggers

- The engine is granted an explicit, operator-declared deletion authority over the bucket with
  a stated retention policy, which would make automatic cleanup an instruction rather than an
  assumption.
- A deployment is found running two pushing configurations against one bucket, which the
  two-pass check does not arbitrate and the current guarantee assumes away.
- The report's 100-file bound is observed truncating so often that the total alone is what
  operators act on, which would argue for a different reporting shape.
