//! The component host: one engine and linker, an epoch ticker arming wall-clock
//! deadlines, and one store per session.
//!
//! A guest's standard library may import the rest of WASI; it links against an empty
//! context — no preopens, no environment, no arguments, no socket grant — so those
//! interfaces exist and grant nothing (`connector.import.empty-context`).

use crate::limits::{Limits, ATTRIBUTION_ENTRIES, ATTRIBUTION_VALUE_BYTES, EPOCH_TICK};
use crate::mediate::{Mediator, Reserve, Traffic};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::package::{guest_config, Artifact, Digest, PinRequirement};
use contextful_core::connector::ConnectorError;
use contextful_core::run::{Failure, FailureTag};
use contextful_runtime::client::HeaderValue;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wasmtime::component::{Component, HasSelf, Instance, Linker, ResourceAny, ResourceTable, TypedFunc};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

mod bindings {
    wasmtime::component::bindgen!({
        world: "contextful:connector/source-connector@1.2.0",
        path: "wit",
        with: {
            "wasi:http": wasmtime_wasi_http::p2::bindings::http,
            "wasi:io": wasmtime_wasi::p2::bindings::sync::io,
            "wasi:clocks": wasmtime_wasi::p2::bindings::clocks,
        },
        require_store_data_send: true,
    });
}

use bindings::contextful::connector::types as wit;
use bindings::SourceConnector;

/// The world a guest's base export answers.
pub const WORLD: &str = "contextful:connector/source-connector@1.2.0";
/// The optional configuration interface, probed on the instance by name.
const CONFIG_INTERFACE: &str = "contextful:connector/config@1.2.0";
/// The optional failure-attribution interface, probed on the instance by name.
const ATTRIBUTION_INTERFACE: &str = "contextful:connector/attribution@1.2.0";

/// A scalar type a field declares (`connector.export.type-taxonomy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    Boolean,
    Int32,
    Int64,
    Float64,
    String,
    Bytes,
    TimestampMillis,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: DataType,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub name: String,
    pub fields: Vec<Field>,
    pub primary_key: Vec<String>,
}

/// How a position advances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    Monotonic,
    OpaqueToken,
    SnapshotId,
}

/// A position: bytes the connector owns, beside its declared kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub kind: CursorKind,
    pub bytes: Vec<u8>,
}

/// What a session grants its guest.
#[derive(Clone)]
pub struct Grant {
    /// The hosts outbound requests may reach.
    pub allow: Allowlist,
    /// Headers the host attaches to every permitted request, overriding a guest header of
    /// the same name. A credential rides here and nowhere the guest reads.
    pub attach: Vec<(String, HeaderValue)>,
    /// The reservation point, when the connector declares a limiter.
    pub gate: Option<Arc<dyn Reserve>>,
}

/// A log line a guest emitted within its session budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub level: &'static str,
    pub context: String,
    pub message: String,
}

struct Ctx {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    table: ResourceTable,
    memory: StoreLimits,
    mediator: Mediator,
    log_left: usize,
    logs: Vec<LogLine>,
    logs_dropped: u64,
}

impl WasiView for Ctx {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl WasiHttpView for Ctx {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView { ctx: &mut self.http, table: &mut self.table, hooks: &mut self.mediator }
    }
}

impl bindings::wasi::logging::logging::Host for Ctx {
    /// A message past the session budget is dropped and counted; logging never blocks.
    fn log(&mut self, level: bindings::wasi::logging::logging::Level, context: String, message: String) {
        use bindings::wasi::logging::logging::Level;
        let cost = context.len() + message.len();
        if cost > self.log_left {
            self.log_left = 0;
            self.logs_dropped += 1;
            return;
        }
        self.log_left -= cost;
        let level = match level {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Critical => "critical",
        };
        self.logs.push(LogLine { level, context, message });
    }
}

/// Advances the engine epoch every [`EPOCH_TICK`]. Each session holds a handle, so a
/// session outliving its host keeps its deadlines.
struct Ticker {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Ticker {
    fn spawn(engine: Engine) -> Result<Ticker, Failure> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("connector-epoch".into())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    std::thread::sleep(EPOCH_TICK);
                    engine.increment_epoch();
                }
            })
            .map_err(|e| Failure::new(FailureTag::Config, format!("starting the epoch ticker: {e}")))?;
        Ok(Ticker { stop, thread: Some(thread) })
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A compiled component, reusable across sessions.
pub struct Connector {
    component: Component,
}

