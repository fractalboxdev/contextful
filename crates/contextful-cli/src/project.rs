//! `contextful init`, and the project a command acts on: `--project` under the working
//! directory, or the nearest `contextful.toml` upward (`store.init.discovery`).

use anyhow::{Context, Result};
use contextful_context::project::{check_name, discover, Project};
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::pipeline::declare::{ManifestFile, PipelineSpec};
use contextful_core::run::derive::task::Tasks;
use contextful_policy::enforce::mask::Pepper;
use std::path::Path;
#[cfg(feature = "data-plane")]
use anyhow::anyhow;
#[cfg(feature = "data-plane")]
use contextful_context::project::{declare_posture, declared_posture, init, Initialized, DECLARATION_FILE};
#[cfg(feature = "data-plane")]
use contextful_core::issue::AuthoringPosture;
use contextful_core::run::record::{resolve_site_id, SiteIdSources};
use std::path::PathBuf;

#[cfg(feature = "data-plane")]
#[derive(clap::Args)]
pub struct InitArgs {
    /// The project name; its store root is `.contextful/context/<name>/`.
    name: String,
    /// `session` or `per_request`: who authors a table write that carries no credential.
    /// Written when the declaration declares none; absent writes no posture, and every
    /// table write refuses until one is declared.
    #[arg(long, value_parser = posture)]
    authoring_posture: Option<AuthoringPosture>,
}

#[cfg(feature = "data-plane")]
fn posture(value: &str) -> Result<AuthoringPosture> {
    AuthoringPosture::parse(value).ok_or_else(|| anyhow!("`{value}` is neither `session` nor `per_request`"))
}

/// A command's project and the declaration it reads.
pub struct Located {
    pub project: Project,
    pub declaration: PathBuf,
}

/// The synced UUID binds an owner credential to one store instance and its clones.
pub(crate) fn owner_identity(project: &Project) -> Result<String> {
    Ok(contextful_context::project::store_id(&project.store_root())?)
}

/// Legacy stores acquire the synced UUID before minting their first owner claim.
pub(crate) fn mint_owner_identity(project: &Project) -> Result<String> {
    Ok(contextful_context::project::ensure_store_id(&project.store_root())?)
}

/// Resolve the project from `--project` or by discovery, and the declaration from
/// `--declaration` or beside the project (`store.init.default-declaration`).
pub fn locate(project: Option<&str>, declaration: Option<PathBuf>) -> Result<Located> {
    let cwd = std::env::current_dir()?;
    let (project, default) = match project {
        Some(name) => {
            check_name(name)?;
            (Project { dir: cwd, name: name.to_string() }, PathBuf::from(contextful_context::project::DECLARATION_FILE))
        }
        None => {
            let p = discover(&cwd)?;
            let d = p.declaration();
            (p, d)
        }
    };
    let located = Located { project, declaration: declaration.unwrap_or(default) };
    // A manifest the command cannot read is the command's own error to report.
    if let Ok(files) = contextful_context::project::manifests(&located.declaration) {
        check_inline(&files)?;
    }
    crate::sync::register_lazy(&located);
    Ok(located)
}

/// The manifest files, in reading order: the declaration when it is a file, then
/// `pipelines/` sorted (`run.declare.manifest-file`).
pub(crate) fn manifests(declaration: &Path) -> Result<Vec<ManifestFile>> {
    let files = contextful_context::project::manifests(declaration)?;
    check_inline(&files)?;
    Ok(files)
}

/// Refuse a plaintext credential written inline in any of `files` unless this build is the
/// development profile (`connector.resolve.inline-outside-development`).
fn check_inline(files: &[ManifestFile]) -> Result<()> {
    for f in files {
        contextful_core::connector::resolve::check_inline(crate::PROFILE, &f.path, &f.text)?;
    }
    Ok(())
}

