import Protocol.Journal
import Std.Data.HashSet

/-!
# The journal model's bounded check and replay

`journalSearch` evaluates the journal invariants on every state the step function reaches,
visiting each distinct state once in breadth-first order. `journalRun` reads one step per
line (`begin 0 1`, `advance 2`, `effect 0 blob`, `sweep aged`, …) and prints the state after
each step in the projection the Rust interleaving prints.
-/

namespace Protocol.Journal

def allCallers : List Caller := List.finRange 3
def allKeys : List Key := List.finRange 2

/-- The keys the bounded check ranges over: one, so three callers race on it. -/
def checkKeys : List Key := allKeys.take 1

/-- Every step over callers `cs` and keys `ks`. -/
def stepsOver (cs : List Caller) (ks : List Key) : List Step :=
  [.sweep true, .sweep false] ++ ks.map .retire ++ cs.flatMap fun c =>
    ks.map (.begin c) ++
    [.advance c, .effect c .inline, .effect c .blob, .effect c .fail, .effect c .unjournaled,
     .note c, .lapse c, .crash c, .finish c]

/-- A state as a finite value. -/
structure Snapshot where
  rows : List (Option Row)
  blobs : List Bool
  callers : List CallerState
  records : List Nat
  deriving BEq, Hashable

def Snapshot.of (s : State) : Snapshot :=
  ⟨allKeys.map s.rows, allCallers.map s.blobs, allCallers.map s.callers, allKeys.map s.records⟩

def Snapshot.toState (x : Snapshot) : State :=
  ⟨fun k => x.rows.getD k.val none, fun c => x.blobs.getD c.val false,
   fun c => x.callers.getD c.val ⟨.idle, false⟩, fun k => x.records.getD k.val 0⟩

