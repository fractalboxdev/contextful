import Protocol.Merge

/-!
# The merge model's replay

`protocol merge run` reads one merge case per line and prints the merged keys. A case is
`me=<n> now=<n>` followed by one ` ; `-separated group per key:
`<log> <held> <verified> <local> <remote> <tomb>`, the flags `0` or `1`, an entry `-` or
`<sha>:<owner>`, a tombstone `-` or `<owner>@<deleted at>`. The output is `conflict`, or one
` ; `-separated group per key: `<entry> <tomb> <refusal>`, the refusal `-`, `foreign` or
`unverified`.
-/

namespace Protocol.Merge

def parseFlag : String → Option Bool
  | "0" => some false
  | "1" => some true
  | _ => none

def parseEntry (s : String) : Option (Option Entry) :=
  if s == "-" then some none
  else match s.splitOn ":" with
    | [sha, owner] => do some (some ⟨← sha.toNat?, ← owner.toNat?⟩)
    | _ => none

def parseTomb (s : String) : Option (Option Tombstone) :=
  if s == "-" then some none
  else match s.splitOn "@" with
    | [owner, at_] => do some (some ⟨← owner.toNat?, ← at_.toNat?⟩)
    | _ => none

def parseInput (group : String) : Option Input :=
  match (group.splitOn " ").filter (· ≠ "") with
  | [log, held, verified, l, r, t] => do
    some ⟨← parseFlag log, ← parseFlag held, ← parseFlag verified, ← parseEntry l, ← parseEntry r, ← parseTomb t⟩
  | _ => none

def parseHeader (s : String) (key : String) : Option Nat :=
  (s.dropPrefix? (key ++ "=")).bind fun rest => rest.toString.toNat?

/-- A case: the writer, the instant and each key's inputs. -/
def parseCase (line : String) : Option (Nat × Nat × List Input) :=
  match line.splitOn " ; " with
  | [] => none
  | head :: groups =>
    match (head.splitOn " ").filter (· ≠ "") with
    | me :: now :: first => do
      let me ← parseHeader me "me"
      let now ← parseHeader now "now"
      let inputs ← ((" ".intercalate first) :: groups).mapM parseInput
      some (me, now, inputs)
    | _ => none

def showEntry : Option Entry → String
  | none => "-"
  | some e => s!"{e.sha}:{e.owner}"

def showTomb : Option Tombstone → String
  | none => "-"
  | some t => s!"{t.owner}@{t.deletedAt}"

def showRefusal : Option Refusal → String
  | none => "-"
  | some .foreign => "foreign"
  | some .unverified => "unverified"

def showMerge : Option (List Output) → String
  | none => "conflict"
  | some os => " ; ".intercalate (os.map fun o => s!"{showEntry o.entry} {showTomb o.tomb} {showRefusal o.refusal}")

partial def runLoop (stdin : IO.FS.Stream) : IO UInt32 := do
  let line ← stdin.getLine
  if line.isEmpty then return 0
  let text := line.trimAscii.toString
  if text.isEmpty then return (← runLoop stdin)
  match parseCase text with
  | none =>
    IO.eprintln s!"unrecognised merge case: {text}"
    return 2
  | some (me, now, inputs) =>
    IO.println (showMerge (merge me now inputs))
    runLoop stdin

/-- `merge run`. -/
def main (args : List String) : IO UInt32 := do
  match args with
  | ["run"] => runLoop (← IO.getStdin)
  | _ =>
    IO.eprintln "usage: protocol merge run"
    return 2

end Protocol.Merge
