# 0156 — A step's command is an argument array and no shell is spawned

**Status:** accepted 2026-09-18
**Decides:** `derive.exec.refusal.shell-string`

## Context

An `exec` step runs a binary on the machine that holds the store, with unit data flowing into
its arguments. The media value substitutes into `{input}`; an address substitutes into
`{input_url}`. Both values came off a parent row, which came from a publisher, a mailbox, a
feed or an agent — parties this deployment does not control.

If the command were a single string handed to a shell, the shell would parse those values as
syntax. Word splitting turns a filename with a space into two arguments. Glob expansion turns
a bracket in a title into a pattern match against the scratch directory. Command substitution
turns a `$(...)` sequence anywhere in a publisher-supplied address into a second command,
running with the resolved credential environment the operator allowlisted for the first one.
None of these requires a hostile publisher; a filename with a space triggers the first on
ordinary data.

The operator's side of this is real too. A shell string is how people write media commands:
pipes between tools, redirection into a file, a variable reference. Taking the shell away
takes those away inside one step.

The apparent middle path is to keep the shell string and escape row data before it is
interpolated. That makes the escaping function a second parser that has to agree, in every
corner, with a shell it does not control and whose version and identity vary by machine.

## Decision

A step's `command` is an array of arguments and no shell is spawned: no word splitting, no
glob expansion, no command substitution. A media address carrying shell metacharacters reaches
the tool as one argument. A `command` given as a single string rather than an array raises
`DeriveShellCommand`.

`{input}`, `{input_url}`, `{output}` and `{output_stem}` substitute as whole argument elements,
and a placeholder embedded inside a longer argument is left untouched. `output_path` is the one
place substring substitution happens, and the values reaching it are paths the engine invented
inside a scratch directory it created, so no unit's data enters that template.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **An argument array, spawned directly** *(chosen)* | Row-supplied data can never become a second command; the boundary between data and syntax is structural rather than textual | An operator loses pipes and redirection inside one step and expresses them as separate chain steps or inside a script that is pinned |
| A shell string with quoting rules | The shape operators already know; pipes, redirection and variables work inside one step | Lost on data becoming syntax: word splitting, globbing and command substitution all read row data as command text, and an ordinary filename with a space triggers the first |
| Escaping row data into a shell string | Keeps the familiar shape while neutralizing the data | Lost on reliability: an escaping function is a second parser that has to agree with a shell it does not control, and one corner it gets wrong is an execution rather than a wrong argument |
| An argument array with a shell opt-in for trusted steps | Safe by default, expressive where the operator asks | Lost on where the danger is: the steps an operator most wants a shell for are the media steps, which are exactly the ones row data flows into |
| Requiring every step to be a script file the operator pins | Full shell inside the script, digest-pinned on the outside | Lost on effort for the common case: a one-line transcode becomes a file to write, pin and re-pin, and the array covers it directly |

## Criteria

1. **Whether row-supplied data can become a second command** — whether any value off a parent
   row can be read as syntax rather than as an argument. **This criterion decided it.** It is
   the only criterion here that distinguishes a wrong result from an execution: every other
   candidate cost is an operator writing more configuration.
2. **Number of parsers the correctness depends on** — whether safety rests on agreement with a
   shell whose identity varies by machine.
3. **Operator expressiveness inside one step** — pipes, redirection, variable references.
4. **Effort for the common case** — what a single transcode step costs to write.

## Consequences

Pipes and redirection do not exist inside a step. An operator who needs them writes the stages
as separate chain steps, passing files through `{output}` and `{output_stem}`, or puts the
pipeline inside a script file and pins it by digest. That is the cost accepted, and it lands on
every deployment that transcodes.

What gets easier: a media value containing quotes, spaces, brackets or dollar signs needs no
special handling anywhere. There is no escaping function to audit, and no machine-dependent
shell behavior in the failure surface.

What is now expensive to reverse: refusing a string `command` is a compatibility boundary.
Accepting one later would make a refused configuration start executing, on
machines whose operators wrote it expecting the refusal.

## Revisit triggers

- Chain steps prove insufficient for a media workflow that cannot be expressed without
  in-process composition between two tools.
- The pinning path for script files becomes cheap enough that a shell-bearing script is the
  ordinary way to write a step, making the array the exception.
