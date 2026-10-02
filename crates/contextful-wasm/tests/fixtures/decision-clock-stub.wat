;; A stand-in decision module importing what the credential library's WebAssembly build
;; imports: its evaluator clock, which the host answers, and wasm-bindgen's description
;; and reference-table hooks, which the module never calls. It decides every case
;; `covered` once the clock reads at or past zero, and traps otherwise.
;; `decision-clock-stub.wasm` is this file after
;; `wasm-tools parse decision-clock-stub.wat -o decision-clock-stub.wasm`.
(module
  (import "__wbindgen_placeholder__" "__wbg_performance_now_0123456789abcdef" (func $now (result f64)))
  (import "__wbindgen_placeholder__" "__wbindgen_describe" (func (param i32)))
  (import "__wbindgen_externref_xform__" "__wbindgen_externref_table_grow" (func (param i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{\"verdict\":\"covered\",\"error\":null,\"dimension\":null}")
  (func (export "contextful_alloc") (param i32) (result i32)
    (i32.const 1024))
  (func (export "contextful_free") (param i32 i32))
  (func (export "contextful_decide") (param i32 i32) (result i64)
    (if (f64.lt (call $now) (f64.const 0)) (then unreachable))
    (i64.const 51)))
