# 0286 — An unreadable schedule string drops one entry and arms the rest of the document

**Status:** accepted 2026-09-18
**Decides:** `control.arm.refusal.unreadable-schedule`

## Context

A control document carries every entry a deployment runs. Arming turns that document into the
armed set: each entry's schedule string is read under a grammar with two forms — an `every`
interval and a five-field cron expression — and an entry whose string the grammar cannot read
has no next fire to compute.

The entries in one document are unrelated to each other by design. The armed set carries no
edge between two entries and no success-triggers-start orchestration; cross-entry ordering is
expressed through the model graph. So a defect in one entry's schedule string carries no
information about any other entry's correctness, and there is no ordering argument for holding
the rest back.

The document is also edited by hand. A typo in a schedule string is the single most likely
authoring defect in the whole control surface, and it arrives through the same apply as
whatever change the operator actually intended.

Whatever arming does with the bad string, one of two failure shapes follows. Either the
failure is wide — everything stops — or it is narrow, and the narrow one hides: an entry that
silently does not arm produces no error at run time, because an entry that never fires has no
run to fail. It reads exactly like an upstream with nothing new.

## Decision

A schedule string the grammar cannot read raises `ScheduleUnreadable` against that one entry,
naming the diagnostic, and every other entry in the same document arms. The refused entry
joins no armed set and fires nothing until its string is corrected. Validation is per entry:
an entry revalidates against the manifest guardrails before it joins the armed set, and a
guardrail refusal holds back that entry alone.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the one entry, arm the rest** *(chosen)* | One typo costs one entry. Every unrelated schedule in the document keeps running through an edit that touched something else. | An entry can sit unarmed indefinitely with only a diagnostic saying so, while every sibling reports healthy. |
| Abort the whole reconcile on one bad string | The defect is unmissable: the deployment stops, somebody looks. | One typo takes down every unrelated schedule in the document, including ingestion that has nothing to do with the edit. The blast radius of an authoring slip becomes the whole deployment. |
| Arm the entry on a default cadence | Nothing is silently absent; the entry runs. | The deployment then runs a schedule nobody wrote, at a rate nobody chose, and the output looks correct. A wrong cadence is harder to notice than a missing one. |
| Keep the entry's previous arming and ignore the new string | Continuity: the entry keeps running as it did before the bad edit. | The armed set and the document disagree, which is the one invariant the reconciler exists to maintain. A later reader of the document cannot tell what is running. |
| Refuse at apply time, so the document never reaches arming | The defect is caught before it can affect anything, at the surface where a human is present. | Arming also happens from a local manifest with no apply in front of it, so the check would exist in one path and not the other, and the weaker path is the one that runs unattended. |

## Criteria

1. **Blast radius of one authoring defect** — how much unrelated work a typo stops. The
   whole-document abort fails this.
2. **Visibility of the failure** — whether an operator learns that an entry is not running.
   The per-entry drop is weakest here; the default cadence is weaker still, since it produces
   confident wrong output rather than nothing.
3. **Agreement between the document and the armed set** — whether what is running is what was
   written. Keeping the previous arming fails this.
4. **Coverage of every arming path** — whether the rule holds when arming happens with no
   apply in front of it.
5. **Correctness of what runs** — whether any entry ever runs on a cadence nobody authored.
   The default cadence fails this.

Blast radius decides it. A control document carries every entry a deployment runs, so the
whole-document abort converts the most common authoring mistake into the widest possible
outage, and it does so at exactly the moment an operator is making an unrelated change. The
visibility cost is real and is paid; it is bounded by a diagnostic and by the status surface,
whereas the abort's cost is bounded by nothing.

## Consequences

An edit to one entry cannot stop another, so an operator can change a document under load
without the change's correctness being a precondition for everything else continuing. The
diagnostic names the entry and the reason, so the fix is one string.

The accepted cost is a silent absence. A refused entry produces no runs, and no runs produce
no failures, so the deployment's health surface reads clean while one pipeline has not
executed since the edit. Nothing in the fire path will ever raise this — an entry that is not
armed cannot fail — which means the only signal is the diagnostic at arm time and whatever
surface reports the armed set against the document.

Reversing this is cheap in either direction, since no state is carried: a later version could
abort the whole reconcile and the next poll would apply it.

## Revisit triggers

- The status surface gains a per-entry armed-versus-declared comparison that makes a refused
  entry as visible as a failing one, which would remove the cost this decision accepts.
- Entries acquire declared dependencies on each other, at which point a refused entry can
  invalidate its dependents and the per-entry bound stops being sound.
- Refused entries are observed sitting unarmed for long periods in real deployments, which
  would mean the diagnostic is not reaching anybody.
