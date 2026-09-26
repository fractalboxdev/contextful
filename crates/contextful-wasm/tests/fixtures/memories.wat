;; Three linear memories across two core instances: 1000 + 1000 pages in one
;; multi-memory module and 2500 pages in another, 281.25 MiB in all. Each memory sits
;; under a 256 MiB cap and their sum sits over it. `memories.wasm` is this file after
;; `wasm-tools parse memories.wat -o memories.wasm`.
(component
  (core module $pair
    (memory 1000)
    (memory 1000))
  (core module $one
    (memory 2500))
  (core instance (instantiate $pair))
  (core instance (instantiate $one)))
