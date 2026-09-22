/-!
# The lease, compare-and-swap and fence protocol

A total, computable step function over one lease object, the two objects its fence guards
— the catalog row and the cursor object — and the nodes contending for the lease.

A grant replaces the lease object with the fence plus one and writes that fence into both
guarded objects: the catalog `UPDATE` and the commit-log entry the lease names on
acquisition. A commit is a fenced compare-and-swap: the node sends a write carrying the
fence it believes it holds, the message may sit in flight across any number of other
steps, and on delivery the storage applies it only when the carried fence is at least the
fence the object holds. A node judges nothing by the clock; expiry is one more step.
-/

namespace Protocol

/-- A node identifier. The proofs range over every node; the bounded check uses three. -/
abbrev Node := Nat

/-- The two objects a fence guards. -/
inductive Target where
  | catalog
  | cursor
  deriving DecidableEq, Hashable, Repr

/-- The lease object: its holder (`none` once released), its fence, and whether the grant is
still unexpired. The object is never deleted once created. -/
structure Lease where
  holder : Option Node
  fence : Nat
  live : Bool
  deriving DecidableEq, Hashable, Repr

/-- One node's local view: the fence it believes it holds, whether it is paused, and the one
fenced write it has in flight. -/
structure NodeState where
  belief : Option Nat
  paused : Bool
  pending : Option (Target × Nat)
  deriving DecidableEq, Hashable, Repr

/-- The idle node: no belief, running, nothing in flight. -/
def NodeState.idle : NodeState := ⟨none, false, none⟩

/-- The protocol state: the lease object (`none` before its first creation), the fence each
guarded object holds, and every node's view. -/
structure State where
  lease : Option Lease
  catalogFence : Nat
  cursorFence : Nat
  node : Node → NodeState

/-- The initial state: no lease object, both guarded objects at fence 0, every node idle. -/
def State.init : State := ⟨none, 0, 0, fun _ => NodeState.idle⟩

/-- The highest fence granted: the lease object's fence, 0 before its creation. -/
def State.granted (s : State) : Nat :=
  match s.lease with
  | none => 0
  | some l => l.fence

/-- The fence a guarded object holds. -/
def State.fenceOf (s : State) : Target → Nat
  | .catalog => s.catalogFence
  | .cursor => s.cursorFence

/-- The steps: lease acquisition, renewal, expiry and release; sending a fenced write and
its delayed delivery; holder pause and resumption; and crash. -/
inductive Step where
  | acquire (n : Node)
  | renew (n : Node)
  | expire
  | release (n : Node)
  | send (n : Node) (t : Target)
  | deliver (n : Node)
  | pause (n : Node)
  | resume (n : Node)
  | crash (n : Node)
  deriving DecidableEq, Repr

/-- Replace one node's view. -/
def State.setNode (s : State) (n : Node) (v : NodeState) : State :=
  { s with node := fun m => if m = n then v else s.node m }

/-- Grant the lease to `n` at fence `f`. With `raise`, the grant writes `f` into both guarded
objects; without it, the guarded objects keep their fences until the holder commits. -/
def grant (raise : Bool) (s : State) (n : Node) (f : Nat) : State :=
  let s' := s.setNode n { s.node n with belief := some f }
  { s' with
    lease := some ⟨some n, f, true⟩
    catalogFence := if raise then max s.catalogFence f else s.catalogFence
    cursorFence := if raise then max s.cursorFence f else s.cursorFence }

/-- Whether a lease object admits a new grant: released or expired. -/
def Lease.open (l : Lease) : Bool := l.holder.isNone || !l.live

/-- Whether node `n` holds the lease object's current grant, by its own view. -/
def holds (s : State) (n : Node) (l : Lease) : Bool :=
  decide (l.holder = some n) && decide ((s.node n).belief = some l.fence)

/-- Apply a delivered write to the object it targets. -/
def State.setFence (s : State) : Target → Nat → State
  | .catalog, f => { s with catalogFence := f }
  | .cursor, f => { s with cursorFence := f }

/-- The step function, parameterised by whether a grant raises the guarded objects' fences.
Every step is total: a step whose guard fails leaves the state unchanged, the refusal the
store reports as `LeaseHeld`, `LeaseNotHeld` or `LeaseFenced`. -/
def stepWith (raise : Bool) (s : State) : Step → State
  | .acquire n =>
    if (s.node n).paused then s else
    match s.lease with
    | none => grant raise s n 1
    | some l => if l.open then grant raise s n (l.fence + 1) else s
  | .renew n =>
    match s.lease with
    | some l => if !(s.node n).paused && holds s n l then { s with lease := some { l with live := true } } else s
    | none => s
  | .expire =>
    match s.lease with
    | some l => { s with lease := some { l with live := false } }
    | none => s
  | .release n =>
    match s.lease with
    | some l =>
      if !(s.node n).paused && holds s n l then
        { s.setNode n { s.node n with belief := none } with lease := some { l with holder := none } }
      else s
    | none => s
  | .send n t =>
    let v := s.node n
    match v.belief, v.pending with
    | some f, none => if v.paused then s else s.setNode n { v with pending := some (t, f) }
    | _, _ => s
  | .deliver n =>
    match (s.node n).pending with
    | some (t, f) =>
      let s' := s.setNode n { s.node n with pending := none }
      if s.fenceOf t ≤ f then s'.setFence t f else s'
    | none => s
  | .pause n => s.setNode n { s.node n with paused := true }
  | .resume n => s.setNode n { s.node n with paused := false }
  | .crash n => s.setNode n { s.node n with belief := none, paused := false }

/-- The protocol's step function. -/
-- spec: assurance.model.protocol-model@c6fe5ad1
def step : State → Step → State := stepWith true

/-- The commit a step lands: the target and carried fence of a delivered write whose
condition holds; `none` for every other step. -/
def landed (s : State) : Step → Option (Target × Nat)
  | .deliver n =>
    match (s.node n).pending with
    | some (t, f) => if s.fenceOf t ≤ f then some (t, f) else none
    | none => none
  | _ => none

/-- The states the step function reaches from the initial state. -/
inductive Reachable : State → Prop where
  | init : Reachable State.init
  | step {s : State} (st : Step) : Reachable s → Reachable (step s st)

end Protocol
