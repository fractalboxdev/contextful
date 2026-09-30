//! The host for the decision module's `wasm32-unknown-unknown` build
//! (`assurance.structure-tree.decision-module`): a core module importing nothing and
//! exporting `memory`, `contextful_alloc`, `contextful_decide` and `contextful_free`, as
//! `contextful_core::decide::abi` defines them.

use wasmtime::{Engine, Instance, Memory, Module, Store, TypedFunc};

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
    store: Store<()>,
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
        let mut store = Store::new(&engine, ());
        let instance = Instance::new(&mut store, &module, &[]).map_err(fault("instantiating"))?;
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
