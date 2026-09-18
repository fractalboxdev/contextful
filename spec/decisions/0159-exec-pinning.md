# 0159 — A path-form command carries a content digest and a bare name on the search path does not

**Status:** accepted 2026-09-18
**Decides:** `derive.exec.refusal.missing-binary`, `derive.exec.refusal.unpinned-path`, `derive.exec.refusal.digest-mismatch`

## Context

Every row an `exec` engine produces carries `derive_engine`, and that value is a hash over each
step's resolved-binary digest together with the arguments that invoked it. The claim the column
makes is that two rows sharing a value came out of the same thing. That claim holds exactly as
far as the digests behind it are real: a step whose binary is identified by name alone contributes
a name, and a name is stable across a silent replacement of the file it points at.

The commands an operator writes fall into two populations with different lifecycles. A bare name
on the search path is a system tool — a transcoder, a downloader — installed and upgraded by a
package manager on a cadence nobody in this system controls. A path-form command is something the
operator placed deliberately: a script under a tools directory, a vendor binary unpacked into a
known location. The second population is also the one an agent edits. A project script is the
most editable surface on the machine, rewritten in place, with the manifest unchanged and nothing
in the configuration to show that the thing being run is now different.

The operator writes the command. An absent binary is therefore not an environmental surprise to
be worked around but a declared intent the machine cannot honour, and resolving it silently to
something else — a fallback name, a second search path — would substitute a binary the operator
never named while leaving the engine identifier claiming otherwise.

## Decision

`command[0]` resolves once at build: an explicit path as written, a bare name through the search
path. The resolved file is checked for executability, canonicalized and digested. A `command[0]`
that is not an executable file on this machine raises `DeriveBinaryMissing` naming the binary and
the step. A `command[0]` written as a path carries `sha256`; an unpinned one raises
`DeriveUnpinnedPath` and quotes the digest just computed, so pinning is a paste. A pinned file
whose bytes differ from its recorded digest raises `DeriveDigestMismatch`. A bare name on the
search path carries no digest requirement.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Pin path-form commands, leave bare names unpinned** *(chosen)* | The agent-editable surface cannot change without a reviewable configuration edit; system tools stay upgradable | A bare-name binary changes underneath the engine with no configuration change |
| Require a digest on every step | Complete provenance: the engine identifier is a full statement about the bytes that ran | Loses on runnability — every package-manager upgrade refuses every derive pipeline on the machine until someone re-pins, so the tier is unusable by anyone tracking security updates |
| Require none | Nothing to maintain; bindings are short | Loses on what the operator's own spelling declares: a project script is rewritten in place with nothing in the manifest to show for it, while the engine identifier still claims sameness |
| Document the convention, do not enforce it | Same guarantee on paper, no refusals | Loses outright — an unenforced pin is documentation, and the one case it exists for is the one where nobody looked |
| Pin every step but warn rather than refuse on mismatch | Upgrade-tolerant, full coverage | A warning in a scheduled run reaches nobody; the failure mode is identical to not pinning |

## Criteria

1. **What the spelling already declares** — whether the rule asks the operator for anything beyond
   the distinction they drew by writing a path rather than a name.
2. **Runnability** — whether a machine kept current with ordinary upgrades can still run a
   pipeline without configuration work.
3. **Review locality** — whether a change to what runs shows up in the same reviewable change as
   the edit that caused it.
4. **Provenance completeness** — how much of the running chain the engine identifier truly
   accounts for.

Runnability decided it where several pulled apart. Criterion 4 argues for universal pinning and
criterion 1 argues the operator already sorted the two cases; between them, criterion 2 is the one
that determines whether the feature exists at all, because a rule that turns every routine
security upgrade into a fleet-wide pipeline outage is not adopted, and an unadopted rule protects
nothing. Criterion 3 is what makes the chosen rule bearable: the digest is quoted in the refusal,
so re-pinning after an intentional script edit is part of the edit.

## Consequences

Easier: an agent or a person rewriting a pinned script gets a refusal in the same change, and the
fix is copying a digest the refusal already printed. An engine identifier changing between two
batches is a fact an operator can act on rather than a mystery.

Harder: a binding that moves a tool from the search path into a pinned directory acquires a
maintenance obligation that did not exist before. Two machines running the same manifest with
differently-built system tools produce rows with different engine identifiers, which is honest but
makes cross-machine comparison require reading the bindings.

Accepted cost: an unpinned system binary can change underneath the engine between two runs with
no configuration change. The only signal is a new engine value on the rows produced after it,
which is a signal a reader has to go looking for rather than one that stops anything.

Expensive to reverse: rows already landed carry engine identifiers computed under this rule.
Widening pinning later changes every identifier at once, so the population of rows before and
after the change cannot be compared on that column even where nothing about the tools changed.

## Revisit triggers

- Engine identifiers churn across runs with no configuration change often enough that the column
  stops discriminating, observed as distinct values per run for an unchanged binding.
- A package manager on a target platform exposes a stable content digest per installed file,
  removing the runnability objection to pinning bare names.
- A defect is traced to a silently-upgraded system tool, which is the accepted cost arriving.
