/-!
# The authority mapping

The delegation profile's permission decision, as the specification states it. A grant
names actions and table patterns, and optionally a tenant scope, a template allowlist,
aggregate constraints, a row, duration and response-byte ceiling and a horizon. A credential carries an authority block
and the blocks appended after it; a block after the first contributes no authority to an
allow decision, so appending narrows a credential or adds nothing. A statement over several
tables needs every access in one coherent grant: actions and tables never flatten into
independent allowlists. The trusted environment supplies the reserved facts — the current
instant and the checkpoint's expected audience — and stays fixed across the comparison.

Narrowing compares a proposed child grant list with its parent grant list, dimension by
dimension, in the order the derivation flowchart gives: widening, expiry, then tenant scope.
A child ceiling above its parent's widens. An admitted child grant takes effect under the
first parent grant including it, inheriting that grant's aggregate constraints when it names
none and the lesser of each ceiling and horizon, which is the child's own where it names one.

Credential bytes, signatures, block chaining and the library's evaluator lie outside the
model; a credential here is the value the library hands the engine after verification.
-/

namespace Authority

set_option genSizeOfSpec false
set_option genInjectivity false

/-- A table name, template identifier, function name or audience, as a character list. -/
abbrev Name := List Char

/-- The action vocabulary: `read` for every row-returning surface, `write` to land rows,
`execute` to fire a run, `admin` to mint, `forget` to erase subject data, and
`read-individual` to read a `cohort` model's source rows beside `read`. -/
inductive Action where
  | read
  | write
  | execute
  | admin
  | forget
  | readIndividual
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

/-- Pattern forms: `*` covers every table, a prefix covers every name beginning with it, any
other string matches exactly, and no concrete pattern covers `*`. -/
-- spec: authority.grant.pattern-forms@8bb6d690
theorem pattern_covers_forms :
    (∀ n : Name, Pattern.star.covers n = true) ∧
      (∀ p n : Name, (Pattern.prefixed p).covers n = true ↔ p <+: n) ∧
        (∀ s n : Name, (Pattern.literal s).covers n = true ↔ s = n) ∧
          ∀ p : Pattern, p.subsumes .star = true → p = .star := by
  refine ⟨fun _ => rfl, fun _ _ => decide_eq_true_iff, fun _ _ => decide_eq_true_iff, ?_⟩
  intro p h
  cases p with
  | star => rfl
  | prefixed _ => cases h
  | literal _ => cases h

/-! ## Grant dimensions -/

/-- A tenant scope: a table and an opaque byte string bound to that table's outermost
partition column. Two scopes are the same when both components are identical. -/
abbrev Tenant := Name × Name

set_option genCtorIdx false in
/-- An exact decimal, `mantissa / 10 ^ exponent`. -/
structure Decimal where
  mantissa : Int
  exponent : Nat

/-- Exact decimal order. -/
def Decimal.le (a b : Decimal) : Bool :=
  decide (a.mantissa * ((10 ^ b.exponent : Nat) : Int) ≤ b.mantissa * ((10 ^ a.exponent : Nat) : Int))

theorem Decimal.le_trans : ∀ {a b c : Decimal}, a.le b = true → b.le c = true → a.le c = true := by
  intro a b c hab hbc
  have pos : ∀ e : Nat, (0 : Int) < ((10 ^ e : Nat) : Int) := fun e =>
    Int.ofNat_lt.mpr (Nat.pow_pos (by decide))
  have h1 := Int.mul_le_mul_of_nonneg_right (of_decide_eq_true hab) (Int.le_of_lt (pos c.exponent))
  have h2 := Int.mul_le_mul_of_nonneg_right (of_decide_eq_true hbc) (Int.le_of_lt (pos a.exponent))
  rw [Int.mul_right_comm b.mantissa] at h1
  rw [Int.mul_right_comm c.mantissa] at h2
  refine decide_eq_true (Int.le_of_mul_le_mul_right ?_ (pos b.exponent))
  rw [Int.mul_right_comm a.mantissa]
  exact Int.le_trans h1 h2

set_option genCtorIdx false in
/-- An aggregate grant's constraints: a minimum group size, a maximum single-contributor
share, the permitted functions, a groups-per-query ceiling and a row ceiling. -/
structure Aggregate where
  minGroupSize : Nat
  share : Decimal
  functions : List Name
  maxGroups : Nat
  maxRows : Option Nat

