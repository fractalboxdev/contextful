import Contextful.Layer

/-!
# Placement and the floor

A zone string names where inference runs: `local:device`, `on-prem:<id>`,
`private-cloud:<id>` or `public-cloud:<id>`; any other text is undeclared, and the
identifier is part of the value. An allow-set entry is `*`, `local:device`, a bare
category (`on-prem:*`, `private-cloud:*`, `public-cloud:*`), or a category carrying a
concrete identifier. A zone is admitted when any entry matches, and only `*` admits an
undeclared zone.
-/

set_option genSizeOfSpec false
set_option genInjectivity false

/-- A zone identifier. The identifier space is unbounded; the model holds it as a character list. -/
abbrev Ident := List Char

/-- The categories whose zones carry an identifier: `on-prem`, `private-cloud`, `public-cloud`. -/
inductive Category where
  | onPrem
  | privateCloud
  | publicCloud
  deriving DecidableEq

set_option genCtorIdx false in
set_option genCtorIdx false in
/-- The declared placement: one case per category the manifest admits, each carrying its
identifier inside the constructor, and one case for an absent declaration. `local:device`
names a single zone, so its constructor carries no free identifier. -/
-- spec: assurance.model.placement-inductive@73a8709c
inductive Placement where
  | localDevice
  | onPrem (id : Ident)
  | privateCloud (id : Ident)
  | publicCloud (id : Ident)
  | undeclared

/-- Whether a placement is `local:device`. -/
def Placement.isLocal : Placement → Bool
  | .localDevice => true
  | .onPrem _ => false
  | .privateCloud _ => false
  | .publicCloud _ => false
  | .undeclared => false

/-- The category and identifier of an identifier-carrying placement; `none` otherwise. -/
def Placement.named : Placement → Option (Category × Ident)
  | .localDevice => none
  | .onPrem i => some (.onPrem, i)
  | .privateCloud i => some (.privateCloud, i)
  | .publicCloud i => some (.publicCloud, i)
  | .undeclared => none

/-- The placement of an identifier-carrying category and an identifier. -/
def Placement.ofNamed : Category → Ident → Placement
  | .onPrem, i => .onPrem i
  | .privateCloud, i => .privateCloud i
  | .publicCloud, i => .publicCloud i

theorem Placement.named_ofNamed (c : Category) (i : Ident) : (Placement.ofNamed c i).named = some (c, i) := by
  cases c <;> rfl

/-- The category prefixes the manifest's zone grammar admits. -/
def manifestCategories : List String := ["local", "on-prem", "private-cloud", "public-cloud"]

/-- The placement case a manifest category maps onto, with an empty identifier; `none` for a
category with no case. -/
def Placement.ofCategory? (category : String) : Option Placement :=
  if category = "local" then some .localDevice
  else if category = "on-prem" then some (.onPrem [])
  else if category = "private-cloud" then some (.privateCloud [])
  else if category = "public-cloud" then some (.publicCloud [])
  else none

-- Elaboration raises `UnmodelledConstructor`, naming the category, for a manifest
-- category with no case in `Placement`.
#eval show IO Unit from do
  for c in manifestCategories do
    if (Placement.ofCategory? c).isNone then
      throw (IO.userError s!"UnmodelledConstructor: {c}")

/-! ## Allow-sets and the floor -/

/-- An allow-set: a decidable predicate over a placement value. -/
abbrev AllowSet := Placement → Bool

/-- One allow-set pointwise included in another. -/
def AllowSet.Included (a b : AllowSet) : Prop := ∀ p, a p = true → b p = true

/-- The floor: the pointwise intersection of a list of allow-sets, folded by conjunction;
`floor []` admits every caller as the unit of that fold. -/
-- spec: assurance.model.floor@d85618be
def floor (evidence : List AllowSet) : AllowSet :=
  fun c => evidence.all (fun a => a c)

/-- A floor admitting a caller forces every member of its evidence list to admit that caller. -/
-- spec: assurance.prove.floor-no-downgrade@98394745
theorem floor_no_downgrade :
    ∀ {es : List AllowSet} {c : Placement}, floor es c = true → ∀ a ∈ es, a c = true :=
  fun h => List.all_eq_true.mp h

