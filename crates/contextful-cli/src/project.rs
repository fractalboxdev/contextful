//! `contextful init`, and the project a command acts on: `--project` under the working
//! directory, or the nearest `contextful.toml` upward (`store.init.discovery`).

use anyhow::{Context, Result};
use contextful_context::project::{check_name, discover, Project};
use contextful_context::read::Face;
use contextful_context::Store;
use contextful_core::pipeline::declare::ManifestFile;
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
    Ok(Located { project, declaration: declaration.unwrap_or(default) })
}

/// The manifest files, in reading order: the declaration when it is a file, then
/// `pipelines/` sorted (`run.declare.manifest-file`).
pub(crate) fn manifests(declaration: &Path) -> Result<Vec<ManifestFile>> {
    Ok(contextful_context::project::manifests(declaration)?)
}

/// Every `pipelines/*.toml` and `pipelines/*.json` beside the declaration, in path order.
pub(crate) fn pipeline_files(declaration: &Path) -> Result<Vec<ManifestFile>> {
    Ok(contextful_context::project::pipeline_files(declaration)?)
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
    let log = contextful_policy::audit::AuditLog::anchor(project.audit_dir(), custody)?;
    Ok(ReadAudit::Signed { log, signer })
}

pub(crate) fn open_face_with_pins(project: &Project, declaration: &Path, manifest: &str, pepper: Pepper, public_key: Option<&str>, keyset: Option<&Path>) -> Result<Face> {
    let store = open_store(project, public_key, keyset)?;
    Ok(Face::open_declared(store, manifest, &pipeline_files(declaration)?, pepper)?)
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
