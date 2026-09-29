//! `contextful init`, and the project a command acts on: `--project` under the working
//! directory, or the nearest `contextful.toml` upward (`store.init.discovery`).

use anyhow::Result;
use contextful_context::project::{check_name, discover, init, Initialized, Project};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct InitArgs {
    /// The project name; its store root is `.contextful/context/<name>/`.
    name: String,
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

pub fn run(args: InitArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let what = match init(&cwd, &args.name)? {
        Initialized::Created => "created contextful.toml",
        Initialized::Adopted => "declared the project in the existing contextful.toml",
        Initialized::Unchanged => "unchanged, contextful.toml already declares it",
    };
    println!("{}: {what}; store root .contextful/context/{}/", args.name, args.name);
    Ok(())
}
