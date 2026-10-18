//! Coordinated exports use the admitted read path and the shared durable outbox.

use super::*;
use contextful_core::export::publication::{
    declarations, GroupState, PublicationDeclaration, PublicationPlan,
};
use contextful_core::run::record::RunRow;
use contextful_core::export::publication::{GroupBatch, PublicationGroup, SourcePublication};
use contextful_sqlite::PublicationLedger;

/// The machine-local ledger, or D1 under the `cloudflare-d1` profile.
enum Ledger {
    Local(Box<PublicationLedger>),
    #[cfg(feature = "d1-catalog")]
    D1(Box<contextful_d1::D1PublicationLedger>),
}

macro_rules! each {
    ($ledger:expr, $l:ident => $call:expr) => {
        match $ledger {
            Ledger::Local($l) => $call.map_err(anyhow::Error::from),
            #[cfg(feature = "d1-catalog")]
            Ledger::D1($l) => $call.map_err(anyhow::Error::from),
        }
    };
}

impl Ledger {
    fn incarnation(&self, candidate: &str) -> Result<String> { each!(self, l => l.incarnation(candidate)) }
    fn record_source(&self, name: &str, source: &SourcePublication) -> Result<()> { each!(self, l => l.record_source(name, source)) }
    fn source(&self, name: &str) -> Result<Option<SourcePublication>> { each!(self, l => l.source(name)) }
    fn active_namespace(&self, name: &str) -> Result<Option<String>> { each!(self, l => l.active_namespace(name)) }
    fn active(&self, name: &str, namespace: &str) -> Result<Option<PublicationGroup>> { each!(self, l => l.active(name, namespace)) }
    fn pending_identity(&self, name: &str, namespace: &str) -> Result<Option<String>> { each!(self, l => l.pending_identity(name, namespace)) }
    fn stage(&mut self, plan: &PublicationPlan, states: &GroupState, identity: &str) -> Result<()> { each!(self, l => l.stage(plan, states, identity)) }
    fn batch(&mut self, name: &str, namespace: &str) -> Result<Option<GroupBatch>> { each!(self, l => l.batch(name, namespace)) }
    fn acknowledge(&mut self, name: &str, namespace: &str, group: &str, through: u64) -> Result<()> { each!(self, l => l.acknowledge(name, namespace, group, through)) }
    fn checkpoint(&self, name: &str, namespace: &str) -> Result<Vec<u8>> { each!(self, l => l.checkpoint(name, namespace)) }
    fn restore(&mut self, name: &str, namespace: &str, bytes: &[u8], incarnation: &str, identity: &str) -> Result<()> { each!(self, l => l.restore(name, namespace, bytes, incarnation, identity)) }
}

/// Under `cloudflare-d1` the publication's durable lease, not the machine file lock, fences writers;
/// `take_over` takes that lease from a holder killed before its expiry (`run.export.group-d1-takeover`).
fn ledger(store: &contextful_context::Store, name: &str, take_over: bool) -> Result<Ledger> {
    supported()?;
    if let Some(crate::storage::Remote::D1 { endpoint, .. }) = crate::storage::remote()? {
        #[cfg(feature = "d1-catalog")]
        {
            let holder = format!("{}:{}:{}", crate::run::boot_id(), std::process::id(), wall_clock()?.unix_nanos());
            let mut ledger = contextful_d1::D1PublicationLedger::connect(&endpoint)?;
            let ttl = contextful_core::coordinate::CADENCE_LEASE_TTL_SECS;
            if take_over {
                ledger.take_over(name, &holder, ttl)?;
            } else {
                ledger.claim(name, &holder, ttl)?;
            }
            return Ok(Ledger::D1(Box::new(ledger)));
        }
        #[cfg(not(feature = "d1-catalog"))]
        {
            let _ = (endpoint, name, take_over);
            anyhow::bail!("ProfileCapabilityAbsent: D1 publication state requires `d1-catalog`");
        }
    }
    let path = store.root().join(MACHINE_CATALOG_FILE);
    Ok(Ledger::Local(Box::new(match store.file_cipher() {
        Some(cipher) => PublicationLedger::open_sealed(&path, cipher)?,
        None => PublicationLedger::open(&path)?,
    })))
}

