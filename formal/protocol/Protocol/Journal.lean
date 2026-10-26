/-!
# The journal's claim, record and blob protocol

A total, computable step function over the journal's row store and blob store and the
callers running a journaled step against them. Each `JournalStore` and `BlobStore` port
operation a caller issues — `create_pending`, `read`, `replace_if_pending`, `record`,
`release`, `note_attempts`, `put`, `get` — is one step, as is each effect a caller runs,
each sweep pass and each retirement.

A caller follows the journal's step loop: claim the key with a pending row, or read the row
and replay a recorded value, wait on a live holder, or take a lapsed holder's claim over;
run the effect; re-read, place a value over the inline cutoff in a blob, and record. The
model holds three callers and two keys, each key the one step of its own execution.
-/

namespace Protocol.Journal

/-- The callers a case ranges over. -/
abbrev Caller := Fin 3

/-- The keys a case ranges over. -/
abbrev Key := Fin 2

/-- A value an effect returned: the caller whose effect produced it, and whether it lies
past the inline cutoff, in the blob that caller's value hashes to. -/
structure Value where
  origin : Caller
  blob : Bool
  deriving DecidableEq, Hashable, Repr

/-- A journal row: a pending claim with its holder and closed attempts, or a recorded value. -/
inductive Row where
  | pending (holder : Caller) (attempts : Nat)
  | recorded (v : Value)
  deriving DecidableEq, Hashable, Repr

/-- What an effect returns. -/
inductive Outcome where
  | inline
  | blob
  | fail
  | unjournaled
  deriving DecidableEq, Hashable, Repr

/-- Why a caller releases its claim. -/
inductive Release where
  | failed
  | unrecorded
  deriving DecidableEq, Hashable, Repr

/-- How a step resolved; `missing` is the `BlobMissing` refusal. -/
inductive Resolution where
  | replayed (v : Value)
  | recorded (v : Value)
  | unrecorded
  | failed
  | missing
  deriving DecidableEq, Hashable, Repr

/-- Where a caller stands: the port call it waits on, its effect, or its resolution. -/
inductive Phase where
  | idle
  /-- Next: `create_pending`. -/
  | claiming (k : Key)
  /-- Next: `read`, before the effect. -/
  | reading (k : Key)
  /-- Next: `replace_if_pending`, naming the lapsed holder. -/
  | takingOver (k : Key) (h : Caller)
  /-- Next: the effect. -/
  | claimed (k : Key)
  /-- Next: `read`, after the effect returned `v`. -/
  | checking (k : Key) (v : Value)
  /-- Next: `put` of `v`'s blob. -/
  | storing (k : Key) (v : Value)
  /-- Next: `record`. -/
  | recording (k : Key) (v : Value)
  /-- Next: `release`. -/
  | releasing (k : Key) (r : Release)
  /-- Next: `get` of the recorded value's blob. -/
  | loading (k : Key) (v : Value)
  | done (k : Key) (r : Resolution)
  deriving DecidableEq, Hashable, Repr

/-- The key a non-idle caller works on. -/
def Phase.key : Phase → Option Key
  | .idle => none
  | .claiming k | .reading k | .takingOver k _ | .claimed k | .checking k _ | .storing k _
  | .recording k _ | .releasing k _ | .loading k _ | .done k _ => some k

/-- One caller: its phase, and whether its owner lease lapsed. A lapse is permanent. -/
structure CallerState where
  phase : Phase
  lapsed : Bool
  deriving DecidableEq, Hashable, Repr

/-- The journal state: each key's row, whether each caller's blob is stored, each caller,
and, as a ghost, the records that landed on each key since its execution last retired. -/
structure State where
  rows : Key → Option Row
  blobs : Caller → Bool
  callers : Caller → CallerState
  records : Key → Nat

/-- No row, no blob, every caller idle and live. -/
def State.init : State := ⟨fun _ => none, fun _ => false, fun _ => ⟨.idle, false⟩, fun _ => 0⟩

/-- The steps. -/
inductive Step where
  /-- An idle caller enters the step loop on a key. -/
  | begin (c : Caller) (k : Key)
  /-- The caller's waiting port call runs. -/
  | advance (c : Caller)
  /-- The claimed caller's effect returns. -/
  | effect (c : Caller) (o : Outcome)
  /-- The claimed caller persists one more closed attempt: `note_attempts`. -/
  | note (c : Caller)
  /-- The caller's owner lease lapses. -/
  | lapse (c : Caller)
  /-- The caller's process dies wherever it waits. -/
  | crash (c : Caller)
  /-- A resolved caller drops its result. -/
  | finish (c : Caller)
  /-- One sweep pass, past the grace window when `aged`, otherwise inside it. -/
  | sweep (aged : Bool)
  /-- The key's execution retires: `retire`. -/
  | retire (k : Key)
  deriving DecidableEq, Repr

/-- The highest attempt count a note writes. -/
def maxAttempts : Nat := 2

