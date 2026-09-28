//! The component host: one engine and linker, an epoch ticker, and one store per session.
//!
//! Every guest call runs on a fiber under a host-side timeout at its wall-clock deadline,
//! so a guest blocked inside a host import — a clock wait, a vendor exchange, a
//! reservation — returns at the deadline. The epoch deadline stays armed beside it for
//! guest code that computes without yielding.
//!
//! A guest's standard library may import the rest of WASI; it links against an empty
//! context — no preopens, no environment, no arguments, sockets and name lookup denied —
//! so those interfaces exist and grant nothing (`connector.import.empty-context`).

use crate::limits::{Limits, ATTRIBUTION_ENTRIES, ATTRIBUTION_VALUE_BYTES, EPOCH_TICK};
use crate::mediate::{Mediator, Reserve, Traffic};
use contextful_core::connector::attach::Allowlist;
use contextful_core::connector::package::{guest_config, Artifact, Digest, PinRequirement};
use contextful_core::connector::ConnectorError;
use contextful_core::run::{Failure, FailureTag};
use contextful_outbound::client::HeaderValue;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wasmtime::component::{Component, HasSelf, Instance, InstancePre, Linker, ResourceAny, ResourceTable, TypedFunc};
use wasmtime::{Config, Engine, ResourceLimiter, Store, Trap};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