/// Every `pipelines/*.toml` and `pipelines/*.json` beside the declaration, in path order.
pub(crate) fn pipeline_files(declaration: &Path) -> Result<Vec<ManifestFile>> {
    Ok(contextful_context::project::pipeline_files(declaration)?)
}

/// The serving guardrail a face runs at start (`disclosure.mirror.unbound-table`): when the
/// declaration declares the project's faces organization-wide
/// (`disclosure.mirror.face-scope`), every table of its declaration set carries a visibility
/// block. A project or declaration that does not resolve is left for the command to report.
pub(crate) fn face_guardrail(project: Option<&str>, declaration: Option<&Path>) -> Result<()> {
    now_offset()?;
    let Ok(located) = locate(project, declaration.map(Path::to_path_buf)) else { return Ok(()) };
    let Ok(text) = std::fs::read_to_string(&located.declaration) else { return Ok(()) };
    let scope = contextful_core::read::face::FaceScope::declared(&text).with_context(|| format!("`{}`", located.declaration.display()))?;
    if scope == contextful_core::read::face::FaceScope::Personal {
        return Ok(());
    }
    let tables = contextful_core::store::declare::TableDecl::parse_declaration_set(&text, &pipeline_files(&located.declaration)?)
        .with_context(|| format!("`{}`", located.declaration.display()))?;
    contextful_core::disclosure::declare::check_unbound(scope, &tables)?;
    Ok(())
}

/// The seconds `CONTEXTFUL_NOW_OFFSET_SECS` moves a staleness check's now; a malformed
/// value stops the process before it serves, naming the variable
/// (`disclosure.bound-staleness.offset-malformed`).
fn now_offset() -> Result<u64> {
    let var = contextful_core::disclosure::stale::NOW_OFFSET_VAR;
    Ok(contextful_core::disclosure::stale::now_offset(std::env::var(var).ok().as_deref())?)
}

/// Open the read face over the project's store, the declaration's text and the
/// `pipelines/` files beside it (`read.register.declaration-set`).
pub(crate) fn open_face(project: &Project, declaration: &Path, manifest: &str, pepper: Pepper) -> Result<Face> {
    open_face_with_pins(project, declaration, manifest, pepper, None, None)
}

pub(crate) fn open_store(project: &Project, public_key: Option<&str>, keyset: Option<&Path>) -> Result<Store> {
    with_erasure_keys(Store::open(&project.dir, &project.name)?, project, public_key, keyset)
}

pub(crate) fn with_erasure_keys(store: Store, project: &Project, public_key: Option<&str>, keyset: Option<&Path>) -> Result<Store> {
    match policy_keys(project, public_key, keyset)? {
        Some(source) => Ok(store.with_erasure_key_source(project.audit_dir(), source)?),
        None => Ok(store),
    }
}

fn policy_keys(project: &Project, public_key: Option<&str>, keyset: Option<&Path>) -> Result<Option<std::sync::Arc<dyn contextful_policy::keyset::KeySource>>> {
    let configured = public_key.map(str::to_owned).or_else(|| std::env::var(crate::admit::PUBKEY_VAR).ok().filter(|value| !value.trim().is_empty()));
    match configured {
        Some(pins) => {
            let source = crate::admit::LivePins::new(crate::admit::static_pins(Some(&pins))?,
                std::sync::Arc::new(crate::admit::LedgerFile::at(&project.dir, keyset)), crate::clock::SystemClock);
            Ok(Some(std::sync::Arc::new(source)))
        }
        None => Ok(None),
    }
}