/-! ## The placement check as a layer -/

/-- The table-zone layer: a row survives when its effective allow-set admits the session's zone. -/
def zoneLayer (effectiveSet : RowId → AllowSet) (session : Placement) : Layer :=
  fun r => effectiveSet r session

/-- With the placement check as one more list member, a row the whole list admits was
admitted by that check. -/
-- spec: assurance.prove.placement-is-a-layer@76f7558d
theorem zone_layer_sound :
    ∀ {effectiveSet : RowId → AllowSet} {session : Placement} {ls : List Layer} {r : RowId},
      zoneLayer effectiveSet session ∈ ls → composed ls r = true → effectiveSet r session = true :=
  fun hmem h => composed_sound h _ hmem

/-! ## Allow-set entries and symbolic inclusion -/

set_option genCtorIdx false in
/-- An allow-set entry: `*`, `local:device`, a bare category, or a category carrying a
concrete identifier. -/
inductive Entry where
  | any
  | localDevice
  | every (c : Category)
  | named (c : Category) (id : Ident)

/-- Whether an entry is `local:device`. -/
def Entry.isLocal : Entry → Bool
  | .any => false
  | .localDevice => true
  | .every _ => false
  | .named _ _ => false

/-- The category an entry is confined to, when it is confined to one. -/
def Entry.category? : Entry → Option Category
  | .any => none
  | .localDevice => none
  | .every c => some c
  | .named c _ => some c

/-- The category and identifier an entry carries, when it carries an identifier. -/
def Entry.named? : Entry → Option (Category × Ident)
  | .any => none
  | .localDevice => none
  | .every _ => none
  | .named c i => some (c, i)

/-- Whether an entry matches a placement: a bare category matches every identifier in it,
an entry carrying an identifier matches that identifier alone, and only `*` matches an
undeclared placement. -/
def Entry.matches : Entry → Placement → Bool
  | .any, _ => true
  | .localDevice, p => p.isLocal
  | .every c, p => decide (p.named.map Prod.fst = some c)
  | .named c i, p => decide (p.named = some (c, i))

/-- The allow-set a list of entries denotes: a placement is admitted when any entry matches. -/
def allowSetOf (es : List Entry) : AllowSet :=
  fun p => es.any (fun e => e.matches p)

/-- Pattern subsumption: `a.subsumes b` when every placement `b` matches, `a` matches. -/
def Entry.subsumes : Entry → Entry → Bool
  | .any, _ => true
  | .localDevice, b => b.isLocal
  | .every c, b => decide (b.category? = some c)
  | .named c i, b => decide (b.named? = some (c, i))

/-- Symbolic inclusion of one entry list in another: every entry of the first is subsumed
by some entry of the second. No placement value is evaluated. -/
def includedIn (small big : List Entry) : Bool :=
  small.all (fun e => big.any (fun a => a.subsumes e))

theorem Entry.subsumes_sound :
    ∀ (a b : Entry) (p : Placement), a.subsumes b = true → b.matches p = true → a.matches p = true := by
  intro a b p hs hm
  cases a with
  | any => rfl
  | localDevice => cases b <;> first | exact hm | cases hs
  | every c =>
    cases b with
    | any => cases hs
    | localDevice => cases hs
    | every d =>
      change decide (some d = some c) = true at hs
      cases of_decide_eq_true hs
      exact hm
    | named d j =>
      change decide (some d = some c) = true at hs
      change decide (p.named = some (d, j)) = true at hm
      cases of_decide_eq_true hs
      exact decide_eq_true (by rw [of_decide_eq_true hm]; rfl)
  | named c i =>
    cases b with
    | any => cases hs
    | localDevice => cases hs
    | every d => cases hs
    | named d j =>
      change decide (some (d, j) = some (c, i)) = true at hs
      cases of_decide_eq_true hs
      exact hm

/-- An identifier longer than every identifier in `avoid`, so absent from it. -/
def fresh (avoid : List Ident) : Ident :=
  List.replicate (avoid.foldr (fun i m => max i.length m) 0 + 1) 'x'

