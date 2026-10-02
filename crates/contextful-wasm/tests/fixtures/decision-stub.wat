;; A stand-in decision module: the decision-module ABI, deciding every case `covered`.
;; Every case lands at offset 1024, since the host frees each region before the next call.
;; `decision-stub.wasm` is this file after
;; `wasm-tools parse decision-stub.wat -o decision-stub.wasm`.
(module
  (memory (export "memory") 1)
  (data (i32.const 0) "{\"verdict\":\"covered\",\"error\":null,\"dimension\":null}")
  (func (export "contextful_alloc") (param i32) (result i32)
    (i32.const 1024))
  (func (export "contextful_free") (param i32 i32))
  (func (export "contextful_decide") (param i32 i32) (result i64)
    (i64.const 51)))