pub(crate) struct ReadSigner {
    signer: std::sync::Arc<contextful_policy::issue::SeedSigner>,
    keys: std::sync::Arc<dyn contextful_policy::keyset::KeySource>,
}
impl ReadSigner {
    fn validate(&self) -> Result<(), contextful_core::AuthorityError> {
        let signer = contextful_policy::issue::SignerKey::of(self.signer.as_ref());
        if !self.keys.keys()?.keys().any(|key| key.algorithm() == signer.algorithm && key.public_key.to_bytes() == signer.public_key) {
            return Err(contextful_core::AuthorityError::IssuerKeyUnresolvable("the read signing port is absent from independently configured current pins".into()));
        }
        Ok(())
    }
}
impl contextful_core::ports::SigningPort for ReadSigner {
    fn encoding(&self) -> contextful_core::issue::SignatureEncoding { self.signer.encoding() }
    fn public_key(&self) -> Vec<u8> { self.signer.public_key() }
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, contextful_core::AuthorityError> { self.validate()?; self.signer.sign(message) }
}

pub(crate) enum ReadAudit {
    Unanchored(contextful_policy::audit::AuditLog<contextful_policy::audit::NoIssuerKey>),
    Signed { log:contextful_policy::audit::AuditLog<ReadSigner>, signer:ReadSigner },
}
impl ReadAudit {
    pub(crate) fn append(&self, attributes: serde_json::Value) -> Result<contextful_policy::audit::AuditEntry, contextful_policy::audit::AuditError> {
        match self {
            Self::Unanchored(log) => log.append(attributes),
            Self::Signed { log, signer } => {
                signer.validate().map_err(|_| contextful_policy::audit::AuditError::AuditEntryUnpersisted("the configured read signing authority is unavailable".into()))?;
                log.append(attributes)
            }
        }
    }
}
impl contextful_agent::mcp::ReadRecord for ReadAudit {
    fn record(&self, attributes: serde_json::Value) -> Result<(), contextful_policy::audit::AuditError> { self.append(attributes).map(|_| ()) }
}

pub(crate) fn read_audit(project: &Project, issuer_key: Option<&Path>, public_key: Option<&str>, keyset: Option<&Path>) -> Result<ReadAudit> {
    let Some(path) = issuer_key else { return Ok(ReadAudit::Unanchored(contextful_policy::audit::AuditLog::unanchored(project.audit_dir())?)) };
    let keys = policy_keys(project, public_key, keyset)?.ok_or_else(|| anyhow::anyhow!("the read signing port has no independently configured verification pins"))?;
    let signer = ReadSigner { signer:std::sync::Arc::new(contextful_policy::issue::SeedSigner::resolve(Some(path))?), keys:keys.clone() };
    signer.validate()?;
    let custody = ReadSigner { signer:signer.signer.clone(), keys };
    custody.validate()?;
    // A read verb opens a chain `audit anchor` holds and never anchors one (`disclosure.record.signed-read`).
    let log = contextful_policy::audit::AuditLog::open_anchored(project.audit_dir(), custody)?;
    Ok(ReadAudit::Signed { log, signer })
}

pub(crate) fn open_face_with_pins(project: &Project, declaration: &Path, manifest: &str, pepper: Pepper, public_key: Option<&str>, keyset: Option<&Path>) -> Result<Face> {
    let offset = now_offset()?;
    let store = open_store(project, public_key, keyset)?;
    let face = Face::open_declared(store, manifest, &pipeline_files(declaration)?, pepper)?;
    let cohorts = cohort_models(declaration)?;
    let derives = if cohorts.is_empty() { Vec::new() } else { derive_lineage(declaration, manifest)? };
    Ok(face.with_cohorts(&cohorts)?.with_derived(&derives).with_now_offset(offset))
}

/// The host tasks the embedding binary registered, whose joins enter a derive's lineage.
static HOST_TASKS: std::sync::OnceLock<Tasks> = std::sync::OnceLock::new();

/// Record the embedding binary's host tasks for [`derive_lineage`].
pub(crate) fn register_tasks(tasks: &Tasks) {
    let _ = HOST_TASKS.set(tasks.clone());
}

