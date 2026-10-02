/-!
# Zone placement

Rendered from `authority.place.zone-string`, `authority.place.allow-set-entry`,
`authority.place.unparsed-pattern`, `authority.place.disjunctive`,
`authority.place.undeclared`, `authority.place.asserted-zone`,
`authority.place.fail-closed`, `authority.place.incognito`,
`authority.place.incognito-widening` and `authority.place.identifier-length`. Written
from the specification text alone.

A zone string parses, after trimming, as `local:device` or a category and an identifier;
anything else is undeclared. Trimming removes the characters Unicode's `White_Space`
property lists from both ends. An identifier is 1 to 128 characters of ASCII letters,
digits, `.`, `_` and `-`.
-/

namespace Reference

/-- The characters carrying Unicode's `White_Space` property. -/
def isWhiteSpace (c : Char) : Bool :=
  let n := c.toNat
  (0x09 ≤ n && n ≤ 0x0D) || n == 0x20 || n == 0x85 || n == 0xA0 || n == 0x1680
    || (0x2000 ≤ n && n ≤ 0x200A) || n == 0x2028 || n == 0x2029 || n == 0x202F
    || n == 0x205F || n == 0x3000

/-- The string with `White_Space` characters removed from both ends. -/
def trimWhite (s : String) : String :=
  String.ofList ((s.toList.dropWhile isWhiteSpace).reverse.dropWhile isWhiteSpace).reverse

/-- The three categories carrying an identifier. -/
inductive Category where
  | onPrem | privateCloud | publicCloud
  deriving Repr, DecidableEq

def Category.parse : String → Option Category
  | "on-prem" => some .onPrem
  | "private-cloud" => some .privateCloud
  | "public-cloud" => some .publicCloud
  | _ => none

def Category.name : Category → String
  | .onPrem => "on-prem"
  | .privateCloud => "private-cloud"
  | .publicCloud => "public-cloud"

/-- A zone a calling process declares. -/
inductive Zone where
  | localDevice
  | inCategory (c : Category) (id : String)
  | undeclared
  deriving Repr, DecidableEq

/-- An identifier: 1 to 128 characters of `[A-Za-z0-9._-]`. -/
def isIdentifier (s : String) : Bool :=
  !s.isEmpty && s.length ≤ 128
    && s.toList.all (fun c => (c.isAlphanum && c.toNat < 128) || c == '.' || c == '_' || c == '-')

/-- The text before the first `:` and the text after it. -/
def splitColon (s : String) : Option (String × String) :=
  match s.toList.span (· != ':') with
  | (before, ':' :: after) => some (String.ofList before, String.ofList after)
  | _ => none

def Zone.parse (s : String) : Zone :=
  let t := trimWhite s
  if t == "local:device" then .localDevice
  else match splitColon t with
    | some (cat, id) =>
      match Category.parse cat with
      | some c => if isIdentifier id then .inCategory c id else .undeclared
      | none => .undeclared
    | none => .undeclared

def Zone.label : Zone → String
  | .localDevice => "local:device"
  | .inCategory c id => c.name ++ ":" ++ id
  | .undeclared => "undeclared"

/-- One allow-set entry. -/
inductive Entry where
  | any
  | localDevice
  | category (c : Category)
  | exact (c : Category) (id : String)
  deriving Repr, DecidableEq

/-- Parse an entry; `none` when it matches no entry form. -/
def Entry.parse (s : String) : Option Entry :=
  let t := trimWhite s
  if t == "*" then some .any
  else if t == "local:device" then some .localDevice
  else match splitColon t with
    | some (cat, id) =>
      match Category.parse cat with
      | some c => if id == "*" then some (.category c) else if isIdentifier id then some (.exact c id) else none
      | none => none
    | none => none

/-- A bare category matches every identifier in it; an identifier matches itself alone;
only `*` matches an undeclared zone. -/
def Entry.matches : Entry → Zone → Bool
  | .any, _ => true
  | .localDevice, .localDevice => true
  | .category c, .inCategory z _ => c == z
  | .exact c id, .inCategory z zid => c == z && id == zid
  | _, _ => false

/-- A zone is admitted when any entry matches. -/
def admits (set : List Entry) (z : Zone) : Bool :=
  set.any (·.matches z)

/-- The fail-closed pair `local:device` and `on-prem:*`. -/
def failClosed : List Entry := [.localDevice, .category .onPrem]

/-- The session zone, or the refusal naming why it does not resolve. An asserted zone
stands only where it equals the signed one; incognito pins the session to the
fail-closed pair, an absent signed zone resolving to `local:device`. -/
def sessionZone (asserted signed : Option String) (incognito : Bool) : Except String Zone :=
  let signedZone := signed.map Zone.parse
  let assertion : Except String Unit :=
    match asserted with
    | some a => if signedZone == some (Zone.parse a) then pure () else throw "EnforceZoneAssertionWidens"
    | none => pure ()
  assertion.bind fun _ =>
    if !incognito then pure (signedZone.getD .undeclared)
    else match signedZone with
      | none => pure .localDevice
      | some z => if admits failClosed z then pure z else throw "EnforceIncognitoWidening"

end Reference
