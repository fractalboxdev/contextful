import Contextful.Placement

/-!
# Zone parsing

Decodes a zone string into `Placement` and an allow-set entry into `Entry`; `allowSetOf`
and `failClosed` decide admission. A zone string parses, after trimming, as `local:device` or
a category and an identifier; anything else is undeclared. Trimming removes the characters
Unicode's `White_Space` property lists from both ends. An identifier is 1 to 128 characters
of ASCII letters, digits, `.`, `_` and `-`.
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

def parseCategory : String → Option Category
  | "on-prem" => some .onPrem
  | "private-cloud" => some .privateCloud
  | "public-cloud" => some .publicCloud
  | _ => none

def categoryName : Category → String
  | .onPrem => "on-prem"
  | .privateCloud => "private-cloud"
  | .publicCloud => "public-cloud"

/-- An identifier: 1 to 128 characters of `[A-Za-z0-9._-]`. -/
def isIdentifier (s : String) : Bool :=
  !s.isEmpty && s.length ≤ 128
    && s.toList.all (fun c => (c.isAlphanum && c.toNat < 128) || c == '.' || c == '_' || c == '-')

/-- The text before the first `:` and the text after it. -/
def splitColon (s : String) : Option (String × String) :=
  match s.toList.span (· != ':') with
  | (before, ':' :: after) => some (String.ofList before, String.ofList after)
  | _ => none

def parseZone (s : String) : Placement :=
  let t := trimWhite s
  if t == "local:device" then .localDevice
  else match splitColon t with
    | some (cat, id) =>
      match parseCategory cat with
      | some c => if isIdentifier id then .ofNamed c id.toList else .undeclared
      | none => .undeclared
    | none => .undeclared

def zoneLabel (z : Placement) : String :=
  if z.isLocal then "local:device"
  else match z.named with
    | some (c, id) => categoryName c ++ ":" ++ String.ofList id
    | none => "undeclared"

/-- Two placements are the same zone: both `local:device`, both undeclared, or the same
category and identifier. -/
def sameZone (a b : Placement) : Bool := a.isLocal == b.isLocal && decide (a.named = b.named)

/-- Parse an entry; `none` when it matches no entry form. -/
def parseEntry (s : String) : Option Entry :=
  let t := trimWhite s
  if t == "*" then some .any
  else if t == "local:device" then some .localDevice
  else match splitColon t with
    | some (cat, id) =>
      match parseCategory cat with
      | some c => if id == "*" then some (.every c) else if isIdentifier id then some (.named c id.toList) else none
      | none => none
    | none => none

/-- The session zone, or the refusal naming why it does not resolve. An asserted zone
stands only where it equals the signed one; incognito pins the session to the
fail-closed pair, an absent signed zone resolving to `local:device`. -/
def sessionZone (asserted signed : Option String) (incognito : Bool) : Except String Placement :=
  let signedZone := signed.map parseZone
  let assertion : Except String Unit :=
    match asserted with
    | some a =>
      if signedZone.any (sameZone (parseZone a)) then pure () else throw "EnforceZoneAssertionWidens"
    | none => pure ()
  assertion.bind fun _ =>
    if !incognito then pure (signedZone.getD .undeclared)
    else match signedZone with
      | none => pure .localDevice
      | some z => if failClosed z then pure z else throw "EnforceIncognitoWidening"

end Reference