/// Each derive pipeline's parents beside its output tables, then each `synthesize` job of
/// `manifest`'s source beside its claims and dead-letter tables, so an output of a
/// per-individual table is itself per-individual (`disclosure.bound-cohort.derived-individual`).
fn derive_lineage(declaration: &Path, manifest: &str) -> Result<Vec<(Vec<String>, Vec<String>)>> {
    let declared = contextful_core::pipeline::declare::collect(&manifests(declaration)?)?;
    let derives = declared
        .iter()
        .map(|d| &d.spec)
        .filter(|s| s.source.name == DERIVE_SOURCE)
        .map(|s| (derive_parents(s, HOST_TASKS.get()), s.tables.iter().map(|t| s.table_name(t.name())).collect()));
    Ok(derives.chain(contextful_core::job::synthesize_lineage(manifest)?).collect())
}

/// The source name a derive pipeline declares.
const DERIVE_SOURCE: &str = "derive";

/// A derive pipeline's source table, then each table its registered host task joins.
pub(crate) fn derive_parents(spec: &PipelineSpec, tasks: Option<&Tasks>) -> Vec<String> {
    let source = spec.source.config.get("source_table").and_then(serde_json::Value::as_str).map(str::to_string);
    source.into_iter().chain(host_joins(spec, tasks)).collect()
}

/// The tables a derive pipeline's registered host task joins; none for another source or task.
pub(crate) fn host_joins(spec: &PipelineSpec, tasks: Option<&Tasks>) -> Vec<String> {
    let task = (spec.source.name == DERIVE_SOURCE)
        .then(|| spec.source.config.get("task").and_then(serde_json::Value::as_str).and_then(|name| tasks?.get(name)))
        .flatten();
    task.map(|t| t.joins().into_iter().map(|j| j.table).collect()).unwrap_or_default()
}

/// Each `cohort` model the declaration set declares, its statement resolved, whose source
/// tables answer only `read-individual` (`disclosure.bound-cohort.per-individual-row`).
pub(crate) fn cohort_models(declaration: &Path) -> Result<Vec<contextful_core::pipeline::model::ModelSpec>> {
    use contextful_core::pipeline::model::{collect_models, Governance};
    let mut cohort = Vec::new();
    for model in collect_models(&manifests(declaration)?, &[])? {
        if model.spec.disclosure.as_ref().is_some_and(|d| d.governance() == Governance::Cohort) {
            let mut spec = model.spec;
            crate::model_source::resolve(&mut spec, &model.file)?;
            cohort.push(spec);
        }
    }
    Ok(cohort)
}

#[cfg(feature = "data-plane")]
pub fn run(args: InitArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let what = match init(&cwd, &args.name)? {
        Initialized::Created => "created contextful.toml",
        Initialized::Adopted => "declared the project in the existing contextful.toml",
        Initialized::Unchanged => "unchanged, contextful.toml already declares it",
    };
    println!("{}: {what}; store root .contextful/context/{}/", args.name, args.name);
    let prepended = match args.authoring_posture {
        Some(p) => declare_posture(&cwd, p)?,
        None => false,
    };
    let path = cwd.join(DECLARATION_FILE);
    match declared_posture(&path, &std::fs::read_to_string(&path)?)? {
        Some(p) if prepended => println!("authoring posture: {p}, declared by this init"),
        Some(p) => println!("authoring posture: {p}"),
        None => println!("no authoring posture: every table write refuses until one is declared (init --authoring-posture session|per_request)"),
    }
    Ok(())
}

/// The site id of one run: the command line's declaration when it makes one, else the
/// manifest's (`run.record.site-id-unresolved`).
pub(crate) fn site_id_for(manifest: &str, path: &Path, id: Option<String>, env: Option<String>) -> Result<String> {
    let var = |k: &str| std::env::var(k).ok();
    let declared = SiteIdSources::from_manifest(manifest, var).with_context(|| format!("`{}`", path.display()))?;
    Ok(resolve_site_id(&SiteIdSources::declared(id, env, var).over(declared))?)
}