/-- The groups-per-query ceiling as it takes effect: at least 1. -/
def Aggregate.ceiling (a : Aggregate) : Nat := max a.maxGroups 1

set_option genCtorIdx false in
/-- What an aggregate statement releases: its smallest group, its largest single-contributor
share, the function it applies and the groups it returns. -/
structure Release where
  smallestGroup : Nat
  largestShare : Decimal
  function : Name
  groups : Nat

/-- Whether an optional ceiling admits a count; an absent ceiling admits every count. -/
def underCeiling (ceiling : Option Nat) (n : Nat) : Bool := ceiling.all (fun m => decide (n ≤ m))

/-- An aggregate grant admits a release meeting each of its constraints. -/
def Aggregate.admits (a : Aggregate) (q : Release) (rows : Nat) : Bool :=
  decide (a.minGroupSize ≤ q.smallestGroup) && q.largestShare.le a.share && a.functions.contains q.function &&
    decide (q.groups ≤ a.ceiling) && underCeiling a.maxRows rows

set_option genCtorIdx false in
/-- A grant. An absent tenant scope, aggregate, ceiling or horizon leaves its dimension
unconstrained; an absent template allowlist confers no template. The duration and
response-byte ceilings bound execution and the response, which no request here carries, so
only narrowing reads them. -/
structure Grant where
  actions : List Action
  tables : List Pattern
  tenant : Option Tenant
  templates : Option (List Name)
  aggregate : Option Aggregate
  maxRows : Option Nat
  maxDurationMs : Option Nat
  maxResponseBytes : Option Nat
  horizon : Option Nat

set_option genCtorIdx false in
/-- One statement's request: an action over every table the statement names, the tenant
partition it reads (`none` reads every tenant), the template it invokes (`none` for a raw
statement), the aggregate it releases (`none` for a raw read), the rows it returns and the
latest availability instant it reads. -/
structure Request where
  action : Action
  tables : List Name
  tenant : Option Tenant
  template : Option Name
  aggregate : Option Release
  rows : Nat
  availability : Nat

/-- The allowlist entry authorizing every template. -/
def allTemplates : Name := ['*']

/-- Whether an allowlist authorizes a template: `*` every template, otherwise the named
identifiers. -/
def allows (l : List Name) (id : Name) : Bool := l.contains allTemplates || l.contains id

/-- Whether a grant's allowlist authorizes a template; an absent allowlist authorizes none. -/
def Grant.allowsTemplate (g : Grant) (id : Name) : Bool := g.templates.any (fun l => allows l id)

def Grant.actionAdmits (g : Grant) (r : Request) : Bool := g.actions.contains r.action

def Grant.tablesAdmit (g : Grant) (r : Request) : Bool :=
  r.tables.all (fun t => g.tables.any (fun p => p.covers t))

def Grant.tenantAdmits (g : Grant) (r : Request) : Bool :=
  g.tenant.all (fun t => decide (r.tenant = some t))

def Grant.templateAdmits (g : Grant) (r : Request) : Bool := r.template.all g.allowsTemplate

/-- A raw grant admits every statement; an aggregate grant admits only a release meeting its
constraints, and no raw read. -/
def Grant.aggregateAdmits (g : Grant) (r : Request) : Bool :=
  g.aggregate.all (fun a => r.aggregate.any (fun q => a.admits q r.rows))

def Grant.rowsAdmit (g : Grant) (r : Request) : Bool := underCeiling g.maxRows r.rows

def Grant.horizonAdmits (g : Grant) (r : Request) : Bool := underCeiling g.horizon r.availability

/-- A grant permits a request when every dimension admits it. -/
def Grant.permits (g : Grant) (r : Request) : Bool :=
  g.actionAdmits r && g.tablesAdmit r && g.tenantAdmits r && g.templateAdmits r && g.aggregateAdmits r &&
    g.rowsAdmit r && g.horizonAdmits r

theorem Grant.permits_iff (g : Grant) (r : Request) :
    g.permits r = true ↔
      g.actionAdmits r = true ∧ g.tablesAdmit r = true ∧ g.tenantAdmits r = true ∧ g.templateAdmits r = true ∧
        g.aggregateAdmits r = true ∧ g.rowsAdmit r = true ∧ g.horizonAdmits r = true := by
  simp only [Grant.permits, Bool.and_eq_true, and_assoc]