mod bindings {
    wasmtime::component::bindgen!({
        world: "contextful:connector/source-connector@1.2.0",
        path: "wit",
        exports: { default: async },
        with: {
            "wasi:http": wasmtime_wasi_http::p2::bindings::http,
            "wasi:io": wasmtime_wasi::p2::bindings::io,
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

/// Core instances one session's store creates. A component's shims and adapters each
/// count; a guest built by the standard toolchain uses under ten.
const INSTANCES: usize = 64;
/// Linear memories one session's store creates.
const MEMORIES: usize = 16;
/// Tables one session's store creates.
const TABLES: usize = 64;
/// Table elements across every table of one session.
const TABLE_ELEMENTS: usize = 1 << 20;

type ConfigFunc = TypedFunc<(String,), (Result<(), wit::Error>,)>;
type AttributionFunc = TypedFunc<(), (Vec<String>,)>;

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

/// The store's resource budget. Linear memory is one budget across every memory the
/// component instantiates (`connector.package.linear-memory`), so a component splitting
/// its heap over several memories or core instances gets no more than one would.
struct Budget {
    memory_cap: usize,
    memory_used: usize,
    memory_pending: usize,
    elements_used: usize,
    elements_pending: usize,
}

impl Budget {
    fn new(memory_cap: usize) -> Budget {
        Budget { memory_cap, memory_used: 0, memory_pending: 0, elements_used: 0, elements_pending: 0 }
    }
}

impl ResourceLimiter for Budget {
    fn memory_growing(&mut self, current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        let grow = desired.saturating_sub(current);
        let total = self.memory_used.saturating_add(grow);
        if total > self.memory_cap {
            wasmtime::bail!("the connector's linear memory would reach {total} bytes, over its {} byte cap", self.memory_cap);
        }
        self.memory_used = total;
        self.memory_pending = grow;
        Ok(true)
    }

    fn memory_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        self.memory_used -= std::mem::take(&mut self.memory_pending);
        Ok(())
    }

    fn table_growing(&mut self, current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        let grow = desired.saturating_sub(current);
        let total = self.elements_used.saturating_add(grow);
        if total > TABLE_ELEMENTS {
            wasmtime::bail!("the connector's tables would hold {total} elements, over {TABLE_ELEMENTS}");
        }
        self.elements_used = total;
        self.elements_pending = grow;
        Ok(true)
    }

    fn table_grow_failed(&mut self, _error: wasmtime::Error) -> wasmtime::Result<()> {
        self.elements_used -= std::mem::take(&mut self.elements_pending);
        Ok(())
    }

    fn instances(&self) -> usize {
        INSTANCES
    }

    fn tables(&self) -> usize {
        TABLES
    }

    fn memories(&self) -> usize {
        MEMORIES
    }
}

struct Ctx {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    table: ResourceTable,
    budget: Budget,
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

/// A compiled and linked component, reusable across sessions.
pub struct Connector {
    pre: InstancePre<Ctx>,
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
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(load_failure)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker).map_err(load_failure)?;
        bindings::wasi::logging::logging::add_to_linker::<Ctx, HasSelf<Ctx>>(&mut linker, |c| c).map_err(load_failure)?;
        let ticker = Arc::new(Ticker::spawn(engine.clone())?);
        Ok(ComponentHost { engine, linker, ticker })
    }

    /// Compile `wasm` as a component and resolve its imports. Core modules, malformed
    /// bytes and imports the host does not offer refuse.
    pub fn load(&self, wasm: &[u8]) -> Result<Connector, Failure> {
        let component = Component::new(&self.engine, wasm).map_err(load_failure)?;
        Ok(Connector { pre: self.linker.instantiate_pre(&component).map_err(load_failure)? })
    }

    /// Admit resolved artifact bytes against their reference's pin, then compile them.
    /// Bytes that fail the pin never reach the compiler (`connector.package.digest-mismatch`).
    pub fn load_artifact(&self, artifact: &Artifact, wasm: &[u8], requirement: PinRequirement) -> Result<(Connector, Digest), Failure> {
        let digest = artifact.admit(wasm, requirement).map_err(|e| Failure::deterministic(FailureTag::Config, e.to_string()))?;
        Ok((self.load(wasm)?, digest))
    }

    /// Instantiate a session. `config` is the pipeline's guest table: checked ahead of
    /// any I/O, then delivered once per instance, ahead of discovery
    /// (`connector.import.forwarded-config`).
    pub fn open(&self, connector: &Connector, grant: Grant, limits: &Limits, config: Option<&serde_json::Value>) -> Result<Session, Failure> {
        let config = config.map(guest_config).transpose().map_err(refuse)?;
        if !grant.attach.is_empty() {
            grant.allow.check_bound().map_err(refuse)?;
        }
        let slots = Mediator::slots();
        let (store, guest, instance) = instantiate(&connector.pre, &grant, limits, &slots)?;
        let mut session = Session {
            pre: connector.pre.clone(),
            grant,
            slots,
            config,
            store,
            guest,
            instance,
            handle: None,
            poisoned: false,
            limits: limits.clone(),
            _ticker: self.ticker.clone(),
        };
        session.configure()?;
        Ok(session)
    }
}

fn refuse(e: ConnectorError) -> Failure {
    Failure::deterministic(FailureTag::Config, e.to_string())
}

/// A fresh store and instance, instantiated under the discovery deadline.
fn instantiate(pre: &InstancePre<Ctx>, grant: &Grant, limits: &Limits, slots: &Arc<tokio::sync::Semaphore>) -> Result<(Store<Ctx>, SourceConnector, Instance), Failure> {
    let mut wasi = WasiCtx::builder();
    wasi.allow_tcp(false).allow_udp(false).allow_ip_name_lookup(false).socket_addr_check(|_, _| Box::pin(async { false }));
    let ctx = Ctx {
        wasi: wasi.build(),
        http: WasiHttpCtx::new(),
        table: ResourceTable::new(),
        budget: Budget::new(usize::try_from(limits.memory_bytes).unwrap_or(usize::MAX)),
        mediator: Mediator::new(grant.allow.clone(), grant.attach.clone(), grant.gate.clone(), slots.clone()),
        log_left: limits.log_bytes,
        logs: Vec::new(),
        logs_dropped: 0,
    };
    let mut store = Store::new(pre.engine(), ctx);
    store.limiter(|c| &mut c.budget);
    store.epoch_deadline_trap();
    let deadline = limits.discovery_deadline;
    arm(&mut store, deadline);
    let instance = bounded(deadline, "instantiation", pre.instantiate_async(&mut store))?;
    let guest = SourceConnector::new(&mut store, &instance).map_err(load_failure)?;
    Ok((store, guest, instance))
}

fn arm(store: &mut Store<Ctx>, deadline: Duration) {
    let ticks = (deadline.as_millis() / EPOCH_TICK.as_millis()).max(1);
    store.set_epoch_deadline(u64::try_from(ticks).unwrap_or(u64::MAX));
}

/// Drive one guest call to completion or to `deadline`, whichever comes first. The epoch
/// deadline interrupts guest code; the timeout interrupts a wait inside a host import.
fn bounded<T>(deadline: Duration, during: &str, call: impl Future<Output = wasmtime::Result<T>>) -> Result<T, Failure> {
    match wasmtime_wasi::runtime::in_tokio(async move { tokio::time::timeout(deadline, call).await }) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(trapped(e, during)),
        Err(_) => Err(overran(during)),
    }
}

fn overran(during: &str) -> Failure {
    Failure::new(FailureTag::Transient, format!("the guest ran past its call deadline during {during}"))
}

/// A trap is a bound hit or a guest fault, and either is transient
/// (`run.retry.bound-hit-is-transient`).
fn trapped(e: wasmtime::Error, during: &str) -> Failure {
    if matches!(e.downcast_ref::<Trap>(), Some(Trap::Interrupt)) {
        return overran(during);
    }
    let why = match e.downcast_ref::<Trap>() {
        Some(other) => format!("{other}"),
        None => format!("{e:#}"),
    };
    Failure::new(FailureTag::Transient, format!("the guest trapped: {why} during {during}"))
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
///
/// A call that traps or overruns its deadline leaves the instance unusable, so the
/// session drops it with its open read, and the next call runs on a fresh instance of the
/// same component under the same grant and configuration. Logs, traffic and the
/// in-flight slots carry over.
pub struct Session {
    pre: InstancePre<Ctx>,
    grant: Grant,
    /// The in-flight slots every instance of the session draws from.
    slots: Arc<tokio::sync::Semaphore>,
    config: Option<String>,
    store: Store<Ctx>,
    guest: SourceConnector,
    instance: Instance,
    handle: Option<ResourceAny>,
    poisoned: bool,
    limits: Limits,
    _ticker: Arc<Ticker>,
}

impl Session {
    /// Replace a poisoned instance, then arm `deadline` and snapshot the traffic record.
    fn begin(&mut self, deadline: Duration) -> Result<Traffic, Failure> {
        if self.poisoned {
            self.revive()?;
        }
        arm(&mut self.store, deadline);
        Ok(self.traffic())
    }

    fn revive(&mut self) -> Result<(), Failure> {
        let (store, guest, instance) = instantiate(&self.pre, &self.grant, &self.limits, &self.slots)?;
        let old = std::mem::replace(&mut self.store, store);
        let traffic = old.data().mediator.traffic.lock().map(|t| t.clone()).unwrap_or_default();
        let old = old.into_data();
        let ctx = self.store.data_mut();
        *ctx.mediator.traffic.lock().unwrap_or_else(|e| e.into_inner()) = traffic;
        (ctx.logs, ctx.logs_dropped, ctx.log_left) = (old.logs, old.logs_dropped, old.log_left);
        (self.guest, self.instance, self.handle, self.poisoned) = (guest, instance, None, false);
        self.configure()
    }

    /// Settle one guest call. A trap or an overrun poisons the instance. A request the
    /// mediation point refused or held back during the call fails it, whatever the guest
    /// made of the refusal.
    fn finish<T>(&mut self, before: Traffic, out: Result<T, Failure>) -> Result<T, Failure> {
        if out.is_err() {
            self.poisoned = true;
            self.handle = None;
        }
        let out = out?;
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

    /// Deliver the guest table, when one is declared, to the current instance.
    fn configure(&mut self) -> Result<(), Failure> {
        let Some(table) = self.config.clone() else {
            return Ok(());
        };
        let Some(configure) = self.config_func() else {
            return Err(refuse(ConnectorError::ConnectorConfigUnclaimed(format!(
                "a guest table is declared and the guest exports no `{CONFIG_INTERFACE}`"
            ))));
        };
        let d = self.limits.discovery_deadline;
        let before = self.begin(d)?;
        let out = bounded(d, "configuration", configure.call_async(&mut self.store, (table,)));
        self.finish(before, out)?.0.map_err(guest_error)
    }

    fn config_func(&mut self) -> Option<ConfigFunc> {
        let instance = self.instance;
        let (_, iface) = instance.get_export(&mut self.store, None, CONFIG_INTERFACE)?;
        let (_, func) = instance.get_export(&mut self.store, Some(&iface), "configure")?;
        instance.get_typed_func(&mut self.store, func).ok()
    }

    fn attribution_func(&mut self) -> Option<AttributionFunc> {
        let instance = self.instance;
        let (_, iface) = instance.get_export(&mut self.store, None, ATTRIBUTION_INTERFACE)?;
        let (_, func) = instance.get_export(&mut self.store, Some(&iface), "failed-partitions")?;
        instance.get_typed_func(&mut self.store, func).ok()
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
        let d = self.limits.discovery_deadline;
        let before = self.begin(d)?;
        let out = bounded(d, "a call", self.guest.contextful_connector_source().call_cursor_kind(&mut self.store));
        self.finish(before, out).map(kind_in)
    }

    pub fn discover(&mut self) -> Result<Vec<Schema>, Failure> {
        let d = self.limits.discovery_deadline;
        let before = self.begin(d)?;
        let out = bounded(d, "a call", self.guest.contextful_connector_source().call_discover(&mut self.store));
        let schemas = self.finish(before, out)?.map_err(guest_error)?;
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
        let d = self.limits.read_deadline;
        let before = self.begin(d)?;
        let out = bounded(d, "a call", self.guest.contextful_connector_source().call_open(&mut self.store, table, from.as_ref()));
        let handle = self.finish(before, out)?.map_err(guest_error)?;
        self.handle = Some(handle);
        Ok(())
    }

    fn opened(&self) -> Result<ResourceAny, Failure> {
        self.handle.ok_or_else(|| Failure::new(FailureTag::Permanent, "a read was asked of a session holding no open read"))
    }

    /// The next batch as Arrow IPC bytes, or `None` once the read is exhausted.
    #[allow(clippy::should_implement_trait, reason = "a fallible guest call under a deadline, not an iterator step")]
    pub fn next(&mut self) -> Result<Option<Vec<u8>>, Failure> {
        let handle = self.opened()?;
        let d = self.limits.read_deadline;
        let before = self.begin(d)?;
        let out = bounded(d, "a call", self.guest.contextful_connector_source().read_handle().call_next(&mut self.store, handle));
        self.finish(before, out)?.map_err(guest_error)
    }

    /// Where the open read stands after the last batch.
    pub fn position(&mut self) -> Result<Cursor, Failure> {
        let handle = self.opened()?;
        let d = self.limits.read_deadline;
        let before = self.begin(d)?;
        let out = bounded(d, "a call", self.guest.contextful_connector_source().read_handle().call_position(&mut self.store, handle));
        let c = self.finish(before, out)?;
        Ok(Cursor { kind: kind_in(c.kind), bytes: c.bytes })
    }

    /// The partition values the session's failed reads belong to, empty for a guest
    /// exporting no attribution. A value over [`ATTRIBUTION_VALUE_BYTES`] and entries past
    /// [`ATTRIBUTION_ENTRIES`] are dropped and stay unattributed; the rest keep their bytes.
    pub fn failed_partitions(&mut self) -> Result<Vec<String>, Failure> {
        let d = self.limits.read_deadline;
        let before = self.begin(d)?;
        let Some(func) = self.attribution_func() else {
            return Ok(Vec::new());
        };
        let out = bounded(d, "a call", func.call_async(&mut self.store, ()));
        let (mut values,) = self.finish(before, out)?;
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
        let Some(handle) = self.handle.take() else {
            return;
        };
        if self.poisoned {
            return;
        }
        // The handle's destructor runs guest code; it gets a fresh deadline, and the
        // table entry goes whether or not it traps.
        let d = self.limits.read_deadline;
        arm(&mut self.store, d);
        if bounded(d, "a close", handle.resource_drop_async(&mut self.store)).is_err() {
            self.poisoned = true;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}
