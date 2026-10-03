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
    let mut files = Vec::new();
    if declaration.is_file() {
        files.push(ManifestFile { path: declaration.display().to_string(), text: std::fs::read_to_string(declaration)? });
    }
    files.extend(pipeline_files(declaration)?);
    Ok(files)
}

/// Every `pipelines/*.toml` and `pipelines/*.json` beside the declaration, in path order.
pub(crate) fn pipeline_files(declaration: &Path) -> Result<Vec<ManifestFile>> {
    let dir = declaration.parent().map(|p| p.join("pipelines")).unwrap_or_else(|| PathBuf::from("pipelines"));
    let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
    let mut paths: Vec<PathBuf> =
        entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml" || x == "json")).collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| Ok(ManifestFile { path: p.strip_prefix(".").unwrap_or(&p).display().to_string(), text: std::fs::read_to_string(&p)? }))
        .collect()
}

/// Open the read face over the project's store, the declaration's text and the
/// `pipelines/` files beside it (`read.register.declaration-set`).
pub(crate) fn open_face(project: &Project, declaration: &Path, manifest: &str, pepper: Pepper) -> Result<Face> {
    let store = Store::open(&project.dir, &project.name)?;
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
