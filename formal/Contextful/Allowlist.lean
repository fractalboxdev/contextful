/-!
# Connector host containment

Names are reversed character lists after the outbound matcher's normalization. A wildcard
stores the reversed domain followed by a dot; a proper prefix excludes its apex. Parsing,
DNS, credential scope meaning and atomic manifest publication lie outside this model.
-/
-- mirrors: connector.declare-capability.host-allowlist
namespace Allowlist

abbrev Name := List Char

inductive Entry where
  | exact (host : Name)
  | subdomains (stem : Name)
  deriving DecidableEq

def Entry.covers : Entry → Name → Bool
  | .exact h, n => decide (h = n)
  | .subdomains p, n => decide (p <+: n ∧ p.length < n.length)

def Entry.subsumes : Entry → Entry → Bool
  | .exact p, .exact c => decide (p = c)
  | .exact _, .subdomains _ => false
  | .subdomains p, .exact c => decide (p <+: c ∧ p.length < c.length)
  | .subdomains p, .subdomains c => decide (p <+: c)

def permits (entries : List Entry) (host : Name) : Bool :=
  entries.any (fun e => e.covers host)

def includedIn (candidate predecessor : List Entry) : Bool :=
  candidate.all (fun c => predecessor.any (fun p => p.subsumes c))

theorem Entry.subsumes_sound (p c : Entry) (host : Name)
    (hs : p.subsumes c = true) (hc : c.covers host = true) : p.covers host = true := by
  cases p with
  | exact p =>
    cases c with
    | exact c =>
      have he : p = c := of_decide_eq_true hs
      subst c
      exact hc
    | subdomains c => cases hs
  | subdomains p =>
    cases c with
    | exact c =>
      have he : c = host := of_decide_eq_true hc
      subst host
      exact hs
    | subdomains c =>
      have hp : p <+: c := of_decide_eq_true hs
      have hh : c <+: host ∧ c.length < host.length := of_decide_eq_true hc
      exact decide_eq_true ⟨hp.trans hh.1, Nat.lt_of_le_of_lt hp.length_le hh.2⟩

/-- Successful syntactic inclusion preserves every permitted host. -/
-- spec: connector.widen.host-inclusion@f6a2edfe
theorem allowlist_includedIn_sound (candidate predecessor : List Entry)
    (included : includedIn candidate predecessor = true) (host : Name)
    (admitted : permits candidate host = true) : permits predecessor host = true := by
  obtain ⟨c, hc, ha⟩ := List.any_eq_true.mp admitted
  obtain ⟨p, hp, hs⟩ := List.any_eq_true.mp (List.all_eq_true.mp included c hc)
  exact List.any_eq_true.mpr ⟨p, hp, Entry.subsumes_sound p c host hs ha⟩

/-- Witness publication checks the concrete matcher on both sides. -/
def witness (candidate predecessor : List Entry) (probes : List Name) : Option Name :=
  probes.find? (fun h => permits candidate h && !permits predecessor h)

/-- A published witness is newly permitted, regardless of probe generation. -/
-- spec: connector.widen.host-witness@e13c9fd4
theorem widen_witness_admitted (candidate predecessor : List Entry) (probes : List Name)
    (host : Name) (found : witness candidate predecessor probes = some host) :
    permits candidate host = true ∧ permits predecessor host = false := by
  have h := List.find?_some found
  simpa only [Bool.and_eq_true, Bool.not_eq_true'] using h

end Allowlist