/// The engine every session of the process runs on.
pub struct ComponentHost {
    engine: Engine,
    linker: Linker<Ctx>,
    ticker: Arc<Ticker>,
}

fn load_failure(e: impl std::fmt::Display) -> Failure {
    Failure::deterministic(FailureTag::Config, format!("the component does not load: {e}"))
}

impl ComponentHost {
    pub fn new() -> Result<ComponentHost, Failure> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(load_failure)?;
        let mut linker: Linker<Ctx> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(load_failure)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_sync(&mut linker).map_err(load_failure)?;
        bindings::wasi::logging::logging::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c).map_err(load_failure)?;
        let ticker = Arc::new(Ticker::spawn(engine.clone())?);
        Ok(ComponentHost { engine, linker, ticker })
    }

    /// Compile `wasm` as a component. Core modules and malformed bytes refuse.
    pub fn load(&self, wasm: &[u8]) -> Result<Connector, Failure> {
        Ok(Connector { component: Component::new(&self.engine, wasm).map_err(load_failure)? })
    }

    /// Admit resolved artifact bytes against their reference's pin, then compile them.
    /// Bytes that fail the pin never reach the compiler (`connector.package.digest-mismatch`).
    pub fn load_artifact(&self, artifact: &Artifact, wasm: &[u8], requirement: PinRequirement) -> Result<(Connector, Digest), Failure> {
        let digest = artifact.admit(wasm, requirement).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        Ok((self.load(wasm)?, digest))
    }

    /// Instantiate a session. `config` is the pipeline's guest table: checked ahead of
    /// any I/O, then delivered once, ahead of discovery (`connector.import.forwarded-config`).
    pub fn open(&self, connector: &Connector, grant: Grant, limits: &Limits, config: Option<&serde_json::Value>) -> Result<Session, Failure> {
        let refuse = |e: ConnectorError| Failure::deterministic(FailureTag::Config, e.to_string());
        let config = config.map(guest_config).transpose().map_err(refuse)?;
        if !grant.attach.is_empty() {
            grant.allow.check_bound().map_err(refuse)?;
        }
        let memory = usize::try_from(limits.memory_bytes).unwrap_or(usize::MAX);
        let ctx = Ctx {
            wasi: WasiCtx::builder().build(),
            http: WasiHttpCtx::new(),
            table: ResourceTable::new(),
            memory: StoreLimitsBuilder::new().memory_size(memory).trap_on_grow_failure(true).build(),
            mediator: Mediator::new(grant.allow, grant.attach, grant.gate),
            log_left: limits.log_bytes,
            logs: Vec::new(),
            logs_dropped: 0,
        };
        let mut store = Store::new(&self.engine, ctx);
        store.limiter(|c| &mut c.memory);
        store.epoch_deadline_trap();
        arm(&mut store, limits.discovery_deadline);
        let instance = self.linker.instantiate(&mut store, &connector.component).map_err(|e| trapped(e, "instantiation"))?;
        let guest = SourceConnector::new(&mut store, &instance).map_err(load_failure)?;
        let mut session = Session { store, guest, instance, handle: None, limits: limits.clone(), _ticker: self.ticker.clone() };
        if let Some(table) = config {
            let Some(configure) = session.config_func() else {
                return Err(refuse(ConnectorError::ConnectorConfigUnclaimed(format!(
                    "a guest table is declared and the guest exports no `{CONFIG_INTERFACE}`"
                ))));
            };
            let deadline = session.limits.discovery_deadline;
            session.call(deadline, move |s| configure.call(&mut s.store, (table,)).map(|(r,)| r))?.map_err(guest_error)?;
        }
        Ok(session)
    }
}

fn arm(store: &mut Store<Ctx>, deadline: Duration) {
    let ticks = (deadline.as_millis() / EPOCH_TICK.as_millis()).max(1);
    store.set_epoch_deadline(u64::try_from(ticks).unwrap_or(u64::MAX));
}

