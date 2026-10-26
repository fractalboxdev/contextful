/-!
# The fold's pointer commit and collection

A total, computable step function over one table's snapshot directories, its pointer and
its chain of published snapshots, the passes that stage and commit snapshots, and the
readers that resolve the pointer. A pass stages a snapshot under the next id, reading the
pointer's version and the compaction lease's fence; its commit moves the staged directory
into place and replaces the pointer only where the version still matches. Collection
removes each staging directory no pass holds and each snapshot directory no chain reaches,
below the current snapshot's id.

The lease invariant is a hypothesis: a pass commits only while its fence is the highest
granted, so a pass a successor's acquisition superseded never commits. Retention and
publish holds lie outside the model.
-/

namespace Protocol.Fold

/-- The passes a case ranges over. -/
abbrev Pass := Fin 2

/-- The readers a case ranges over. -/
abbrev Reader := Fin 2

/-- A pass: idle, or holding a staged snapshot with the pointer version and lease fence it
read. -/
inductive PassState where
  | idle
  | staged (id version fence : Nat)
  deriving DecidableEq, Repr

/-- The table: the next snapshot id, the highest granted fence, the pointer's version, the
published chain of `(id, fence)` newest first, which snapshot and staging directories exist,
each pass and the snapshot each reader resolved. -/
structure State where
  next : Nat
  granted : Nat
  version : Nat
  chain : List (Nat × Nat)
  final : Nat → Bool
  staging : Nat → Bool
  passes : Pass → PassState
  readers : Reader → Option Nat

def State.init : State := ⟨0, 1, 0, [], fun _ => false, fun _ => false, fun _ => .idle, fun _ => none⟩

/-- The steps. -/
inductive Step where
  /-- A successor acquires the compaction lease. -/
  | acquire
  /-- An idle pass stages a snapshot. -/
  | stage (p : Pass)
  /-- A staged pass commits. -/
  | commit (p : Pass)
  /-- A staged pass dies, leaving its staging directory. -/
  | abandon (p : Pass)
  /-- Collection runs. -/
  | collect
  /-- A reader resolves the pointer. -/
  | read (r : Reader)
  /-- A reader finishes. -/
  | release (r : Reader)
  deriving DecidableEq, Repr

/-- The snapshot the pointer names. -/
def State.current (s : State) : Option Nat := s.chain.head?.map Prod.fst

/-- Whether a chain entry names `id`. -/
def State.reachable (s : State) (id : Nat) : Bool := s.chain.any fun e => e.1 == id

/-- Whether a staged pass holds the staging directory `id`. -/
def State.inFlight (s : State) (id : Nat) : Bool :=
  (List.finRange 2).any fun p => match s.passes p with
    | .staged i _ _ => i == id
    | .idle => false

def State.setPass (s : State) (p : Pass) (v : PassState) : State :=
  { s with passes := fun q => if q = p then v else s.passes q }

/-- Whether collection removes directories under `id`: below the current snapshot and on
no chain. -/
def State.collectible (s : State) (id : Nat) : Bool :=
  match s.current with
  | some cur => id < cur && !s.reachable id
  | none => false

/-- The step function. -/
def step (s : State) : Step → State
  | .acquire => { s with granted := s.granted + 1 }
  | .stage p =>
    match s.passes p with
    | .idle =>
      { s.setPass p (.staged s.next s.version s.granted) with
        next := s.next + 1
        staging := fun i => if i = s.next then true else s.staging i }
    | .staged _ _ _ => s
  | .commit p =>
    match s.passes p with
    | .staged id v f =>
      if f = s.granted then
        let moved := { s.setPass p .idle with
          staging := fun i => if i = id then false else s.staging i
          final := fun i => if i = id then true else s.final i }
        if v = s.version then { moved with version := s.version + 1, chain := (id, f) :: s.chain }
        else moved
      else s
    | .idle => s
  | .abandon p => s.setPass p .idle
  | .collect =>
    { s with
      final := fun i => s.final i && !s.collectible i
      staging := fun i => s.staging i && !(s.collectible i && !s.inFlight i) }
  | .read r => { s with readers := fun q => if q = r then s.current else s.readers q }
  | .release r => { s with readers := fun q => if q = r then none else s.readers q }

/-- The states the step function reaches from the initial state. -/
inductive Reachable : State → Prop where
  | init : Reachable State.init
  | step {s : State} (st : Step) : Reachable s → Reachable (step s st)

end Protocol.Fold