def State.setPhase (s : State) (c : Caller) (p : Phase) : State :=
  { s with callers := fun d => if d = c then { s.callers c with phase := p } else s.callers d }

def State.setRow (s : State) (k : Key) (r : Option Row) : State :=
  { s with rows := fun j => if j = k then r else s.rows j }

/-- Whether a recorded row names `b`'s blob. -/
def State.referenced (s : State) (b : Caller) : Bool :=
  (List.finRange 2).any fun k => s.rows k == some (.recorded ⟨b, true⟩)

/-- Whether caller `b` stands between the `put` of its blob and its `record`. -/
def State.inFlight (s : State) (b : Caller) : Bool :=
  match (s.callers b).phase with
  | .recording _ v => v.blob && decide (v.origin = b)
  | _ => false

/-- A resolution read from a recorded value: a blob value waits on `get`. -/
def replay (k : Key) (v : Value) : Phase :=
  if v.blob then .loading k v else .done k (.replayed v)

/-- The caller's waiting port call. -/
def advance (s : State) (c : Caller) : State :=
  match (s.callers c).phase with
  | .claiming k =>
    match s.rows k with
    | none => (s.setRow k (some (.pending c 0))).setPhase c (.claimed k)
    | some _ => s.setPhase c (.reading k)
  | .reading k =>
    match s.rows k with
    | none => s.setPhase c (.claiming k)
    | some (.recorded v) => s.setPhase c (replay k v)
    | some (.pending h _) =>
      if h = c then s.setPhase c (.claimed k)
      else if (s.callers h).lapsed then s.setPhase c (.takingOver k h)
      else s.setPhase c (.claiming k)
  | .takingOver k h =>
    match s.rows k with
    | some (.pending h' a) =>
      if h' = h then (s.setRow k (some (.pending c a))).setPhase c (.claimed k)
      else s.setPhase c (.claiming k)
    | _ => s.setPhase c (.claiming k)
  | .checking k v =>
    match s.rows k with
    | some (.recorded w) => s.setPhase c (replay k w)
    | _ => s.setPhase c (if v.blob then .storing k v else .recording k v)
  | .storing k v =>
    { s.setPhase c (.recording k v) with blobs := fun b => if b = v.origin then true else s.blobs b }
  | .recording k v =>
    match s.rows k with
    | some (.recorded w) => s.setPhase c (replay k w)
    | _ =>
      { (s.setRow k (some (.recorded v))).setPhase c (.done k (.recorded v)) with
        records := fun j => if j = k then s.records k + 1 else s.records j }
  | .releasing k r =>
    let s' := match s.rows k with
      | some (.pending h _) => if h = c then s.setRow k none else s
      | _ => s
    s'.setPhase c (.done k (match r with | .failed => .failed | .unrecorded => .unrecorded))
  | .loading k v => s.setPhase c (.done k (if s.blobs v.origin then .replayed v else .missing))
  | _ => s

/-- The step function. With `grace`, an aged sweep spares the blob of a caller between its
`put` and its `record`: the grace window outlasts that span. -/
def stepWith (grace : Bool) (s : State) : Step → State
  | .begin c k => if (s.callers c).phase = .idle then s.setPhase c (.claiming k) else s
  | .advance c => advance s c
  | .effect c o =>
    match (s.callers c).phase with
    | .claimed k =>
      s.setPhase c (match o with
        | .inline => .checking k ⟨c, false⟩
        | .blob => .checking k ⟨c, true⟩
        | .fail => .releasing k .failed
        | .unjournaled => .releasing k .unrecorded)
    | _ => s
  | .note c =>
    match (s.callers c).phase with
    | .claimed k =>
      let n := match s.rows k with
        | some (.pending h a) => if h = c then a + 1 else 1
        | _ => 1
      if n ≤ maxAttempts then
        match s.rows k with
        | none => s.setRow k (some (.pending c n))
        | some (.pending h _) => if h = c then s.setRow k (some (.pending c n)) else s
        | some (.recorded _) => s
      else s
    | _ => s
  | .lapse c => { s with callers := fun d => if d = c then { s.callers c with lapsed := true } else s.callers d }
  | .crash c => s.setPhase c .idle
  | .finish c =>
    match (s.callers c).phase with
    | .done _ _ => s.setPhase c .idle
    | _ => s
  | .sweep aged =>
    if aged then
      { s with blobs := fun b => s.blobs b && (s.referenced b || (grace && s.inFlight b)) }
    else s
  | .retire k =>
    if (List.finRange 3).all fun c => (s.callers c).phase.key != some k then
      { s.setRow k none with records := fun j => if j = k then 0 else s.records j }
    else s

/-- The journal's step function, under the grace hypothesis. -/
def step : State → Step → State := stepWith true

/-- The states the step function reaches from the initial state. -/
inductive Reachable : State → Prop where
  | init : Reachable State.init
  | step {s : State} (st : Step) : Reachable s → Reachable (step s st)

end Protocol.Journal
