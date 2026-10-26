import Protocol.Fold

/-!
# The fold model's replay

`protocol fold run` reads one step per line (`acquire`, `stage 0`, `commit 1`, `abandon 0`,
`collect`, `read 1`, `release 0`) and prints the state after each step: the pointer's
snapshot and fence, the chain, each snapshot id's directories (`f` published, `s` staging),
and the snapshot each reader resolved.
-/

namespace Protocol.Fold

def parseIndex (s : String) : Option (Fin 2) :=
  s.toNat?.bind fun n => if h : n < 2 then some ⟨n, h⟩ else none

def parseStep (line : String) : Option Step :=
  match (line.splitOn " ").filter (· ≠ "") with
  | ["acquire"] => some .acquire
  | ["collect"] => some .collect
  | ["stage", p] => (parseIndex p).map .stage
  | ["commit", p] => (parseIndex p).map .commit
  | ["abandon", p] => (parseIndex p).map .abandon
  | ["read", r] => (parseIndex r).map .read
  | ["release", r] => (parseIndex r).map .release
  | _ => none

def showOpt : Option Nat → String
  | none => "-"
  | some n => toString n

def showState (s : State) : String :=
  let chain := ",".intercalate (s.chain.map fun e => toString e.1)
  let dirs := " ".intercalate ((List.range s.next).map fun i =>
    let d := (if s.final i then "f" else "") ++ (if s.staging i then "s" else "")
    s!"{i}:{if d.isEmpty then "-" else d}")
  let readers := " ".intercalate ((List.finRange 2).map fun r => s!"r{r.val}={showOpt (s.readers r)}")
  s!"ptr={showOpt s.current} fence={showOpt (s.chain.head?.map Prod.snd)} chain=[{chain}] dirs=[{dirs}] {readers}"

partial def runLoop (stdin : IO.FS.Stream) (s : State) : IO UInt32 := do
  let line ← stdin.getLine
  if line.isEmpty then return 0
  let text := line.trimAscii.toString
  if text.isEmpty then return (← runLoop stdin s)
  match parseStep text with
  | none =>
    IO.eprintln s!"unrecognised step: {text}"
    return 2
  | some st =>
    let s' := step s st
    IO.println (showState s')
    runLoop stdin s'

/-- `fold run`. -/
def main (args : List String) : IO UInt32 := do
  match args with
  | ["run"] => runLoop (← IO.getStdin) State.init
  | _ =>
    IO.eprintln "usage: protocol fold run"
    return 2

end Protocol.Fold
