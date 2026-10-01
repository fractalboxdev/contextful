//! `contextful init`, and the project a command acts on: `--project` under the working
//! directory, or the nearest `contextful.toml` upward (`store.init.discovery`).

use anyhow::{anyhow, Result};
use contextful_context::project::{check_name, declare_posture, declared_posture, discover, init, Initialized, Project, DECLARATION_FILE};
use contextful_core::issue::AuthoringPosture;
use std::path::PathBuf;

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

/// The directory the issuance policy, the default issuer seed and the key-set ledger sit
/// under (`authority.issue.project-root`): the working directory under `--project`, else
/// the nearest directory upward holding `contextful.toml`, else the working directory.
pub fn root(project: Option<&str>) -> Result<PathBuf> {
    let cwd = std::env::current_dir()?;
    if project.is_some() {
        return Ok(cwd);
    }
    let found = cwd.ancestors().find(|d| d.join(contextful_context::project::DECLARATION_FILE).is_file()).map(PathBuf::from);
    Ok(found.unwrap_or(cwd))
}

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
