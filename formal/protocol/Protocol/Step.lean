/-!
# The lease, compare-and-swap and fence protocol

A total, computable step function over one lease object, the one object its fence guards,
and the nodes contending for the lease. Two lease paths share it:

* `compaction` — a table's compaction lease object in the bucket guarding the table
  pointer: `Syncer::acquire` writes the lease, then raises the pointer's fence, and
  `Syncer::publish` replaces the pointer under the holder's fence.
* `run` — a pipeline's catalog lease row guarding its commit log: the runner takes the row,
  then `commit_log::open_fence` appends an acquisition entry, and the commit point appends
  a commit entry under the holder's fence.

Acquisition is two steps. A grant writes the lease object with the fence plus one; a raise
then writes that fence into the guarded object, and the acquisition completes. A node
commits only after its acquisition completes. A commit is a fenced compare-and-swap: the
node sends a write carrying the fence it believes it holds, the message may sit in flight
across any number of other steps, and on delivery the guarded object applies it only when
the carried fence is at least the fence the object holds. A node judges nothing by the
clock; expiry is one more step. A raise moves the guarded object's fence and keeps its head,
the last commit applied.
-/

namespace Protocol

/-- A node identifier. The proofs range over every node; the bounded check uses three. -/
abbrev Node := Nat

/-- The two lease paths: the bucket compaction lease guarding a table pointer, and the
catalog run lease guarding a pipeline's commit log. -/
inductive Path where
  | compaction
  | run
  deriving DecidableEq, Hashable, Repr

/-- The lease object: its holder (`none` once released), its fence, and whether the grant is
still unexpired. The object is never deleted once created. -/
structure Lease where
  holder : Option Node
  fence : Nat
  live : Bool
  deriving DecidableEq, Hashable, Repr

/-- One node's local view: the fence it believes it holds, whether its acquisition of that
fence completed, whether it is paused, and the fence of the one commit it has in flight. -/
structure NodeState where
  belief : Option Nat
  raised : Bool
  paused : Bool
  pending : Option Nat
  deriving DecidableEq, Hashable, Repr

/-- The idle node: no belief, running, nothing in flight. -/
def NodeState.idle : NodeState := ⟨none, false, false, none⟩

/-- The protocol state: the lease object (`none` before its first creation), the fence the
guarded object holds, the fence of the last commit it applied (its head: the published
snapshot or the newest commit entry, 0 before any), and every node's view. -/
structure State where
  lease : Option Lease
  fence : Nat
  head : Nat
  node : Node → NodeState

/-- The initial state: no lease object, the guarded object at fence 0 with no commit, every
node idle. -/
def State.init : State := ⟨none, 0, 0, fun _ => NodeState.idle⟩

/-- The highest fence granted: the lease object's fence, 0 before its creation. -/
def State.granted (s : State) : Nat :=
  match s.lease with
  | none => 0
  | some l => l.fence

/-- The steps: the grant and the raise of an acquisition, renewal, expiry and release;
sending a fenced commit and its delayed delivery; holder pause and resumption; and crash. -/
inductive Step where
  | grant (n : Node)
  | raise (n : Node)
  | renew (n : Node)
  | expire
  | release (n : Node)
  | send (n : Node)
  | deliver (n : Node)
  | pause (n : Node)
  | resume (n : Node)
  | crash (n : Node)
  deriving DecidableEq, Repr

/-- The spelling of each step kind, as the harness and the step listing write it. -/
def Step.kind : Step → String
  | .grant _ => "grant"
  | .raise _ => "raise"
  | .renew _ => "renew"
  | .expire => "expire"
  | .release _ => "release"
  | .send _ => "send"
  | .deliver _ => "deliver"
  | .pause _ => "pause"
  | .resume _ => "resume"
  | .crash _ => "crash"

/-- Every step kind, in declaration order. -/
def Step.kinds : List String :=
  ["grant", "raise", "renew", "expire", "release", "send", "deliver", "pause", "resume", "crash"]

/-- The listing names every step: a constructor missing from `Step.kinds` fails elaboration. -/
theorem Step.kind_listed (st : Step) : st.kind ∈ Step.kinds := by
  cases st <;> simp [Step.kind, Step.kinds]

/-- Replace one node's view. -/
def State.setNode (s : State) (n : Node) (v : NodeState) : State :=
  { s with node := fun m => if m = n then v else s.node m }

/-- Grant the lease to `n` at fence `f`: the lease object names `n`, and `n` believes it holds
`f` with its acquisition not yet complete. The guarded object keeps its fence. -/
def grant (s : State) (n : Node) (f : Nat) : State :=
  { s.setNode n { s.node n with belief := some f, raised := false } with lease := some ⟨some n, f, true⟩ }

/-- Whether a lease object admits a new grant: released or expired. -/
def Lease.open (l : Lease) : Bool := l.holder.isNone || !l.live