/-- Grant fields: an absent tenant scope, aggregate, row ceiling or horizon leaves its
dimension unconstrained, and an absent template allowlist confers no template. -/
theorem absent_grant_field_admits :
    ∀ (g : Grant) (r : Request),
      (g.tenant = none → g.tenantAdmits r = true) ∧ (g.aggregate = none → g.aggregateAdmits r = true) ∧
        (g.maxRows = none → g.rowsAdmit r = true) ∧ (g.horizon = none → g.horizonAdmits r = true) ∧
          (g.templates = none → ∀ id : Name, g.allowsTemplate id = false) := by
  intro g r
  refine ⟨fun h => ?_, fun h => ?_, fun h => ?_, fun h => ?_, fun h _ => ?_⟩
  · unfold Grant.tenantAdmits; rw [h]; rfl
  · unfold Grant.aggregateAdmits; rw [h]; rfl
  · unfold Grant.rowsAdmit underCeiling; rw [h]; rfl
  · unfold Grant.horizonAdmits underCeiling; rw [h]; rfl
  · unfold Grant.allowsTemplate; rw [h]; rfl

/-- Aggregate grants: a grant carrying aggregate constraints, or carrying no `read`,
contributes nothing to a raw read. -/
theorem aggregate_grant_no_raw_read :
    ∀ (g : Grant) (r : Request), r.action = .read → r.aggregate = none →
      (g.aggregate.isSome = true ∨ g.actions.contains .read = false) → g.permits r = false := by
  intro g r hread hraw hg
  cases hp : g.permits r
  · rfl
  · have h := (Grant.permits_iff g r).mp hp
    rcases hg with hg | hg
    · have hagg := h.2.2.2.2.1
      unfold Grant.aggregateAdmits at hagg
      rw [hraw] at hagg
      cases hga : g.aggregate
      · rw [hga] at hg; cases hg
      · rw [hga] at hagg; cases hagg
    · have ha : g.actions.contains r.action = true := h.1
      rw [hread, hg] at ha
      cases ha

/-! ## Narrowing -/

def Grant.actionsWithin (c p : Grant) : Bool := c.actions.all (fun a => p.actions.contains a)

def Grant.tablesWithin (c p : Grant) : Bool := c.tables.all (fun q => p.tables.any (fun pp => pp.subsumes q))

/-- A child allowlist names only identifiers its parent named or covered. Absent on the child
confers no template; an absent parent allowlist admits only an allowlist naming nothing. -/
def Grant.templatesWithin (c p : Grant) : Bool :=
  c.templates.all (fun cs => p.templates.elim cs.isEmpty
    (fun ps => ps.contains allTemplates || cs.all (fun t => ps.contains t)))

/-- Whether one optional ceiling lies within another; either absent compares nothing. -/
def ceilingWithin (c p : Option Nat) : Bool := c.all (fun x => underCeiling p x)

/-- Every aggregate constraint holds or tightens, the groups-per-query ceilings as they take
effect. -/
def Aggregate.within (c p : Aggregate) : Bool :=
  decide (p.minGroupSize ≤ c.minGroupSize) && c.share.le p.share && c.functions.all (fun f => p.functions.contains f) &&
    decide (c.ceiling ≤ p.ceiling) && ceilingWithin c.maxRows p.maxRows

/-- Aggregate constraints compare only where both grants carry them: an absent child aggregate
inherits its parent's, and an absent parent aggregate is unconstrained. -/
def Grant.aggregateWithin (c p : Grant) : Bool := c.aggregate.all (fun ca => p.aggregate.all ca.within)

/-- A later horizon reads later rows; an absent child horizon inherits its parent's. -/
def Grant.horizonWithin (c p : Grant) : Bool := ceilingWithin c.horizon p.horizon

/-- A parent tenant scope is kept byte for byte; adding a scope to an unscoped parent narrows. -/
def Grant.tenantKept (c p : Grant) : Bool := p.tenant.all (fun t => decide (c.tenant = some t))

/-- The dimensions `AttenuationWidens` names, in the order the refusal lists them. -/
inductive Dimension where
  | actions
  | tables
  | templates
  | aggregate
  | maxRows
  | maxDurationMs
  | maxResponseBytes
  | horizon
  deriving DecidableEq