fn supported() -> Result<()> {
    if std::env::var("CONTEXTFUL_STORAGE_PROFILE").as_deref() == Ok("postgres") {
        anyhow::bail!(
            "PublicationStorageUnsupported: postgres lacks coordinated-group-outbox"
        );
    }
    Ok(())
}

pub(crate) fn lock(located: &crate::project::Located) -> Result<FileLock> {
    let dir = cursor_dir(&located.project.dir, &located.project.name);
    std::fs::create_dir_all(&dir)?;
    Ok(FileLock::acquire(
        &dir.join("publication-groups.lock"),
        std::time::Duration::from_secs(LOCK_WAIT_SECS),
    )?)
}

/// Each declared publication as its name, producer job and member tables.
pub(crate) fn families(text: &str) -> Result<Vec<(String, String, Vec<String>)>> {
    let exports = parse_exports(text)?;
    Ok(declarations(text)?
        .into_iter()
        .map(|group| {
            let tables = group
                .members
                .iter()
                .map(|name| exports.iter().find(|e| &e.name == name).expect("validated member").table.clone())
                .collect();
            (group.name, group.job, tables)
        })
        .collect())
}

/// The lock files fencing serve's dispatch over publication `name`'s inputs: an intent an
/// export takes first, so no unit starts over them, then the inputs each started unit holds
/// shared (`surface.dispatch.export-quiesced`, `run.export.group-quiesce`).
fn quiesce_files(project_dir: &Path, project: &str, name: &str) -> Result<(std::fs::File, std::fs::File)> {
    let dir = cursor_dir(project_dir, project);
    std::fs::create_dir_all(&dir)?;
    let open = |kind: &str| -> Result<std::fs::File> {
        let path = dir.join(format!("publication-{name}.{kind}.lock"));
        Ok(std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)
            .with_context(|| format!("opening `{}`", path.display()))?)
    };
    Ok((open("intent")?, open("inputs")?))
}

/// Hold publication `name`'s inputs shared for one serve unit, or `None` while an export holds
/// or awaits them (`surface.dispatch.export-claim`).
pub(crate) fn hold_inputs(project_dir: &Path, project: &str, name: &str) -> Result<Option<std::fs::File>> {
    let (intent, inputs) = quiesce_files(project_dir, project, name)?;
    for file in [&intent, &inputs] {
        match file.try_lock_shared() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
    Ok(Some(inputs))
}

/// Take publication `name`'s intent, then its inputs, exclusively; each waits at most
/// [`QUIESCE_WAIT_SECS`] for serve's units or another export (`run.export.group-quiesce-wait`).
fn quiesce(located: &crate::project::Located, name: &str) -> Result<(std::fs::File, std::fs::File)> {
    let (intent, inputs) = quiesce_files(&located.project.dir, &located.project.name, name)?;
    for (file, what) in [(&intent, "dispatch intent"), (&inputs, "inputs")] {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(QUIESCE_WAIT_SECS);
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50))
                }
                Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
                    "PublicationInputsBusy: publication `{name}`: its {what} stayed held for {QUIESCE_WAIT_SECS} s"
                ),
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
    }
    Ok((intent, inputs))
}

/// (`run.export.group-quiesce-wait`)
const QUIESCE_WAIT_SECS: u64 = 300;

/// The completion frontier enters the ledger only after every output closes successfully.
pub(crate) fn record_source(
    store: &contextful_context::Store,
    text: &str,
    job: &str,
    row: &RunRow,
) -> Result<()> {
    record_completion(
        store,
        text,
        job,
        &row.run_id,
        serde_json::to_value(&row.input)?,
    )
}

pub(crate) fn record_completion(
    store: &contextful_context::Store,
    text: &str,
    job: &str,
    run: &str,
    input: serde_json::Value,
) -> Result<()> {
    let groups = declarations(text)?;
    let exports = parse_exports(text)?;
    if groups.iter().any(|g| g.job == job) {
        supported()?;
    }
    for group in groups.iter().filter(|g| g.job == job) {
        let frontiers = group
            .members
            .iter()
            .map(|name| {
                let export = exports
                    .iter()
                    .find(|e| &e.name == name)
                    .expect("validated member");
                Ok((export.table.clone(), source_frontier(store, &export.table)?))
            })
            .collect::<Result<_>>()?;
        ledger(store, &group.name, false)?.record_source(
            &group.name,
            &SourcePublication {
                run: run.into(),
                input: input.clone(),
                frontiers,
            },
        )?;
    }
    Ok(())
}