/-- Whether node `n` holds the lease object's current grant, its acquisition complete, by its
own view. -/
def holds (s : State) (n : Node) (l : Lease) : Bool :=
  decide (l.holder = some n) && decide ((s.node n).belief = some l.fence) && (s.node n).raised

/-- Drop node `n`'s belief: its acquisition and every commit it would send are abandoned. -/
def State.abandon (s : State) (n : Node) : State :=
  s.setNode n { s.node n with belief := none, raised := false }

/-- Whether the run path's commit check passes for `n` at fence `f`: the catalog row still
names `n` at `f`, unexpired. -/
def leaseHolds (s : State) (n : Node) (f : Nat) : Bool :=
  match s.lease with
  | some l => decide (l.holder = some n) && decide (l.fence = f) && l.live
  | none => false

/-- The step function, parameterised by the lease path and by whether a raise writes the
fence into the guarded object. Every step is total: a step whose guard fails leaves the
state unchanged, the refusal the store reports as `LeaseHeld`, `LeaseNotHeld` or
`LeaseFenced`.

A node starts no second acquisition while its first is incomplete, and a node's release
ends its own belief whether or not the lease object still names it.

The paths differ in three places. A raise finding a higher fence in the guarded object
completes on the compaction path, whose publish then loses its condition, and abandons the
acquisition on the run path, whose acquisition entry refuses. A run sends a commit only
while its catalog row holds, and abandons the run and releases the row otherwise. A
compaction lease is never renewed. -/
def stepWith (p : Path) (fenced : Bool) (s : State) : Step → State
  | .grant n =>
    if (s.node n).paused || ((s.node n).belief.isSome && !(s.node n).raised) then s else
    match s.lease with
    | none => grant s n 1
    | some l => if l.open then grant s n (l.fence + 1) else s
  | .raise n =>
    let v := s.node n
    match v.belief with
    | some f =>
      if v.paused || v.raised then s
      else if f < s.fence then
        match p with
        | .compaction => s.setNode n { v with raised := true }
        | .run => s.abandon n
      else
        { s.setNode n { v with raised := true } with fence := if fenced then f else s.fence }
    | none => s
  | .renew n =>
    match s.lease with
    | some l =>
      if decide (p = .run) && !(s.node n).paused && holds s n l then { s with lease := some { l with live := true } }
      else s
    | none => s
  | .expire =>
    match s.lease with
    | some l => { s with lease := some { l with live := false } }
    | none => s
  | .release n =>
    if (s.node n).paused || !(s.node n).raised then s else
    match s.lease with
    | some l =>
      if holds s n l then { s.abandon n with lease := some { l with holder := none } }
      else s.abandon n
    | none => s.abandon n
  | .send n =>
    let v := s.node n
    match v.belief, v.pending with
    | some f, none =>
      if v.paused || !v.raised then s
      else match p with
        | .compaction => s.setNode n { v with pending := some f }
        | .run =>
          if leaseHolds s n f then s.setNode n { v with pending := some f }
          else
            match s.lease with
            | some l => if l.holder = some n && l.fence = f then { s.abandon n with lease := some { l with holder := none } } else s.abandon n
            | none => s.abandon n
    | _, _ => s
  | .deliver n =>
    match (s.node n).pending with
    | some f =>
      let s' := s.setNode n { s.node n with pending := none }
      if s.fence ≤ f then { s' with fence := f, head := f } else s'
    | none => s
  | .pause n => s.setNode n { s.node n with paused := true }
  | .resume n => s.setNode n { s.node n with paused := false }
  | .crash n => s.setNode n { s.node n with belief := none, raised := false, paused := false }

/-- The protocol's step function on path `p`. -/
-- spec: assurance.model.protocol-model@0732c506
def step (p : Path) : State → Step → State := stepWith p true

/-- The fence of the commit a step lands: the carried fence of a delivered write whose
condition holds; `none` for every other step. -/
def landed (s : State) : Step → Option Nat
  | .deliver n =>
    match (s.node n).pending with
    | some f => if s.fence ≤ f then some f else none
    | none => none
  | _ => none

/-- The fence of node `m`'s completed acquisition: its belief once its raise is written,
`none` before. -/
def State.acquire (s : State) (m : Node) : Option Nat :=
  if (s.node m).raised then (s.node m).belief else none

/-- A lost update: a step on path `p` removes the guarded object's head, the last commit it
applied. -/
def lostUpdate (p : Path) (s : State) (st : Step) : Prop :=
  (step p s st).head < s.head

/-- Whether a step writes the guarded object: a raise that moves its fence, or a landed
commit. The harness compares the count of these writes with the object's own. -/
def wrote (p : Path) (s : State) (st : Step) : Bool :=
  match st with
  | .raise _ => (step p s st).fence != s.fence
  | _ => (landed s st).isSome

/-- The states the step function on path `p` reaches from the initial state. -/
inductive Reachable (p : Path) : State → Prop where
  | init : Reachable p State.init
  | step {s : State} (st : Step) : Reachable p s → Reachable p (step p s st)

end Protocol