/// A trap is a bound hit or a guest fault, and either is transient
/// (`run.retry.bound-hit-is-transient`).
fn trapped(e: wasmtime::Error, during: &str) -> Failure {
    let why = match e.downcast_ref::<Trap>() {
        Some(Trap::Interrupt) => "ran past its call deadline".to_string(),
        Some(other) => format!("trapped: {other}"),
        None => format!("trapped: {e:#}"),
    };
    Failure::new(FailureTag::Transient, format!("the guest {why} during {during}"))
}

fn guest_error(e: wit::Error) -> Failure {
    match e {
        wit::Error::Transient(m) => Failure::new(FailureTag::Transient, m),
        wit::Error::Permanent(m) => Failure::new(FailureTag::Permanent, m),
        wit::Error::AuthExpired(m) => Failure::new(FailureTag::AuthExpired, m),
        wit::Error::RateLimited(secs) => Failure::new(FailureTag::RateLimited, "the guest was rate limited").with_retry_after(u64::from(secs)),
        wit::Error::SchemaIncompatible(m) => Failure::new(FailureTag::SchemaIncompatible, m),
    }
}

fn kind_in(k: wit::CursorKind) -> CursorKind {
    match k {
        wit::CursorKind::Monotonic => CursorKind::Monotonic,
        wit::CursorKind::OpaqueToken => CursorKind::OpaqueToken,
        wit::CursorKind::SnapshotId => CursorKind::SnapshotId,
    }
}

fn kind_out(k: CursorKind) -> wit::CursorKind {
    match k {
        CursorKind::Monotonic => wit::CursorKind::Monotonic,
        CursorKind::OpaqueToken => wit::CursorKind::OpaqueToken,
        CursorKind::SnapshotId => wit::CursorKind::SnapshotId,
    }
}

fn data_type(t: wit::DataType) -> DataType {
    match t {
        wit::DataType::Boolean => DataType::Boolean,
        wit::DataType::Int32 => DataType::Int32,
        wit::DataType::Int64 => DataType::Int64,
        wit::DataType::Float64 => DataType::Float64,
        wit::DataType::String => DataType::String,
        wit::DataType::Bytes => DataType::Bytes,
        wit::DataType::TimestampMillis => DataType::TimestampMillis,
        wit::DataType::Json => DataType::Json,
    }
}

/// One instantiated guest and the read it holds open.
pub struct Session {
    store: Store<Ctx>,
    guest: SourceConnector,
    instance: Instance,
    handle: Option<ResourceAny>,
    limits: Limits,
    _ticker: Arc<Ticker>,
}

impl Session {
    /// Run one guest call under a fresh deadline. A request the mediation point refused
    /// or held back during the call fails it, whatever the guest made of the refusal.
    fn call<T>(&mut self, deadline: Duration, f: impl FnOnce(&mut Self) -> wasmtime::Result<T>) -> Result<T, Failure> {
        arm(&mut self.store, deadline);
        let before = self.traffic();
        let out = f(self).map_err(|e| trapped(e, "a call"))?;
        let after = self.traffic();
        if let Some(host) = after.denied.get(before.denied.len()) {
            return Err(Failure::deterministic(
                FailureTag::Config,
                ConnectorError::SecretUnpermittedRequest(format!("the guest reached `{host}`, a host the declaration does not cover")).to_string(),
            ));
        }
        if let Some(why) = after.refused.get(before.refused.len()) {
            return Err(Failure::deterministic(FailureTag::Config, why.clone()));
        }
        if let Some(why) = after.held_back.get(before.held_back.len()) {
            return Err(Failure::new(FailureTag::Transient, ConnectorError::ConnectorUnmetered(format!("a request went unreserved: {why}")).to_string()));
        }
        Ok(out)
    }

    fn read_deadline(&self) -> Duration {
        self.limits.read_deadline
    }

    fn discovery_deadline(&self) -> Duration {
        self.limits.discovery_deadline
    }

    fn config_func(&mut self) -> Option<TypedFunc<(String,), (Result<(), wit::Error>,)>> {
        let instance = self.instance;
        let (_, iface) = instance.get_export(&mut self.store, None, CONFIG_INTERFACE)?;
        let (_, func) = instance.get_export(&mut self.store, Some(&iface), "configure")?;
        instance.get_typed_func(&mut self.store, &func).ok()
    }

