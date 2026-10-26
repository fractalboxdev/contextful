import Protocol
import Std.Data.HashSet

/-!
# The protocol executable

`protocol check` evaluates the five invariants on every state reached by every step sequence
over three nodes and four lease generations, on each lease path, visiting each distinct state
once in breadth-first order; a breaking state prints `ProtocolInvariantViolated` with the
path and the shortest sequence reaching it and exits 1. `protocol check --unfenced` runs the
same search over the variant whose raise leaves the guarded object's fence untouched.

`protocol run <compaction|run>` reads one step per line on standard input (`grant 0`,
`raise 0`, `send 1`, `expire`, …) and prints the state after each step on that path.
`protocol steps` prints every step kind, one per line.
-/

open Protocol

/-- The nodes the bounded check ranges over. -/
def nodes : List Node := [0, 1, 2]

/-- The lease generations the bounded check ranges over: no state past fence 4 is expanded. -/
def generations : Nat := 4

/-- The lease paths the bounded check covers. -/
def paths : List Path := [.compaction, .run]

/-- Every step over the three nodes. -/
def steps : List Step :=
  .expire :: nodes.flatMap fun n =>
    [.grant n, .raise n, .renew n, .release n, .send n, .deliver n, .pause n, .resume n, .crash n]

/-- A state as a finite value: steps over `nodes` touch no other node, so the three views
determine the state. -/
structure Snapshot where
  lease : Option Lease
  fence : Nat
  head : Nat
  views : List NodeState
  deriving BEq, Hashable

def Snapshot.of (s : State) : Snapshot := ⟨s.lease, s.fence, s.head, nodes.map s.node⟩

def Snapshot.toState (x : Snapshot) : State :=
  ⟨x.lease, x.fence, x.head, fun n => x.views.getD n NodeState.idle⟩

/-- The invariant a transition breaks, if any: the same five properties
`Protocol.Invariants` proves over every reachable state. -/
def broken (stepFn : State → Step → State) (s : State) (st : Step) : Option String :=
  let s' := stepFn s st
  let beliefs := nodes.filterMap fun n => (s'.node n).belief
  if beliefs.eraseDups.length != beliefs.length then some "one lease holder per fence"
  else if !(s.granted ≤ s'.granted && s.fence ≤ s'.fence) then some "fences only increase"
  else if (landed s st).any (fun f => nodes.any fun m => (s.acquire m).any (f < ·)) then
    some "no commit after a successor's acquisition"
  else if s'.head < s.head then some "no lost update"
  else match st with
    | .release _ => if (s'.lease.map Lease.fence) != (s.lease.map Lease.fence) then
        some "release keeps the lease object and its fence" else none
    | _ => none

def showPath : Path → String
  | .compaction => "compaction"
  | .run => "run"

def showStep (st : Step) : String :=
  match st with
  | .grant n | .raise n | .renew n | .release n | .send n | .deliver n | .pause n | .resume n | .crash n =>
    s!"{st.kind} {n}"
  | .expire => st.kind

def showState (s : State) : String :=
  let lease := match s.lease with
    | none => "none"
    | some l => s!"holder={l.holder} fence={l.fence} live={l.live && l.holder.isSome}"
  let views := nodes.map fun n =>
    let v := s.node n
    s!"n{n}(belief={v.belief} raised={v.raised} paused={v.paused} pending={v.pending})"
  s!"lease[{lease}] fence={s.fence} head={s.head} " ++ " ".intercalate views

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

def parsePath : String → Option Path
  | "compaction" => some .compaction
  | "run" => some .run
  | _ => none

def parseStep (line : String) : Option Step :=
  match (line.splitOn " ").filter (· ≠ "") with
  | ["expire"] => some .expire
  | ["grant", n] => (parseNode n).map .grant
  | ["raise", n] => (parseNode n).map .raise
  | ["renew", n] => (parseNode n).map .renew
  | ["release", n] => (parseNode n).map .release
  | ["send", n] => (parseNode n).map .send
  | ["deliver", n] => (parseNode n).map .deliver
  | ["pause", n] => (parseNode n).map .pause
  | ["resume", n] => (parseNode n).map .resume
  | ["crash", n] => (parseNode n).map .crash
  | _ => none

partial def runLoop (p : Path) (stdin : IO.FS.Stream) (s : State) (writes : Nat) : IO UInt32 := do
  let line ← stdin.getLine
  if line.isEmpty then return 0
  let text := line.trimAscii.toString
  if text.isEmpty then return (← runLoop p stdin s writes)
  match parseStep text with
  | none =>
    IO.eprintln s!"unrecognised step: {text}"
    return 2
  | some st =>
    let s' := step p s st
    let writes' := writes + (if wrote p s st then 1 else 0)
    IO.println s!"{showState s'} writes={writes'}"
    runLoop p stdin s' writes'

def main (args : List String) : IO UInt32 := do
  match args with
  | ["run", path] =>
    match parsePath path with
    | some p => runLoop p (← IO.getStdin) State.init 0
    | none =>
      IO.eprintln s!"unrecognised lease path: {path}"
      return 2
  | ["steps"] =>
    for k in Step.kinds do IO.println k
    return 0
  | [] | ["check"] | ["check", "--unfenced"] =>
    let fenced := !args.contains "--unfenced"
    let mut total := 0
    for p in paths do
      match search (stepWith p fenced) with
      | .ok n => total := total + n
      | .error (trace, why) =>
        IO.eprintln s!"ProtocolInvariantViolated: {why} on the {showPath p} path, after {trace.length} steps"
        for st in trace do IO.eprintln s!"  {showStep st}"
        return 1
    IO.println s!"ok: 5 invariants hold on {total} states over {nodes.length} nodes and {generations} lease generations on {paths.length} lease paths"
    return 0
  | _ =>
    IO.eprintln "usage: protocol [check [--unfenced] | run <compaction|run> | steps]"
    return 2
