import Reference

/-! The reference binary: one case on standard input, one decision on standard output.
`contextful-reference ops` prints every operation a case may name, one per line. -/

partial def readAll (h : IO.FS.Stream) (acc : ByteArray) : IO ByteArray := do
  let chunk ← h.read 65536
  if chunk.isEmpty then pure acc else readAll h (acc ++ chunk)

def main (args : List String) : IO UInt32 := do
  match args with
  | ["ops"] =>
    for op in Reference.ops do IO.println op
    return 0
  | [] =>
    let input ← readAll (← IO.getStdin) ByteArray.empty
    IO.println (Reference.runBytes input).toJson.compress
    return 0
  | _ =>
    IO.eprintln "usage: contextful-reference [ops]"
    return 2