    fn attribution_func(&mut self) -> Option<TypedFunc<(), (Vec<String>,)>> {
        let instance = self.instance;
        let (_, iface) = instance.get_export(&mut self.store, None, ATTRIBUTION_INTERFACE)?;
        let (_, func) = instance.get_export(&mut self.store, Some(&iface), "failed-partitions")?;
        instance.get_typed_func(&mut self.store, &func).ok()
    }

    /// Whether the guest exports the configuration interface.
    pub fn accepts_config(&mut self) -> bool {
        self.config_func().is_some()
    }

    /// Whether the guest exports the failure-attribution interface.
    pub fn attributes_failures(&mut self) -> bool {
        self.attribution_func().is_some()
    }

    pub fn cursor_kind(&mut self) -> Result<CursorKind, Failure> {
        let d = self.discovery_deadline();
        self.call(d, |s| s.guest.contextful_connector_source().call_cursor_kind(&mut s.store)).map(kind_in)
    }

    pub fn discover(&mut self) -> Result<Vec<Schema>, Failure> {
        let d = self.discovery_deadline();
        let schemas = self.call(d, |s| s.guest.contextful_connector_source().call_discover(&mut s.store))?.map_err(guest_error)?;
        Ok(schemas
            .into_iter()
            .map(|s| Schema {
                name: s.name,
                fields: s.fields.into_iter().map(|f| Field { name: f.name, ty: data_type(f.ty), nullable: f.nullable }).collect(),
                primary_key: s.primary_key,
            })
            .collect())
    }

    /// Open a read against `table` from `from`, replacing any read the session holds.
    pub fn open(&mut self, table: &str, from: Option<&Cursor>) -> Result<(), Failure> {
        self.close();
        let from = from.map(|c| wit::Cursor { kind: kind_out(c.kind), bytes: c.bytes.clone() });
        let d = self.read_deadline();
        let handle = self.call(d, |s| s.guest.contextful_connector_source().call_open(&mut s.store, table, from.as_ref()))?.map_err(guest_error)?;
        self.handle = Some(handle);
        Ok(())
    }

    fn opened(&self) -> Result<ResourceAny, Failure> {
        self.handle.ok_or_else(|| Failure::new(FailureTag::Permanent, "a read was asked of a session holding no open read"))
    }

    /// The next batch as Arrow IPC bytes, or `None` once the read is exhausted.
    pub fn next(&mut self) -> Result<Option<Vec<u8>>, Failure> {
        let handle = self.opened()?;
        let d = self.read_deadline();
        self.call(d, |s| s.guest.contextful_connector_source().read_handle().call_next(&mut s.store, handle))?.map_err(guest_error)
    }

    /// Where the open read stands after the last batch.
    pub fn position(&mut self) -> Result<Cursor, Failure> {
        let handle = self.opened()?;
        let d = self.read_deadline();
        let c = self.call(d, |s| s.guest.contextful_connector_source().read_handle().call_position(&mut s.store, handle))?;
        Ok(Cursor { kind: kind_in(c.kind), bytes: c.bytes })
    }

    /// The partition values the session's failed reads belong to, empty for a guest
    /// exporting no attribution. A value over [`ATTRIBUTION_VALUE_BYTES`] and entries past
    /// [`ATTRIBUTION_ENTRIES`] are dropped and stay unattributed; the rest keep their bytes.
    pub fn failed_partitions(&mut self) -> Result<Vec<String>, Failure> {
        let Some(func) = self.attribution_func() else {
            return Ok(Vec::new());
        };
        let d = self.read_deadline();
        let mut values = self.call(d, move |s| func.call(&mut s.store, ()).map(|(v,)| v))?;
        values.retain(|v| v.len() <= ATTRIBUTION_VALUE_BYTES);
        values.truncate(ATTRIBUTION_ENTRIES);
        Ok(values)
    }

    /// What the mediation point did during the session.
    pub fn traffic(&self) -> Traffic {
        self.store.data().mediator.traffic.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// Log lines kept within the session budget.
    pub fn logs(&self) -> &[LogLine] {
        &self.store.data().logs
    }

    /// Log messages dropped past the session budget.
    pub fn logs_dropped(&self) -> u64 {
        self.store.data().logs_dropped
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            // The handle's destructor runs guest code; it gets a fresh deadline, and the
            // table entry goes whether or not it traps.
            let d = self.read_deadline();
            arm(&mut self.store, d);
            let _ = handle.resource_drop(&mut self.store);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}