/-- The dimension when a check fails, nothing when it holds. -/
def failing (ok : Bool) (d : Dimension) : List Dimension := if ok then [] else [d]

/-- The dimensions on which child grant `c` is broader than parent grant `p`, tenant scope aside. -/
def Grant.widened (c p : Grant) : List Dimension :=
  failing (c.actionsWithin p) .actions ++ failing (c.tablesWithin p) .tables ++
    failing (c.templatesWithin p) .templates ++ failing (c.aggregateWithin p) .aggregate ++
      failing (ceilingWithin c.maxRows p.maxRows) .maxRows ++
        failing (ceilingWithin c.maxDurationMs p.maxDurationMs) .maxDurationMs ++
          failing (ceilingWithin c.maxResponseBytes p.maxResponseBytes) .maxResponseBytes ++
            failing (c.horizonWithin p) .horizon

/-- The narrowing check on one grant: the child is broader than the parent on no dimension
and keeps the parent's tenant scope. -/
def Grant.includedIn (c p : Grant) : Bool := (c.widened p).isEmpty && c.tenantKept p

theorem failing_nil {ok : Bool} {d : Dimension} (h : failing ok d = []) : ok = true := by
  cases ok
  · cases h
  · rfl

theorem Grant.within_of_widened {c p : Grant} (h : c.widened p = []) :
    c.actionsWithin p = true ∧ c.tablesWithin p = true ∧ c.templatesWithin p = true ∧
      c.aggregateWithin p = true ∧ ceilingWithin c.maxRows p.maxRows = true ∧
        ceilingWithin c.maxDurationMs p.maxDurationMs = true ∧
          ceilingWithin c.maxResponseBytes p.maxResponseBytes = true ∧ c.horizonWithin p = true := by
  unfold Grant.widened at h
  obtain ⟨h, hh⟩ := List.append_eq_nil_iff.mp h
  obtain ⟨h, hg⟩ := List.append_eq_nil_iff.mp h
  obtain ⟨h, hf⟩ := List.append_eq_nil_iff.mp h
  obtain ⟨h, he⟩ := List.append_eq_nil_iff.mp h
  obtain ⟨h, hd⟩ := List.append_eq_nil_iff.mp h
  obtain ⟨h, hc⟩ := List.append_eq_nil_iff.mp h
  obtain ⟨ha, hb⟩ := List.append_eq_nil_iff.mp h
  exact ⟨failing_nil ha, failing_nil hb, failing_nil hc, failing_nil hd, failing_nil he, failing_nil hf,
    failing_nil hg, failing_nil hh⟩

/-- The lesser of two optional ceilings; an absent one imposes none. -/
def minOpt (a b : Option Nat) : Option Nat := a.elim b (fun x => some (b.elim x (min x)))

theorem minOpt_bound : ∀ (a b : Option Nat) (x : Nat), underCeiling (minOpt a b) x = true → underCeiling b x = true := by
  intro a b x h
  cases a with
  | none => exact h
  | some a =>
    cases b with
    | none => rfl
    | some b => exact decide_eq_true (Nat.le_trans (of_decide_eq_true h) (Nat.min_le_right a b))

/-- An aggregate as it takes effect under a parent's: an absent one inherits the parent's, and
the row ceiling is the lesser of the two. -/
def inherit (c p : Option Aggregate) : Option Aggregate :=
  c.elim p (fun ca => some { ca with maxRows := minOpt ca.maxRows (p.bind (·.maxRows)) })

/-- The child grant as it takes effect under parent grant `p`: an absent aggregate inherits the
parent's, and each ceiling and the horizon is the lesser of the two. -/
def Grant.under (c p : Grant) : Grant :=
  { c with
    aggregate := inherit c.aggregate p.aggregate
    maxRows := minOpt c.maxRows p.maxRows
    maxDurationMs := minOpt c.maxDurationMs p.maxDurationMs
    maxResponseBytes := minOpt c.maxResponseBytes p.maxResponseBytes
    horizon := minOpt c.horizon p.horizon }

/-- A ceiling within its parent's takes effect as the child names it, or as the parent's when
the child names none: the lesser of the two clamps nothing. -/
theorem minOpt_of_within : ∀ (c p : Option Nat), ceilingWithin c p = true → minOpt c p = c.or p := by
  intro c p h
  cases c with
  | none => rfl
  | some x =>
    cases p with
    | none => rfl
    | some y => exact congrArg some (Nat.min_eq_left (of_decide_eq_true h))