pub(crate) fn validate_job(text: &str, job: &str, tables: &[String]) -> Result<bool> {
    let groups = declarations(text)?;
    if groups.iter().any(|g| g.job == job) {
        supported()?;
    }
    let exports = parse_exports(text)?;
    for group in groups.iter().filter(|g| g.job == job) {
        let members: std::collections::BTreeSet<_> = group
            .members
            .iter()
            .map(|name| {
                &exports
                    .iter()
                    .find(|e| &e.name == name)
                    .expect("validated export")
                    .table
            })
            .collect();
        if members != tables.iter().collect() {
            anyhow::bail!(
                "PublicationIncompatible: group `{}` names exactly every output of job `{job}`",
                group.name
            );
        }
    }
    Ok(groups.iter().any(|g| g.job == job))
}

fn group(text: &str, name: &str) -> Result<(PublicationDeclaration, Vec<Export>)> {
    let group = declarations(text)?
        .into_iter()
        .find(|g| g.name == name)
        .with_context(|| format!("no publication `{name}` is declared"))?;
    let all = parse_exports(text)?;
    let exports = group
        .members
        .iter()
        .map(|m| {
            all.iter()
                .find(|e| &e.name == m)
                .expect("validated member")
                .clone()
        })
        .collect();
    Ok((group, exports))
}

fn identity(
    face: &contextful_context::read::face::Face,
    session: &Session,
    authority: &contextful_policy::verify::AdmittedAuthority,
    exports: &[Export],
) -> Result<String> {
    let mut identities = BTreeMap::new();
    for export in exports {
        identities.insert(
            &export.name,
            read_identity(face, session, authority, export)?,
        );
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            identities,
            exports[0].endpoint.as_str()
        ))?)
    ))
}