theorem length_le_bound : ∀ (avoid : List Ident) (i : Ident),
    i ∈ avoid → i.length ≤ avoid.foldr (fun i m => max i.length m) 0 := by
  intro avoid i h
  induction avoid with
  | nil => cases h
  | cons j js ih =>
    cases h with
    | head => exact Nat.le_max_left _ _
    | tail _ h' => exact Nat.le_trans (ih h') (Nat.le_max_right _ _)

theorem fresh_not_mem : ∀ (avoid : List Ident), fresh avoid ∉ avoid := by
  intro avoid h
  have hle := length_le_bound avoid (fresh avoid) h
  rw [fresh, List.length_replicate] at hle
  exact Nat.not_succ_le_self _ hle

/-- A placement an entry matches, whose identifier — for a bare category — lies outside
`avoid`, so no identifier-carrying entry drawn from `avoid` matches it. -/
def witness (e : Entry) (avoid : List Ident) : Placement :=
  match e with
  | .any => .undeclared
  | .localDevice => .localDevice
  | .every c => .ofNamed c (fresh avoid)
  | .named c i => .ofNamed c i

theorem witness_matches : ∀ (e : Entry) (avoid : List Ident), e.matches (witness e avoid) = true := by
  intro e avoid
  cases e with
  | any => rfl
  | localDevice => rfl
  | every c => exact decide_eq_true (by rw [witness, Placement.named_ofNamed]; rfl)
  | named c i => exact decide_eq_true (Placement.named_ofNamed c i)

theorem subsumes_of_matches_witness :
    ∀ (a e : Entry) (avoid : List Ident),
      (∀ j, a.named?.map Prod.snd = some j → j ∈ avoid) → a.matches (witness e avoid) = true →
        a.subsumes e = true := by
  intro a e avoid hid hm
  cases a with
  | any => rfl
  | localDevice =>
    cases e with
    | any => cases hm
    | localDevice => rfl
    | every c => cases c <;> cases hm
    | named c i => cases c <;> cases hm
  | every d =>
    cases e with
    | any => cases hm
    | localDevice => cases hm
    | every c =>
      have h := of_decide_eq_true (hm : decide ((witness (.every c) avoid).named.map Prod.fst = some d) = true)
      rw [witness, Placement.named_ofNamed] at h
      exact decide_eq_true h
    | named c i =>
      have h := of_decide_eq_true (hm : decide ((witness (.named c i) avoid).named.map Prod.fst = some d) = true)
      rw [witness, Placement.named_ofNamed] at h
      exact decide_eq_true h
  | named d j =>
    cases e with
    | any => cases hm
    | localDevice => cases hm
    | every c =>
      have h := of_decide_eq_true (hm : decide ((witness (.every c) avoid).named = some (d, j)) = true)
      rw [witness, Placement.named_ofNamed] at h
      cases h
      exact absurd (hid _ rfl) (fresh_not_mem avoid)
    | named c i =>
      have h := of_decide_eq_true (hm : decide ((witness (.named c i) avoid).named = some (d, j)) = true)
      rw [witness, Placement.named_ofNamed] at h
      exact decide_eq_true h

/-- Zone inclusion decided by subsumption over patterns: the symbolic check holds exactly
when every placement, over every constructor and every identifier, admitted by the first
list is admitted by the second. -/
-- spec: assurance.prove.symbolic-inclusion@e84fedf6
theorem includedIn_iff_placement_inclusion :
    ∀ (small big : List Entry),
      includedIn small big = true ↔ ∀ p : Placement, allowSetOf small p = true → allowSetOf big p = true := by
  intro small big
  constructor
  · intro h p hp
    obtain ⟨e, he, hm⟩ := List.any_eq_true.mp hp
    obtain ⟨a, ha, hs⟩ := List.any_eq_true.mp (List.all_eq_true.mp h e he)
    exact List.any_eq_true.mpr ⟨a, ha, Entry.subsumes_sound a e p hs hm⟩
  · intro h
    refine List.all_eq_true.mpr fun e he => ?_
    let avoid := big.filterMap (fun a => a.named?.map Prod.snd)
    obtain ⟨a, ha, hm⟩ :=
      List.any_eq_true.mp (h (witness e avoid) (List.any_eq_true.mpr ⟨e, he, witness_matches e avoid⟩))
    exact List.any_eq_true.mpr
      ⟨a, ha, subsumes_of_matches_witness a e avoid (fun _ hj => List.mem_filterMap.mpr ⟨a, ha, hj⟩) hm⟩