/-- Ceilings never widen and never clamp: a child grant the narrowing check admits names no
ceiling above its parent grant's, and each ceiling takes effect as the child names it, or as
the parent's when the child names none. -/
theorem grant_under_keeps_named_max_ceilings :
    ∀ (c p : Grant), c.includedIn p = true →
      (c.under p).maxRows = c.maxRows.or p.maxRows ∧ (c.under p).maxDurationMs = c.maxDurationMs.or p.maxDurationMs ∧
        (c.under p).maxResponseBytes = c.maxResponseBytes.or p.maxResponseBytes := by
  intro c p hinc
  have hw := Grant.within_of_widened (List.isEmpty_iff.mp ((Bool.and_eq_true _ _).mp hinc).1)
  exact ⟨minOpt_of_within _ _ hw.2.2.2.2.1, minOpt_of_within _ _ hw.2.2.2.2.2.1,
    minOpt_of_within _ _ hw.2.2.2.2.2.2.1⟩

/-! ### One lemma per dimension -/

theorem actions_within_admits :
    ∀ (c p : Grant) (r : Request), c.actionsWithin p = true → (c.under p).actionAdmits r = true → p.actionAdmits r = true :=
  fun _ _ _ hw ha => List.all_eq_true.mp hw _ (List.contains_iff_mem.mp ha)

theorem tables_within_admit :
    ∀ (c p : Grant) (r : Request), c.tablesWithin p = true → (c.under p).tablesAdmit r = true → p.tablesAdmit r = true := by
  intro c p r hw ht
  refine List.all_eq_true.mpr fun t htm => ?_
  obtain ⟨q, hq, hcov⟩ := List.any_eq_true.mp (List.all_eq_true.mp ht t htm)
  obtain ⟨pp, hpm, hsub⟩ := List.any_eq_true.mp (List.all_eq_true.mp hw q hq)
  exact List.any_eq_true.mpr ⟨pp, hpm, Pattern.subsumes_sound pp q t hsub hcov⟩

theorem tenant_kept_admits :
    ∀ (c p : Grant) (r : Request), c.tenantKept p = true → (c.under p).tenantAdmits r = true → p.tenantAdmits r = true := by
  intro c p r hk ht
  unfold Grant.tenantKept at hk
  unfold Grant.tenantAdmits at ht ⊢
  cases hp : p.tenant with
  | none => rfl
  | some t =>
    rw [hp] at hk
    change (c.under p).tenant.all _ = true at ht
    change c.tenant.all _ = true at ht
    rw [of_decide_eq_true hk] at ht
    exact ht

theorem allows_within :
    ∀ (cs ps : List Name) (id : Name), (ps.contains allTemplates || cs.all (fun t => ps.contains t)) = true →
      allows cs id = true → allows ps id = true := by
  intro cs ps id hw ha
  unfold allows at ha ⊢
  rcases Bool.or_eq_true_iff.mp hw with hw | hw
  · exact Bool.or_eq_true_iff.mpr (Or.inl hw)
  · rcases Bool.or_eq_true_iff.mp ha with ha | ha
    · exact Bool.or_eq_true_iff.mpr (Or.inl (List.all_eq_true.mp hw _ (List.contains_iff_mem.mp ha)))
    · exact Bool.or_eq_true_iff.mpr (Or.inr (List.all_eq_true.mp hw _ (List.contains_iff_mem.mp ha)))

theorem templates_within_allows :
    ∀ (c p : Grant) (id : Name), c.templatesWithin p = true → c.allowsTemplate id = true → p.allowsTemplate id = true := by
  intro c p id hw ha
  unfold Grant.templatesWithin at hw
  unfold Grant.allowsTemplate at ha ⊢
  cases hc : c.templates with
  | none => rw [hc] at ha; cases ha
  | some cs =>
    rw [hc] at hw ha
    cases hp : p.templates with
    | none =>
      rw [hp] at hw
      rw [List.isEmpty_iff.mp hw] at ha
      cases ha
    | some ps =>
      rw [hp] at hw
      exact allows_within cs ps id hw ha