pub(super) fn deliver(
    name: &str,
    requested_namespace: Option<String>,
    project: &ProjectArgs,
    declaration: Option<PathBuf>,
    admit: &AdmitArgs,
    restore: Option<PathBuf>,
    take_over: bool,
) -> Result<()> {
    supported()?;
    let located = project.locate(declaration)?;
    crate::storage::preflight(&located.project)?;
    let text = std::fs::read_to_string(&located.declaration)?;
    let (declared, exports) = group(&text, name)?;
    // Serve starts no unit over the publication's inputs until this export ends, and every
    // unit already running ends before the pull (`run.export.group-quiesce`).
    let _quiesced = quiesce(&located, name)?;
    crate::sync::pull_before_run(&located)?;
    let _held = lock(&located)?;
    let vars: BTreeMap<String, String> = std::env::vars().collect();
    let resolver = contextful_outbound::assemble(&vars, Arc::new(SystemClock))?;
    for export in &exports {
        resolver.preflight(export.headers.values())?;
    }
    let export = &exports[0];
    if exports.iter().any(|e| e.headers != export.headers) {
        anyhow::bail!("PublicationIncompatible: publication members share one header binding");
    }
    let allow = Allowlist::parse(&[export.endpoint.host_str().unwrap_or_default()])?;
    if export.headers.values().any(|t| t.has_reference()) {
        allow.check_bound()?;
    }
    let client = Client::new(allow, export.endpoint.clone()).with_body_limit(64 * 1024);
    let (authority, _) = admit.admit(project.project.as_deref(), "a coordinated export")?;
    let face = face(&located)?;
    let session = face.session(&authority, &Request { zone: None }, Bounds::default())?;
    // Resolve every member through the admitted face, including empty members.
    for export in &exports {
        face.query(
            &session,
            &format!("SELECT * FROM {} LIMIT 0", ident(&export.table)),
            ReadOptions::default(),
        )?;
    }
    let identity = identity(&face, &session, &authority, &exports)?;
    let mut ledger = ledger(face.store(), name, take_over)?;
    let incarnation = ledger.incarnation(&format!(
        "{:x}",
        Sha256::digest(format!(
            "{}:{}:{}",
            wall_clock()?.unix_nanos(),
            std::process::id(),
            located.project.name
        ))
    ))?;
    let rebuilding = requested_namespace.is_some() && restore.is_none();
    let namespace = requested_namespace
        .or(ledger.active_namespace(name)?)
        .unwrap_or_else(|| "initial".into());
    if !contextful_core::store::lay_out::is_path_segment(&namespace) {
        anyhow::bail!("PublicationIncompatible: namespace is a path segment");
    }
    if let Some(path) = restore {
        let bytes = face
            .store()
            .metadata_files()
            .read_optional(&path)?
            .context("publication checkpoint is absent")?;
        ledger.restore(name, &namespace, &bytes, &incarnation, &identity)?;
    } else if rebuilding && ledger.active(name, &namespace)?.is_some() {
        anyhow::bail!("PublicationIncompatible: a rebuild requires a fresh namespace");
    }
    if let Some(pending_identity) = ledger.pending_identity(name, &namespace)? {
        if pending_identity != identity {
            anyhow::bail!("PublicationIncompatible: pending read identity differs");
        }
    } else {
        let source = ledger
            .source(&declared.name)?
            .context("publication has no completed producer frontier")?;
        let mut states = GroupState::new();
        for export in &exports {
            if source.frontiers.get(&export.table)
                != Some(&source_frontier(face.store(), &export.table)?)
            {
                anyhow::bail!(
                    "PublicationIncompatible: the producer frontier is incomplete or has advanced"
                );
            }
            states.insert(export.name.clone(), read_state(&face, &session, export)?);
        }
        for export in &exports {
            if source.frontiers.get(&export.table)
                != Some(&source_frontier(face.store(), &export.table)?)
            {
                anyhow::bail!(
                    "PublicationIncompatible: the producer frontier moved during the read"
                );
            }
        }
        if identity != self::identity(&face, &session, &authority, &exports)? {
            anyhow::bail!("PublicationIncompatible: read identity changed during staging");
        }
        let active = ledger.active(name, &namespace)?;
        let input = serde_json::Value::String(source.digest()?);
        let plan = PublicationPlan {
            name: name.into(),
            incarnation,
            namespace: namespace.clone(),
            destination: format!("{:x}", Sha256::digest(export.endpoint.as_str().as_bytes())),
            read_identity: identity.clone(),
            base: active.as_ref().map(|g| g.id.clone()),
            order: active
                .as_ref()
                .map_or(Some(1), |g| g.plan.order.checked_add(1))
                .context("publication order overflow")?,
            input,
            members: exports
                .iter()
                .map(|e| (e.name.clone(), e.table.clone()))
                .collect(),
        };
        ledger.stage(&plan, &states, &identity)?;
    }
    let mut delivered = 0;
    while let Some(batch) = ledger.batch(name, &namespace)? {
        let mut headers = vec![(
            "Content-Type".into(),
            HeaderValue::Plain("application/json".into()),
        )];
        for (name, template) in &export.headers {
            let value = resolver.render(template)?;
            headers.push((
                name.clone(),
                if template.has_reference() {
                    HeaderValue::Sensitive(Arc::new(value))
                } else {
                    HeaderValue::Plain(value.reveal().into())
                },
            ));
        }
        let bytes = serde_json::to_vec(&batch)?;
        match client.send("POST", &export.endpoint, &headers, Some(&bytes)) {
            Ok(response) if (200..300).contains(&response.status) => ledger.acknowledge(
                name,
                &namespace,
                &batch.group.id,
                batch.events.last().expect("nonempty batch").sequence,
            )?,
            Ok(response) => {
                return Err(ExportError::ExportDeliveryRefused(format!(
                    "publication `{name}`: target answered {}",
                    response.status
                ))
                .into())
            }
            Err(failure) => {
                return Err(ExportError::ExportDeliveryRefused(format!(
                    "publication `{name}`: {}",
                    failure.message
                ))
                .into())
            }
        }
        delivered += batch.events.len();
    }
    println!("{name}: acknowledged {delivered} coordinated events in namespace {namespace}");
    Ok(())
}

pub(super) fn checkpoint(
    name: &str,
    namespace: &str,
    out: &Path,
    project: &ProjectArgs,
    declaration: Option<PathBuf>,
) -> Result<()> {
    let located = project.locate(declaration)?;
    crate::storage::preflight(&located.project)?;
    crate::sync::pull_before_run(&located)?;
    let _held = lock(&located)?;
    let face = face(&located)?;
    let bytes = ledger(face.store(), name, false)?.checkpoint(name, namespace)?;
    face.store().metadata_files().replace(out, &bytes)?;
    Ok(())
}
