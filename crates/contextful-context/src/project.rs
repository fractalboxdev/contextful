//! `store.init`: a project's declaration file, and the project a working directory
//! belongs to.

use crate::error::{IoPath, Result};
use contextful_core::store::lay_out::{is_path_segment, store_root};
use contextful_core::store::StoreError;
use std::fs;
use std::path::{Path, PathBuf};

/// The project's declaration file (`store.init.declaration-file`).
pub const DECLARATION_FILE: &str = "contextful.toml";

/// A project and the directory every project path is based on (`store.init.project-paths`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub dir: PathBuf,
    pub name: String,
}

impl Project {
    /// The declaration file in the project's directory.
    pub fn declaration(&self) -> PathBuf {
        self.dir.join(DECLARATION_FILE)
    }

    /// The store root `.contextful/context/<project>/` (`store.lay-out.store-root`).
    pub fn store_root(&self) -> PathBuf {
        self.dir.join(store_root(&self.name))
    }

    /// The run state `.contextful/run/<project>/`.
    pub fn run_dir(&self) -> PathBuf {
        self.dir.join(".contextful/run").join(&self.name)
    }

    /// The memory pass state `.contextful/memory/<project>/`.
    pub fn memory_dir(&self) -> PathBuf {
        self.dir.join(".contextful/memory").join(&self.name)
    }
}

/// What an init did to the directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initialized {
    /// No declaration existed; one was written.
    Created,
    /// A declaration naming no project gained the `[project]` table.
    Adopted,
    /// The declaration already named this project; nothing was rewritten.
    Unchanged,
}

/// Refuse a project name outside path-safe `/`-separated segments (`store.init.name-shape`).
pub fn check_name(name: &str) -> Result<()> {
    if name.split('/').all(is_path_segment) {
        Ok(())
    } else {
        Err(StoreError::StoreProjectNameInvalid(format!("project name `{name}` is not `/`-separated segments of [A-Za-z0-9._-]")).into())
    }
}

/// What a declaration's `project` key says.
enum Declared {
    Absent,
    Named(String),
    /// A `project` key without a string `name`.
    Nameless,
}

fn declared(path: &Path, text: &str) -> Result<Declared> {
    let value: toml::Value =
        toml::from_str(text).map_err(|e| crate::ContextError::Invalid(format!("{}: {}", path.display(), e.message())))?;
    Ok(match value.get("project") {
        None => Declared::Absent,
        Some(p) => match p.get("name").and_then(toml::Value::as_str) {
            Some(n) => Declared::Named(n.to_string()),
            None => Declared::Nameless,
        },
    })
}

/// `contextful init <name>` in `dir`: write or adopt the declaration, then create the
/// store root. A declaration naming the same project is left as it stands
/// (`store.init.repeat`); one naming another refuses before any write
/// (`store.init.name-conflict`).
pub fn init(dir: &Path, name: &str) -> Result<Initialized> {
    check_name(name)?;
    let path = dir.join(DECLARATION_FILE);
    let table = format!("[project]\nname = \"{name}\"\n");
    let outcome = match fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_new(&path, &table)?;
            Initialized::Created
        }
        Err(e) => return Err(e).at(&path),
        Ok(text) => match declared(&path, &text)? {
            Declared::Named(n) if n == name => Initialized::Unchanged,
            Declared::Named(n) => {
                return Err(StoreError::StoreProjectConflict(format!(
                    "`{}` declares project `{n}`; init names `{name}`",
                    path.display()
                ))
                .into())
            }
            Declared::Nameless => {
                return Err(StoreError::StoreProjectConflict(format!(
                    "`{}` declares a `project` key with no string `name`; init names `{name}`",
                    path.display()
                ))
                .into())
            }
            Declared::Absent => {
                let sep = match text.as_str() {
                    "" => "",
                    t if t.ends_with('\n') => "\n",
                    _ => "\n\n",
                };
                let mut file = fs::OpenOptions::new().append(true).open(&path).at(&path)?;
                std::io::Write::write_all(&mut file, format!("{sep}{table}").as_bytes()).at(&path)?;
                file.sync_all().at(&path)?;
                Initialized::Adopted
            }
        },
    };
    let root = dir.join(store_root(name));
    fs::create_dir_all(&root).at(&root)?;
    Ok(outcome)
}

/// Create the declaration, never replacing one written between the read and the write.
fn write_new(path: &Path, text: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path).at(path)?;
    std::io::Write::write_all(&mut file, text.as_bytes()).at(path)?;
    file.sync_all().at(path)
}

/// The project `start` belongs to: the nearest `contextful.toml` in `start` or an
/// ancestor, and its `[project] name` (`store.init.discovery`). None, or a nearest one
/// naming no project, refuses (`store.init.undiscovered`).
pub fn discover(start: &Path) -> Result<Project> {
    for dir in start.ancestors() {
        let path = dir.join(DECLARATION_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).at(&path),
        };
        return match declared(&path, &text)? {
            Declared::Named(name) => {
                check_name(&name)?;
                Ok(Project { dir: dir.to_path_buf(), name })
            }
            Declared::Absent | Declared::Nameless => Err(StoreError::StoreProjectUndiscovered(format!(
                "from `{}`: the nearest `{}` declares no `[project] name`; run `contextful init <name>` there or pass `--project`",
                start.display(),
                path.display()
            ))
            .into()),
        };
    }
    Err(StoreError::StoreProjectUndiscovered(format!(
        "no {DECLARATION_FILE} in `{}` or any ancestor; run `contextful init <name>` or pass `--project`",
        start.display()
    ))
    .into())
}
