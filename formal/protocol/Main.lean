import Protocol
import Std.Data.HashSet

/-!
# The protocol executable

`protocol check` evaluates the four invariants on every state reached by every step sequence
over three nodes and four lease generations, visiting each distinct state once in
breadth-first order; a breaking state prints `ProtocolInvariantViolated` with the shortest
sequence reaching it and exits 1. `protocol check --unfenced` runs the same search over the
variant whose grant leaves the guarded objects' fences untouched.

`protocol run` reads one step per line on standard input (`acquire 0`, `send 1 catalog`,
`expire`, …) and prints the state after each step.
-/

open Protocol

/-- The nodes the bounded check ranges over. -/
def nodes : List Node := [0, 1, 2]

/-- The lease generations the bounded check ranges over: no state past fence 4 is expanded. -/
def generations : Nat := 4

/-- Every step over the three nodes. -/
def steps : List Step :=
  .expire :: nodes.flatMap fun n =>
    [.acquire n, .renew n, .release n, .send n .catalog, .send n .cursor, .deliver n,
     .pause n, .resume n, .crash n]

/-- A state as a finite value: steps over `nodes` touch no other node, so the three views
determine the state. -/
structure Snapshot where
  lease : Option Lease
  catalogFence : Nat
  cursorFence : Nat
  views : List NodeState
  deriving BEq, Hashable

def Snapshot.of (s : State) : Snapshot := ⟨s.lease, s.catalogFence, s.cursorFence, nodes.map s.node⟩

def Snapshot.toState (x : Snapshot) : State :=
  ⟨x.lease, x.catalogFence, x.cursorFence, fun n => x.views.getD n NodeState.idle⟩

/-- The invariant a state or a transition breaks, if any: the same four properties
`Protocol.Invariants` proves over every reachable state. -/
def broken (stepFn : State → Step → State) (s : State) (st : Step) : Option String :=
  let s' := stepFn s st
  let beliefs := nodes.filterMap fun n => (s'.node n).belief
  if beliefs.eraseDups.length != beliefs.length then some "one lease holder per fence"
  else if !(s.granted ≤ s'.granted && s.catalogFence ≤ s'.catalogFence && s.cursorFence ≤ s'.cursorFence) then
    some "fences only increase"
  else if (landed s st).any (fun (_, f) => f < s.granted) then
    some "no commit below the highest granted fence"
  else match st with
    | .release _ => if (s'.lease.map Lease.fence) != (s.lease.map Lease.fence) then
        some "release keeps the lease object and its fence" else none
    | _ => none

instance : ToString Target := ⟨fun | .catalog => "catalog" | .cursor => "cursor"⟩

def showStep : Step → String
  | .acquire n => s!"acquire {n}"
  | .renew n => s!"renew {n}"
  | .expire => "expire"
  | .release n => s!"release {n}"
  | .send n t => s!"send {n} {t}"
  | .deliver n => s!"deliver {n}"
  | .pause n => s!"pause {n}"
  | .resume n => s!"resume {n}"
  | .crash n => s!"crash {n}"

def showState (s : State) : String :=
  let lease := match s.lease with
    | none => "none"
    | some l => s!"holder={l.holder} fence={l.fence} live={l.live}"
  let views := nodes.map fun n =>
    let v := s.node n
    let pending := v.pending.map fun (t, f) => s!"{t}@{f}"
    s!"n{n}(belief={v.belief} paused={v.paused} pending={pending})"
  s!"lease[{lease}] catalog={s.catalogFence} cursor={s.cursorFence} " ++ " ".intercalate views

/-- Breadth-first search over distinct states. Returns the number of states visited, or the
shortest step sequence ending in a breaking transition with the invariant it breaks. -/
def search (stepFn : State → Step → State) : Except (List Step × String) Nat := Id.run do
  let init := Snapshot.of State.init
  let mut seen : Std.HashSet Snapshot := Std.HashSet.emptyWithCapacity 4096 |>.insert init
  -- Each visited state with its parent's index and the step reaching it.
  let mut states : Array (Snapshot × Nat × Option Step) := #[(init, 0, none)]
  let mut next := 0
  while h : next < states.size do
    let (x, _, _) := states[next]
    let s := x.toState
    for st in steps do
      let s' := stepFn s st
      if let some why := broken stepFn s st then
        let mut path := [st]
        let mut i := next
        while i != 0 do
          match states[i]? with
          | some (_, parent, via) =>
            path := via.toList ++ path
            i := parent
          | none => i := 0
        return .error (path, why)
      if s'.granted ≤ generations then
        let y := Snapshot.of s'
        if !seen.contains y then
          seen := seen.insert y
          states := states.push (y, next, some st)
    next := next + 1
  return .ok states.size

def parseNode (s : String) : Option Node := s.toNat?

def parseStep (line : String) : Option Step :=
  match (line.splitOn " ").filter (· ≠ "") with
  | ["expire"] => some .expire
  | ["acquire", n] => (parseNode n).map .acquire
  | ["renew", n] => (parseNode n).map .renew
  | ["release", n] => (parseNode n).map .release
  | ["send", n, "catalog"] => (parseNode n).map (.send · .catalog)
  | ["send", n, "cursor"] => (parseNode n).map (.send · .cursor)
  | ["deliver", n] => (parseNode n).map .deliver
  | ["pause", n] => (parseNode n).map .pause
  | ["resume", n] => (parseNode n).map .resume
  | ["crash", n] => (parseNode n).map .crash
  | _ => none

partial def runLoop (stdin : IO.FS.Stream) (s : State) (catalogEtag cursorEtag : Nat) : IO UInt32 := do
  let line ← stdin.getLine
  if line.isEmpty then return 0
  let text := line.trimAscii.toString
  if text.isEmpty then return (← runLoop stdin s catalogEtag cursorEtag)
  match parseStep text with
  | none =>
    IO.eprintln s!"unrecognised step: {text}"
    return 2
  | some st =>
    let s' := step s st
    let grant := if s'.granted > s.granted then 1 else 0
    let catalogWrite := match landed s st with
      | some (.catalog, _) => 1
      | _ => 0
    let cursorWrite := match landed s st with
      | some (.cursor, _) => 1
      | _ => 0
    let catalogEtag' := catalogEtag + grant + catalogWrite
    let cursorEtag' := cursorEtag + grant + cursorWrite
    IO.println s!"{showState s'} catalog-etag={catalogEtag'} cursor-etag={cursorEtag'}"
    runLoop stdin s' catalogEtag' cursorEtag'

def main (args : List String) : IO UInt32 := do
  match args with
  | ["run"] => runLoop (← IO.getStdin) State.init 0 0
  | [] | ["check"] | ["check", "--unfenced"] =>
    let stepFn := if args.contains "--unfenced" then stepWith false else step
    match search stepFn with
    | .ok n =>
      IO.println s!"ok: 4 invariants hold on {n} states over {nodes.length} nodes and {generations} lease generations"
      return 0
    | .error (path, why) =>
      IO.eprintln s!"ProtocolInvariantViolated: {why}, after {path.length} steps"
      for st in path do IO.eprintln s!"  {showStep st}"
      return 1
  | _ =>
    IO.eprintln "usage: protocol [check [--unfenced] | run]"
    return 2