theorem templates_within_admits :
    ∀ (c p : Grant) (r : Request), c.templatesWithin p = true → (c.under p).templateAdmits r = true → p.templateAdmits r = true := by
  intro c p r hw ht
  unfold Grant.templateAdmits at ht ⊢
  cases hr : r.template with
  | none => rfl
  | some id =>
    rw [hr] at ht
    exact templates_within_allows c p id hw ht

theorem Aggregate.within_admits :
    ∀ (ca pa : Aggregate) (q : Release) (rows : Nat), ca.within pa = true →
      ({ ca with maxRows := minOpt ca.maxRows pa.maxRows } : Aggregate).admits q rows = true → pa.admits q rows = true := by
  intro ca pa q rows hw ha
  obtain ⟨hw, -⟩ := (Bool.and_eq_true _ _).mp hw
  obtain ⟨hw, hceil⟩ := (Bool.and_eq_true _ _).mp hw
  obtain ⟨hw, hfun⟩ := (Bool.and_eq_true _ _).mp hw
  obtain ⟨hmin, hshare⟩ := (Bool.and_eq_true _ _).mp hw
  obtain ⟨ha, hrows⟩ := (Bool.and_eq_true _ _).mp ha
  obtain ⟨ha, hqceil⟩ := (Bool.and_eq_true _ _).mp ha
  obtain ⟨ha, hqfun⟩ := (Bool.and_eq_true _ _).mp ha
  obtain ⟨hqmin, hqshare⟩ := (Bool.and_eq_true _ _).mp ha
  have hm : (decide (pa.minGroupSize ≤ q.smallestGroup) && q.largestShare.le pa.share) = true :=
    (Bool.and_eq_true _ _).mpr
      ⟨decide_eq_true (Nat.le_trans (of_decide_eq_true hmin) (of_decide_eq_true hqmin)), Decimal.le_trans hqshare hshare⟩
  have hf := (Bool.and_eq_true _ _).mpr ⟨hm, List.all_eq_true.mp hfun _ (List.contains_iff_mem.mp hqfun)⟩
  have hc := (Bool.and_eq_true _ _).mpr
    ⟨hf, decide_eq_true (Nat.le_trans (of_decide_eq_true hqceil) (of_decide_eq_true hceil))⟩
  exact (Bool.and_eq_true _ _).mpr ⟨hc, minOpt_bound _ _ _ hrows⟩

theorem aggregate_within_admits :
    ∀ (c p : Grant) (r : Request), c.aggregateWithin p = true → (c.under p).aggregateAdmits r = true → p.aggregateAdmits r = true := by
  intro c p r hw ha
  unfold Grant.aggregateAdmits at ha ⊢
  change (inherit c.aggregate p.aggregate).all _ = true at ha
  unfold Grant.aggregateWithin at hw
  cases hp : p.aggregate with
  | none => rfl
  | some pa =>
    rw [hp] at ha hw
    cases hc : c.aggregate with
    | none => rw [hc] at ha; exact ha
    | some ca =>
      rw [hc] at ha hw
      change (r.aggregate.any _) = true at ha
      cases hq : r.aggregate with
      | none => rw [hq] at ha; cases ha
      | some q =>
        rw [hq] at ha
        exact Aggregate.within_admits ca pa q r.rows hw ha

theorem rows_admit :
    ∀ (c p : Grant) (r : Request), (c.under p).rowsAdmit r = true → p.rowsAdmit r = true :=
  fun c p r h => minOpt_bound c.maxRows p.maxRows r.rows h

theorem horizon_admits :
    ∀ (c p : Grant) (r : Request), (c.under p).horizonAdmits r = true → p.horizonAdmits r = true :=
  fun c p r h => minOpt_bound c.horizon p.horizon r.availability h

/-- Inclusion: a child grant the narrowing check admits, as it takes effect under its parent
grant, permits no request that parent grant refuses. -/
theorem grant_includedIn_permits :
    ∀ (child parent : Grant), child.includedIn parent = true →
      ∀ r : Request, (child.under parent).permits r = true → parent.permits r = true := by
  intro c p hinc r hp
  have hb := (Bool.and_eq_true _ _).mp hinc
  have hw := Grant.within_of_widened (List.isEmpty_iff.mp hb.1)
  have h := (Grant.permits_iff _ r).mp hp
  exact (Grant.permits_iff p r).mpr
    ⟨actions_within_admits c p r hw.1 h.1, tables_within_admit c p r hw.2.1 h.2.1,
      tenant_kept_admits c p r hb.2 h.2.2.1, templates_within_admits c p r hw.2.2.1 h.2.2.2.1,
      aggregate_within_admits c p r hw.2.2.2.1 h.2.2.2.2.1, rows_admit c p r h.2.2.2.2.2.1,
      horizon_admits c p r h.2.2.2.2.2.2⟩