/-- The invariant a state or a transition breaks, if any: the properties the journal
theorems prove over every reachable state. Without `stored`, a recorded row naming a missing
blob passes, so the search runs on to the `BlobMissing` a later reader meets. -/
def broken (stored : Bool) (stepFn : State → Step → State) (s : State) (st : Step) : Option String :=
  let s' := stepFn s st
  if allKeys.any (fun k => match s.rows k with
      | some (.recorded v) => s'.rows k != some (.recorded v) && st != .retire k
      | _ => false) then some "a recorded value stays recorded"
  else if allKeys.any (fun k => s'.records k > 1) then some "at most one record lands per key"
  else if stored && allKeys.any (fun k => match s'.rows k with
      | some (.recorded v) => v.blob && !s'.blobs v.origin
      | _ => false) then some "no recorded row names a missing blob"
  else if allCallers.any (fun c => match (s'.callers c).phase with
      | .done k (.replayed v) | .done k (.recorded v) | .loading k v => s'.rows k != some (.recorded v)
      | _ => false) then some "every journaled resolution is the recorded value"
  else if allCallers.any (fun c => match (s'.callers c).phase with
      | .claimed k => !(match s'.rows k with
          | some (.pending h _) => h == c
          | some (.recorded _) => true
          | none => false) && !(s'.callers c).lapsed
      | _ => false) then some "an effect runs only under a claim or a lapsed lease"
  else if allCallers.any (fun c => match (s'.callers c).phase with
      | .done _ .missing => true
      | _ => false) then some "no caller reads BlobMissing"
  else none

def Value.show (v : Value) : String := s!"{if v.blob then "b" else "i"}{v.origin.val}"

def Phase.show : Phase → String
  | .idle => "idle"
  | .claiming k => s!"claiming {k.val}"
  | .reading k => s!"reading {k.val}"
  | .takingOver k h => s!"taking-over {k.val} {h.val}"
  | .claimed k => s!"claimed {k.val}"
  | .checking k v => s!"checking {k.val} {v.show}"
  | .storing k v => s!"storing {k.val} {v.show}"
  | .recording k v => s!"recording {k.val} {v.show}"
  | .releasing k .failed => s!"releasing {k.val} failed"
  | .releasing k .unrecorded => s!"releasing {k.val} unrecorded"
  | .loading k v => s!"loading {k.val} {v.show}"
  | .done k (.replayed v) => s!"done {k.val} replayed {v.show}"
  | .done k (.recorded v) => s!"done {k.val} recorded {v.show}"
  | .done k .unrecorded => s!"done {k.val} unrecorded"
  | .done k .failed => s!"done {k.val} failed"
  | .done k .missing => s!"done {k.val} missing"

def showRow : Option Row → String
  | none => "none"
  | some (.pending h a) => s!"pending {h.val} {a}"
  | some (.recorded v) => s!"recorded {v.show}"

/-- The state in the projection the Rust interleaving prints. -/
def showState (s : State) : String :=
  let rows := " ".intercalate (allKeys.map fun k => s!"k{k.val}={showRow (s.rows k)}")
  let blobs := String.join (allCallers.map fun c => if s.blobs c then "1" else "0")
  let cs := " ".intercalate (allCallers.map fun c =>
    s!"c{c.val}({(s.callers c).phase.show} lapsed={(s.callers c).lapsed})")
  s!"rows[{rows}] blobs={blobs} {cs}"

def showOutcome : Outcome → String
  | .inline => "inline" | .blob => "blob" | .fail => "fail" | .unjournaled => "unjournaled"

def showStep : Step → String
  | .begin c k => s!"begin {c.val} {k.val}"
  | .advance c => s!"advance {c.val}"
  | .effect c o => s!"effect {c.val} {showOutcome o}"
  | .note c => s!"note {c.val}"
  | .lapse c => s!"lapse {c.val}"
  | .crash c => s!"crash {c.val}"
  | .finish c => s!"finish {c.val}"
  | .sweep aged => s!"sweep {if aged then "aged" else "fresh"}"
  | .retire k => s!"retire {k.val}"

/-- Breadth-first search over distinct states. Returns the number of states visited, or the
shortest step sequence ending in a breaking transition with the invariant it breaks. -/
def search (stored : Bool) (stepFn : State → Step → State) (steps : List Step) : Except (List Step × String) Nat := Id.run do
  let init := Snapshot.of State.init
  let mut seen : Std.HashSet Snapshot := Std.HashSet.emptyWithCapacity 65536 |>.insert init
  let mut states : Array (Snapshot × Nat × Option Step) := #[(init, 0, none)]
  let mut next := 0
  while h : next < states.size do
    let (x, _, _) := states[next]
    let s := x.toState
    for st in steps do
      if let some why := broken stored stepFn s st then
        let mut path := [st]
        let mut i := next
        while i != 0 do
          match states[i]? with
          | some (_, parent, via) =>
            path := via.toList ++ path
            i := parent
          | none => i := 0
        return .error (path, why)
      let y := Snapshot.of (stepFn s st)
      if !seen.contains y then
        seen := seen.insert y
        states := states.push (y, next, some st)
    next := next + 1
  return .ok states.size

def parseCaller (s : String) : Option Caller :=
  s.toNat?.bind fun n => if h : n < 3 then some ⟨n, h⟩ else none

def parseKey (s : String) : Option Key :=
  s.toNat?.bind fun n => if h : n < 2 then some ⟨n, h⟩ else none

def parseStep (line : String) : Option Step :=
  match (line.splitOn " ").filter (· ≠ "") with
  | ["begin", c, k] => do some (.begin (← parseCaller c) (← parseKey k))
  | ["advance", c] => (parseCaller c).map .advance
  | ["effect", c, "inline"] => (parseCaller c).map (.effect · .inline)
  | ["effect", c, "blob"] => (parseCaller c).map (.effect · .blob)
  | ["effect", c, "fail"] => (parseCaller c).map (.effect · .fail)
  | ["effect", c, "unjournaled"] => (parseCaller c).map (.effect · .unjournaled)
  | ["note", c] => (parseCaller c).map .note
  | ["lapse", c] => (parseCaller c).map .lapse
  | ["crash", c] => (parseCaller c).map .crash
  | ["finish", c] => (parseCaller c).map .finish
  | ["sweep", "aged"] => some (.sweep true)
  | ["sweep", "fresh"] => some (.sweep false)
  | ["retire", k] => (parseKey k).map .retire
  | _ => none

partial def runLoop (stepFn : State → Step → State) (stdin : IO.FS.Stream) (s : State) : IO UInt32 := do
  let line ← stdin.getLine
  if line.isEmpty then return 0
  let text := line.trimAscii.toString
  if text.isEmpty then return (← runLoop stepFn stdin s)
  match parseStep text with
  | none =>
    IO.eprintln s!"unrecognised step: {text}"
    return 2
  | some st =>
    let s' := stepFn s st
    IO.println (showState s')
    runLoop stepFn stdin s'

/-- `journal check [--no-grace]` and `journal run [--no-grace]`. `--no-grace` lets an aged
sweep take a blob between its `put` and its `record`, and the check prints the shortest
sequence reaching `BlobMissing`. -/
def main (args : List String) : IO UInt32 := do
  let grace := !args.contains "--no-grace"
  match args.filter (· ≠ "--no-grace") with
  | ["run"] => runLoop (stepWith grace) (← IO.getStdin) State.init
  | ["check"] =>
    match search grace (stepWith grace) (stepsOver allCallers checkKeys) with
    | .ok n =>
      IO.println s!"ok: 6 journal invariants hold on {n} states over {allCallers.length} callers and {checkKeys.length} key"
      return 0
    | .error (path, why) =>
      IO.eprintln s!"JournalInvariantViolated: {why}, after {path.length} steps"
      for st in path do IO.eprintln s!"  {showStep st}"
      return 1
  | _ =>
    IO.eprintln "usage: protocol journal [check | run] [--no-grace]"
    return 2

end Protocol.Journal