/-! ## The fail-closed pair -/

/-- The fail-closed allow-set: the pair `local:device` and `on-prem:*`. -/
def failClosed : AllowSet := allowSetOf [.localDevice, .every .onPrem]

/-- An evidence list holding one fail-closed member yields a floor admitting no public-cloud
caller, and the fail-closed allow-set rejects both cloud categories. -/
-- spec: assurance.prove.fail-closed@93adceae
theorem failClosed_floor_rejects_cloud :
    ∀ {es : List AllowSet}, failClosed ∈ es → ∀ id : Ident,
      floor es (Placement.publicCloud id) = false ∧
        failClosed (Placement.privateCloud id) = false ∧ failClosed (Placement.publicCloud id) = false := by
  intro es hmem id
  refine ⟨?_, rfl, rfl⟩
  cases h : floor es (Placement.publicCloud id) with
  | false => rfl
  | true => cases floor_no_downgrade h failClosed hmem

/-! ## The effective policy -/

/-- The meet of two entries. Entry patterns are nested or disjoint, so the meet is the
narrower entry when one subsumes the other, and empty otherwise. -/
def Entry.meet (x y : Entry) : List Entry :=
  if x.subsumes y then [y] else if y.subsumes x then [x] else []

theorem Entry.subsumes_refl : ∀ e : Entry, e.subsumes e = true := by
  intro e
  cases e with
  | any => rfl
  | localDevice => rfl
  | every c => exact decide_eq_true rfl
  | named c i => exact decide_eq_true rfl

/-- Each entry of a meet is subsumed by an entry of each side. -/
theorem Entry.meet_subsumed :
    ∀ (x y e : Entry), e ∈ x.meet y → x.subsumes e = true ∧ y.subsumes e = true := by
  intro x y e he
  cases hxy : x.subsumes y
  · cases hyx : y.subsumes x
    · simp only [Entry.meet, hxy, hyx, Bool.false_eq_true, ↓reduceIte, List.not_mem_nil] at he
    · simp only [Entry.meet, hxy, hyx, Bool.false_eq_true, ↓reduceIte, List.mem_singleton] at he
      subst he; exact ⟨Entry.subsumes_refl _, hyx⟩
  · simp only [Entry.meet, hxy, ↓reduceIte, List.mem_singleton] at he
    subst he; exact ⟨hxy, Entry.subsumes_refl _⟩

/-- The effective policy of two entry lists: the pairwise meet of their entries. -/
def effective (a b : List Entry) : List Entry :=
  a.flatMap (fun x => b.flatMap (fun y => x.meet y))

/-- An effective policy computed from two allow-sets is included in both. -/
-- spec: assurance.prove.effective-policy@b969416d
theorem effective_included_in_both :
    ∀ (a b : List Entry),
      AllowSet.Included (allowSetOf (effective a b)) (allowSetOf a) ∧
        AllowSet.Included (allowSetOf (effective a b)) (allowSetOf b) := by
  intro a b
  have side : ∀ (big : List Entry), (∀ e ∈ effective a b, ∃ s ∈ big, s.subsumes e = true) →
      AllowSet.Included (allowSetOf (effective a b)) (allowSetOf big) := fun big h =>
    (includedIn_iff_placement_inclusion _ big).mp
      (List.all_eq_true.mpr fun e he => List.any_eq_true.mpr (h e he))
  refine ⟨side a fun e he => ?_, side b fun e he => ?_⟩ <;>
    obtain ⟨x, hx, he⟩ := List.mem_flatMap.mp he <;>
    obtain ⟨y, hy, he⟩ := List.mem_flatMap.mp he
  · exact ⟨x, hx, (Entry.meet_subsumed x y e he).1⟩
  · exact ⟨y, hy, (Entry.meet_subsumed x y e he).2⟩