/-- Tenant scope never widens: a child grant the narrowing check admits under a tenant-scoped
parent grant carries that scope, and every request it permits reads that tenant alone. -/
theorem tenant_never_widens :
    ∀ (child parent : Grant) (t : Tenant), child.includedIn parent = true → parent.tenant = some t →
      (child.under parent).tenant = some t ∧ ∀ r : Request, (child.under parent).permits r = true → r.tenant = some t := by
  intro c p t hinc ht
  have hk := ((Bool.and_eq_true _ _).mp hinc).2
  unfold Grant.tenantKept at hk
  rw [ht] at hk
  have hct : (c.under p).tenant = some t := of_decide_eq_true hk
  refine ⟨hct, fun r hp => ?_⟩
  have h : (c.under p).tenantAdmits r = true := ((Grant.permits_iff _ r).mp hp).2.2.1
  unfold Grant.tenantAdmits at h
  rw [hct] at h
  exact of_decide_eq_true h

/-- Template allowlists deny by default: absent authorizes none, `*` authorizes every
template, and a child allowlist the narrowing check admits authorizes only templates its
parent's authorizes. -/
-- spec: authority.grant.template-allowlist@417f09a0
theorem template_allowlist :
    (∀ (g : Grant) (id : Name), g.templates = none → g.allowsTemplate id = false) ∧
      (∀ (g : Grant) (l : List Name) (id : Name), g.templates = some l → allTemplates ∈ l → g.allowsTemplate id = true) ∧
        ∀ (c p : Grant) (id : Name), c.templatesWithin p = true → c.allowsTemplate id = true → p.allowsTemplate id = true := by
  refine ⟨fun g id h => ?_, fun g l id h hm => ?_, templates_within_allows⟩
  · unfold Grant.allowsTemplate; rw [h]; rfl
  · unfold Grant.allowsTemplate; rw [h]
    exact Bool.or_eq_true_iff.mpr (Or.inl (List.contains_iff_mem.mpr hm))

/-! ### Narrowing along a chain -/

/-- The effective grants of a child under a parent: each child grant as it takes effect under
the first parent grant including it; `none` when some child grant lies within none. -/
def narrowGrants (parent : List Grant) : List Grant → Option (List Grant)
  | [] => some []
  | c :: cs => (narrowGrants parent cs).bind fun gs => (parent.find? (fun p => c.includedIn p)).map fun p => c.under p :: gs

theorem narrowGrants_permits :
    ∀ (parent child gs : List Grant), narrowGrants parent child = some gs →
      ∀ r : Request, gs.any (fun g => g.permits r) = true → parent.any (fun p => p.permits r) = true := by
  intro parent child
  induction child with
  | nil =>
    intro gs h r hg
    cases h
    cases hg
  | cons c cs ih =>
    intro gs h r hg
    change (narrowGrants parent cs).bind _ = some gs at h
    cases hn : narrowGrants parent cs with
    | none => rw [hn] at h; cases h
    | some gs' =>
      rw [hn] at h
      change (parent.find? (fun p => c.includedIn p)).map _ = some gs at h
      cases hp : parent.find? (fun p => c.includedIn p) with
      | none => rw [hp] at h; cases h
      | some p =>
        rw [hp] at h
        cases h
        obtain ⟨g, hgm, hgp⟩ := List.any_eq_true.mp hg
        rcases List.mem_cons.mp hgm with rfl | hgm
        · exact List.any_eq_true.mpr
            ⟨p, List.mem_of_find?_eq_some hp, grant_includedIn_permits c p (List.find?_some hp) r hgp⟩
        · exact ih gs' hn r (List.any_eq_true.mpr ⟨g, hgm, hgp⟩)

/-- Admission's chain check: each hop narrows the effective grants before it. -/
def checkChain (root : List Grant) : List (List Grant) → Option (List Grant)
  | [] => some root
  | hop :: hops => (narrowGrants root hop).bind fun gs => checkChain gs hops

