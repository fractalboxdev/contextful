/-!
# The bucket manifest merge

A total, computable model of one writer's merge of its local entries into the remote bucket
manifest. The merge decides each key from that key's local entry, remote entry and remote
tombstone alone, so the model is a function per key; a commit-log key whose local and remote
copies differ refuses the whole merge.

An owner is a node number, `0` for an unowned key. Times are Unix seconds.
-/

namespace Protocol.Merge

/-- A node; `0` owns nothing, the unowned key's owner. -/
abbrev Owner := Nat

/-- A manifest entry: its content hash and its owner. -/
structure Entry where
  sha : Nat
  owner : Owner
  deriving DecidableEq, Repr

/-- A tombstone: the node that deleted the entry, and when. -/
structure Tombstone where
  owner : Owner
  deletedAt : Nat
  deriving DecidableEq, Repr

/-- Why a merge keeps an entry a remote tombstone names. -/
inductive Refusal where
  /-- `SyncTombstoneForeign`: the tombstone's owner is not the entry's. -/
  | foreign
  /-- `SyncTombstoneUnverified`: the tombstone fails verification. -/
  | unverified
  deriving DecidableEq, Repr

/-- One key's inputs to a merge. -/
structure Input where
  /-- Whether the key is a commit-log entry. -/
  log : Bool
  /-- Whether the writer's store held the key at its last sync. -/
  held : Bool
  /-- Whether the tombstone the remote lists for the key verifies. -/
  verified : Bool
  localEntry : Option Entry
  remoteEntry : Option Entry
  remoteTomb : Option Tombstone
  deriving DecidableEq, Repr

/-- One key's merged entry and tombstone, and the refusal that kept an entry, if any. -/
structure Output where
  entry : Option Entry
  tomb : Option Tombstone
  refusal : Option Refusal
  deriving DecidableEq, Repr

/-- The age at which a tombstone leaves the manifest: 30 d. -/
def ttl : Nat := 30 * 86400

/-- Whether a commit-log key's two copies differ: the merge refuses outright. -/
def conflicts (me : Owner) (i : Input) : Bool :=
  match i.localEntry, i.remoteEntry with
  | some e, some r => i.log && (e.owner == 0 || e.owner == me) && r.sha != e.sha
  | _, _ => false

/-- The entry and tombstone before remote tombstones apply: a local entry this writer owns or
no writer owns, else the remote entry, which leaves with a fresh tombstone when this writer
owns it and held it. -/
def base (me : Owner) (now : Nat) (i : Input) : Option Entry × Option Tombstone :=
  match i.localEntry.filter (fun e => e.owner == 0 || e.owner == me) with
  | some e => (some e, none)
  | none =>
    match i.remoteEntry with
    | some r => if r.owner == me && i.held then (none, some ⟨me, now⟩) else (some r, none)
    | none => (none, none)

/-- A remote tombstone applied to the entry and tombstone `base` left: a fresh tombstone
stands, an expired one leaves, a foreign one or one naming this writer's entry keeps the
entry, and a verified one removes it. -/
def applyTomb (me now : Nat) (verified : Bool) : Option Entry × Option Tombstone → Option Tombstone → Output
  | (e, some t), _ => ⟨e, some t, none⟩
  | (e, none), none => ⟨e, none, none⟩
  | (e, none), some rt =>
    if ttl ≤ now - rt.deletedAt then ⟨e, none, none⟩
    else match e with
      | none => ⟨none, some rt, none⟩
      | some x =>
        if x.owner != rt.owner then ⟨some x, none, some .foreign⟩
        else if x.owner == me then ⟨some x, none, none⟩
        else if verified then ⟨none, some rt, none⟩
        else ⟨some x, none, some .unverified⟩

/-- One key's merge. -/
def mergeKey (me now : Nat) (i : Input) : Output :=
  applyTomb me now i.verified (base me now i) i.remoteTomb

/-- A merge over every key: `none`, the `SyncCursorConflict` refusal, when a commit-log key's
copies differ. -/
def merge (me : Owner) (now : Nat) (is : List Input) : Option (List Output) :=
  if is.any (conflicts me) then none else some (is.map (mergeKey me now))

/-- The key's inputs read back from a merge's output, for a second merge of the same store. -/
def Input.after (i : Input) (o : Output) : Input :=
  { i with remoteEntry := o.entry, remoteTomb := o.tomb }

end Protocol.Merge
