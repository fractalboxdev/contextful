//! The host for the decision module's `wasm32-unknown-unknown` build
//! (`assurance.structure-tree.decision-module`): a core module exporting `memory`,
//! `contextful_alloc`, `contextful_decide` and `contextful_free`, as
//! `contextful_policy::decide::abi` defines them.
//!
//! The module imports the credential library's evaluator clock, which the host answers
//! with milliseconds since the module loaded, and wasm-bindgen's description and
//! reference-table hooks, which only a JavaScript binding generator calls; the host links
//! each hook to a trap. Any other import refuses the module at load.

use std::time::Instant;
use wasmtime::{Engine, ExternType, Linker, Memory, Module, Store, TypedFunc, ValType};

/// The import module holding the clock and the description hook.
const BINDGEN_MODULE: &str = "__wbindgen_placeholder__";
/// The import module holding the reference-table hooks.
const EXTERNREF_MODULE: &str = "__wbindgen_externref_xform__";
/// The clock import's name, before the library's per-release hash.
const CLOCK_PREFIX: &str = "__wbg_performance_now_";

/// A decision module that does not load, or a call into it that fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionModuleError(pub String);

impl std::fmt::Display for DecisionModuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "decision module: {}", self.0)
    }
}

impl std::error::Error for DecisionModuleError {}

fn fault(what: &str) -> impl Fn(wasmtime::Error) -> DecisionModuleError + '_ {
    move |e| DecisionModuleError(format!("{what}: {e:#}"))
}

/// One instantiated decision module.
pub struct DecisionModule {
    store: Store<Instant>,
    memory: Memory,
    alloc: TypedFunc<u32, u32>,
    decide: TypedFunc<(u32, u32), u64>,
    free: TypedFunc<(u32, u32), ()>,
}

impl DecisionModule {
    /// Compile and instantiate a module's bytes.
    pub fn load(bytes: &[u8]) -> Result<DecisionModule, DecisionModuleError> {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes).map_err(fault("compiling"))?;
        let mut linker = Linker::new(&engine);
        for import in module.imports() {
            let (from, name) = (import.module(), import.name());
            let ExternType::Func(ty) = import.ty() else {
                return Err(DecisionModuleError(format!("the module imports `{from}::{name}`, which is no function")));
            };
            let clock = from == BINDGEN_MODULE && name.starts_with(CLOCK_PREFIX);
            if clock && ty.params().len() == 0 && ty.results().map(|r| matches!(r, ValType::F64)).eq([true]) {
                linker
                    .func_wrap(from, name, |caller: wasmtime::Caller<'_, Instant>| caller.data().elapsed().as_secs_f64() * 1000.0)
                    .map_err(fault("linking the clock"))?;
            } else if from == BINDGEN_MODULE || from == EXTERNREF_MODULE {
                let hook = format!("{from}::{name}");
                linker
                    .func_new(from, name, ty, move |_, _, _| Err(wasmtime::Error::msg(format!("the module called `{hook}`"))))
                    .map_err(fault("linking a hook"))?;
            } else {
                return Err(DecisionModuleError(format!("the module imports `{from}::{name}`, which the host does not provide")));
            }
        }
        let mut store = Store::new(&engine, Instant::now());
        let instance = linker.instantiate(&mut store, &module).map_err(fault("instantiating"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| DecisionModuleError("the module exports no `memory`".into()))?;
        let alloc = instance.get_typed_func(&mut store, "contextful_alloc").map_err(fault("`contextful_alloc`"))?;
        let decide = instance.get_typed_func(&mut store, "contextful_decide").map_err(fault("`contextful_decide`"))?;
        let free = instance.get_typed_func(&mut store, "contextful_free").map_err(fault("`contextful_free`"))?;
        Ok(DecisionModule { store, memory, alloc, decide, free })
    }

    /// The decision JSON the module prints for one case text.
    pub fn decide(&mut self, case: &[u8]) -> Result<Vec<u8>, DecisionModuleError> {
        let len = u32::try_from(case.len()).map_err(|_| DecisionModuleError("a case past 4 GiB".into()))?;
        let at = self.alloc.call(&mut self.store, len).map_err(fault("`contextful_alloc`"))?;
        self.memory
            .write(&mut self.store, at as usize, case)
            .map_err(|e| DecisionModuleError(format!("writing the case: {e}")))?;
        let packed = self.decide.call(&mut self.store, (at, len)).map_err(fault("`contextful_decide`"))?;
        self.free.call(&mut self.store, (at, len)).map_err(fault("`contextful_free`"))?;
        let (out, out_len) = ((packed >> 32) as u32, packed as u32);
        let mut decision = vec![0u8; out_len as usize];
        self.memory
            .read(&self.store, out as usize, &mut decision)
            .map_err(|e| DecisionModuleError(format!("reading the decision: {e}")))?;
        self.free.call(&mut self.store, (out, out_len)).map_err(fault("`contextful_free`"))?;
        Ok(decision)
    }
}