theorem checkChain_permits_root :
    ∀ (hops : List (List Grant)) (root gs : List Grant), checkChain root hops = some gs →
      ∀ r : Request, gs.any (fun g => g.permits r) = true → root.any (fun g => g.permits r) = true := by
  intro hops
  induction hops with
  | nil =>
    intro root gs h r hg
    cases h
    exact hg
  | cons hop hops ih =>
    intro root gs h r hg
    change (narrowGrants root hop).bind _ = some gs at h
    cases hn : narrowGrants root hop with
    | none => rw [hn] at h; cases h
    | some mid =>
      rw [hn] at h
      exact narrowGrants_permits root hop mid hn r (ih mid gs h r hg)

/-! ## Credentials -/

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

/-- Profile meaning: a credential permits a request exactly when its audience is the
checkpoint's and every block, the authority block and each appended one, admits the request.
No block adds authority another block lacks. -/
theorem permits_iff_audience_and_every_block :
    ∀ (env : Env) (c : Credential) (r : Request),
      permits env c r = true ↔
        c.audience = some env.audience ∧ ∀ (b : Block), b ∈ c.authority :: c.appended → b.admits env r = true := by
  intro env c r
  simp only [permits, Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true, List.mem_cons]
  constructor
  · intro ⟨⟨ha, hauth⟩, happ⟩
    exact ⟨ha, fun b hb => hb.elim (fun h => h ▸ hauth) (happ b)⟩
  · intro ⟨ha, hall⟩
    exact ⟨⟨ha, hall _ (Or.inl rfl)⟩, fun b hb => hall b (Or.inr hb)⟩

/-- A chain of attenuations: append each block in order, leaving the parent's blocks unchanged. -/
def attenuateAll (c : Credential) (bs : List Block) : Credential :=
  bs.foldl attenuate c

theorem attenuateAll_appended :
    ∀ (bs : List Block) (c : Credential), attenuateAll c bs = { c with appended := c.appended ++ bs } := by
  intro bs
  induction bs with
  | nil => intro c; simp [attenuateAll]
  | cons b bs ih =>
    intro c
    change attenuateAll (attenuate c b) bs = _
    rw [ih]
    simp [attenuate, List.append_assoc]

/-- Restriction preservation: for a fixed trusted environment, every block of a credential,
its authority block and each appended one, admits each request that any chain of
attenuations of that credential permits. No attenuation drops a parent restriction. -/
-- spec: assurance.prove.restriction-preservation@99cb0d14
theorem attenuateAll_permits_every_parent_block :
    ∀ (env : Env) (c : Credential) (bs : List Block) (r : Request),
      permits env (attenuateAll c bs) r = true →
        ∀ (b : Block), b ∈ c.authority :: c.appended → b.admits env r = true := by
  intro env c bs r h b hb
  rw [attenuateAll_appended] at h
  have ⟨_, hall⟩ := (permits_iff_audience_and_every_block env _ r).mp h
  apply hall b
  rcases List.mem_cons.mp hb with hb | hb
  · exact hb ▸ List.mem_cons_self
  · exact List.mem_cons_of_mem _ (List.mem_append_left _ hb)

/-- The authority mapping's four proof targets: inclusion, narrowing for a fixed trusted
environment, profile meaning, and restriction preservation. -/
-- spec: assurance.prove.proof-targets@060e2988
theorem authorityMapping_inclusion_narrowing_meaning_restriction :
    (∀ (child parent : Grant), child.includedIn parent = true →
        ∀ r : Request, (child.under parent).permits r = true → parent.permits r = true) ∧
      (∀ (env : Env) (c : Credential) (b : Block) (r : Request),
        permits env (attenuate c b) r = true → permits env c r = true) ∧
      (∀ (env : Env) (c : Credential) (r : Request),
        permits env c r = true ↔
          c.audience = some env.audience ∧ ∀ (b : Block), b ∈ c.authority :: c.appended → b.admits env r = true) ∧
      ∀ (env : Env) (c : Credential) (bs : List Block) (r : Request),
        permits env (attenuateAll c bs) r = true →
          ∀ (b : Block), b ∈ c.authority :: c.appended → b.admits env r = true :=
  ⟨grant_includedIn_permits, attenuate_permits_parent, permits_iff_audience_and_every_block,
    attenuateAll_permits_every_parent_block⟩

end Authority
