/-!
# The authority mapping

The delegation profile's permission decision, as the specification states it. A grant
names actions and table patterns. A credential carries an authority block and the blocks
appended after it; a block after the first contributes no authority to an allow decision,
so appending narrows a credential or adds nothing. A statement over several tables needs
every access in one coherent grant: actions and tables never flatten into independent
allowlists. The trusted environment supplies the reserved facts — the current instant and
the checkpoint's expected audience — and stays fixed across the comparison.

Credential bytes, signatures, block chaining and the library's evaluator lie outside the
model; a credential here is the value the library hands the engine after verification.
-/

namespace Authority

set_option genSizeOfSpec false
set_option genInjectivity false

/-- A table name or audience, as a character list. -/
abbrev Name := List Char

/-- The action vocabulary: `read` for every row-returning surface, `write` to land rows,
`execute` to fire a run, `admin` to mint. -/
inductive Action where
  | read
  | write
  | execute
  | admin
  deriving DecidableEq

set_option genCtorIdx false in
/-- A table pattern: `*` covering every table, a prefix ending in `*` covering every name
beginning with that prefix (held without its `*`), or any other string matched exactly. -/
inductive Pattern where
  | star
  | prefixed (p : Name)
  | literal (n : Name)

/-- Whether a pattern covers a table name. -/
def Pattern.covers : Pattern → Name → Bool
  | .star, _ => true
  | .prefixed p, n => decide (p <+: n)
  | .literal s, n => decide (s = n)

/-- The prefix every name a pattern covers begins with; `none` for `*`. -/
def Pattern.stem? : Pattern → Option Name
  | .star => none
  | .prefixed q => some q
  | .literal n => some n

/-- The name a literal pattern matches; `none` for `*` and for a prefix. -/
def Pattern.literal? : Pattern → Option Name
  | .star => none
  | .prefixed _ => none
  | .literal n => some n

/-- Pattern subsumption: `a.subsumes b` when every name `b` covers, `a` covers. A concrete
pattern never subsumes `*`. -/
def Pattern.subsumes : Pattern → Pattern → Bool
  | .star, _ => true
  | .prefixed p, b => b.stem?.any (fun q => decide (p <+: q))
  | .literal s, b => decide (b.literal? = some s)

theorem Pattern.subsumes_sound :
    ∀ (a b : Pattern) (n : Name), a.subsumes b = true → b.covers n = true → a.covers n = true := by
  intro a b n hs hc
  cases a with
  | star => rfl
  | prefixed p =>
    cases b with
    | star => cases hs
    | prefixed q =>
      change decide (p <+: q) = true at hs
      change decide (q <+: n) = true at hc
      exact decide_eq_true ((of_decide_eq_true hs).trans (of_decide_eq_true hc))
    | literal m =>
      change decide (m = n) = true at hc
      cases of_decide_eq_true hc
      exact hs
  | literal s =>
    cases b with
    | star => cases hs
    | prefixed q => cases hs
    | literal m =>
      change decide (some m = some s) = true at hs
      cases of_decide_eq_true hs
      exact hc

set_option genCtorIdx false in
/-- A grant: the actions it confers over the tables its patterns cover. -/
structure Grant where
  actions : List Action
  tables : List Pattern

set_option genCtorIdx false in
/-- One statement's request: an action over every table the statement names. -/
structure Request where
  action : Action
  tables : List Name

/-- A grant permits a request when it confers the action and covers every table named. -/
def Grant.permits (g : Grant) (r : Request) : Bool :=
  g.actions.contains r.action && r.tables.all (fun t => g.tables.any (fun p => p.covers t))

/-- The narrowing check on one grant: every child action is a parent action, and every
child pattern is subsumed by a parent pattern. -/
def Grant.includedIn (child parent : Grant) : Bool :=
  child.actions.all (fun a => parent.actions.contains a) &&
    child.tables.all (fun q => parent.tables.any (fun p => p.subsumes q))

/-- Inclusion: a child grant the narrowing check admits permits no request its parent grant refuses. -/
theorem grant_includedIn_permits :
    ∀ (child parent : Grant), child.includedIn parent = true →
      ∀ r : Request, child.permits r = true → parent.permits r = true := by
  intro child parent hinc r hp
  have ⟨ha, ht⟩ := (Bool.and_eq_true _ _).mp hinc
  have ⟨hpa, hpt⟩ := (Bool.and_eq_true _ _).mp hp
  refine (Bool.and_eq_true _ _).mpr ⟨List.all_eq_true.mp ha _ (List.contains_iff_mem.mp hpa), ?_⟩
  refine List.all_eq_true.mpr fun t htm => ?_
  obtain ⟨q, hq, hcov⟩ := List.any_eq_true.mp (List.all_eq_true.mp hpt t htm)
  obtain ⟨p, hpm, hsub⟩ := List.any_eq_true.mp (List.all_eq_true.mp ht q hq)
  exact List.any_eq_true.mpr ⟨p, hpm, Pattern.subsumes_sound p q t hsub hcov⟩

set_option genCtorIdx false in
/-- A block: the grants it states and the expiry instant it carries, if any. -/
structure Block where
  grants : List Grant
  expiry : Option Nat

set_option genCtorIdx false in
/-- A credential: the audience it names, its authority block, and the blocks appended to it. -/
structure Credential where
  audience : Option Name
  authority : Block
  appended : List Block

set_option genCtorIdx false in
/-- The trusted environment's reserved facts: the evaluation instant and the checkpoint's
expected audience. -/
structure Env where
  now : Nat
  audience : Name

/-- A block admits a request when it has not expired and one of its grants permits the request. -/
def Block.admits (env : Env) (b : Block) (r : Request) : Bool :=
  b.expiry.all (fun e => decide (env.now < e)) && b.grants.any (fun g => g.permits r)

/-- The permission decision: the audience check, the authority block, and every appended block. -/
def permits (env : Env) (c : Credential) (r : Request) : Bool :=
  decide (c.audience = some env.audience) && c.authority.admits env r && c.appended.all (fun b => b.admits env r)

/-- Attenuation: append one block, leaving the parent's blocks unchanged. -/
def attenuate (c : Credential) (b : Block) : Credential :=
  { c with appended := c.appended ++ [b] }

/-- Narrowing: for a fixed trusted environment, permission under an attenuated child implies
permission under its parent. -/
theorem attenuate_permits_parent :
    ∀ (env : Env) (c : Credential) (b : Block) (r : Request),
      permits env (attenuate c b) r = true → permits env c r = true := by
  intro env c b r h
  have ⟨hl, hr⟩ := (Bool.and_eq_true _ _).mp h
  refine (Bool.and_eq_true _ _).mpr ⟨hl, ?_⟩
  exact List.all_eq_true.mpr fun x hx =>
    List.all_eq_true.mp (hr : (c.appended ++ [b]).all _ = true) x (List.mem_append_left _ hx)

/-- The authority mapping's two proof targets: inclusion, and narrowing for a fixed trusted environment. -/
-- spec: assurance.prove.proof-targets@96991a70
theorem authorityMapping_inclusion_and_narrowing :
    (∀ (child parent : Grant), child.includedIn parent = true →
        ∀ r : Request, child.permits r = true → parent.permits r = true) ∧
      ∀ (env : Env) (c : Credential) (b : Block) (r : Request),
        permits env (attenuate c b) r = true → permits env c r = true :=
  ⟨grant_includedIn_permits, attenuate_permits_parent⟩

end Authority
