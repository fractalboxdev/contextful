//! `store.init`: a project's declaration file, and the project a working directory
//! belongs to.

use crate::error::{IoPath, Result};
use contextful_core::issue::AuthoringPosture;
use contextful_core::store::lay_out::{is_path_segment, store_root, STORE_ID_FILE};
use contextful_core::store::StoreError;
use std::fs;
use std::path::{Path, PathBuf};

/// The project's declaration file (`store.init.declaration-file`).
pub const DECLARATION_FILE: &str = "contextful.toml";

/// The audit pseudonym key has independent entropy and stays outside the synced store.
/// Unix creation uses owner-only permissions; Windows inherits the configured directory ACL.
pub fn audit_key(store: &crate::Store, project: &Project) -> Result<[u8; 32]> {
    load_audit_key(store, project, AuditKeyPurpose::Create)
}

pub(crate) fn existing_audit_key(store: &crate::Store, project: &Project) -> Result<[u8; 32]> {
    load_audit_key(store, project, AuditKeyPurpose::Existing)
}

#[derive(Clone, Copy)]
enum AuditKeyPurpose { Create, Existing }

fn load_audit_key(store: &crate::Store, project: &Project, purpose: AuditKeyPurpose) -> Result<[u8; 32]> {
    use std::io::{Read, Write};
    let incomplete = |why: &str| crate::ContextError::from(contextful_core::disclosure::erase::ErasureError::ErasureTransactionIncomplete(why.to_string()));
    if store.root() != project.store_root() { return Err(incomplete("the audit key project disagrees with the store")); }
    store.with_frontier(|bound| {
        let directory = project.dir.join(".contextful");
        let metadata = fs::symlink_metadata(&directory).at(&directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() { return Err(incomplete("the audit key directory is not owned project content")); }
        let path = directory.join("audit.key");
        if !path.try_exists().at(&path)? && fs::symlink_metadata(&path).is_err() {
            if matches!(purpose, AuditKeyPurpose::Existing) { return Err(incomplete("the canonical project audit key is absent")); }
            if bound.metadata().read_optional(&bound.root().join(crate::erasure_frontier::FRONTIER_FILE))?.is_some() {
                return Err(incomplete("a published erasure has no persisted project audit key"));
            }
            let mut bytes = [0; 32];
            getrandom::fill(&mut bytes).map_err(|_| incomplete("audit key entropy is unavailable"))?;
            let staged = contextful_fs::tmp_sibling(&path);
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)] {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let result = (|| -> Result<()> {
                let mut file = options.open(&staged).at(&staged)?;
                file.write_all(&bytes).at(&staged)?;
                file.sync_all().at(&staged)?;
                contextful_fs::create_exclusive(&staged, &path).at(&path)?;
                contextful_fs::open_dir_for_sync(&directory).at(&directory)?.sync_all().at(&directory)
            })();
            if result.is_err() { let _ = fs::remove_file(&staged); }
            result?;
        }
        let metadata = fs::symlink_metadata(&path).at(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != 32 { return Err(incomplete("the audit key is not a complete owned key file")); }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 { return Err(incomplete("the audit key permissions expose it beyond its owner")); }
        }
        let mut file = fs::File::open(&path).at(&path)?;
        if file.metadata().at(&path)?.len() != 32 || !contextful_fs::names_file(&path, &file).at(&path)? || fs::symlink_metadata(&path).at(&path)?.file_type().is_symlink() {
            return Err(incomplete("the audit key identity changed while opening it"));
        }
        let mut bytes = [0; 32];
        file.read_exact(&mut bytes).at(&path)?;
        let mut extra = [0; 1];
        if file.read(&mut extra).at(&path)? != 0 { return Err(incomplete("the audit key length changed while reading it")); }
        Ok(bytes)
    })
}

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

    /// The read audit chain `.contextful/audit/`, outside the synced store root
    /// (`disclosure.record.read-chain`).
    pub fn audit_dir(&self) -> PathBuf {
        self.dir.join(".contextful/audit")
    }

    /// The memory pass state `.contextful/memory/<project>/`.
    pub fn memory_dir(&self) -> PathBuf {
        self.dir.join(".contextful/memory").join(&self.name)
    }
}

/// Read one persistent UUID from the synced store root.
pub fn store_id(root: &Path) -> Result<String> {
    let path = root.join(STORE_ID_FILE);
    let raw = fs::read_to_string(&path).at(&path)?;
    let id = raw.trim();
    let valid = id.len() == 36 && id.bytes().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        14 => b == b'4',
        19 => matches!(b, b'8' | b'9' | b'a' | b'b'),
        _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
    });
    if !valid {
        return Err(StoreError::StoreIdentityInvalid(format!("`{}` holds no canonical version-4 UUID", path.display())).into());
    }
    Ok(id.to_string())
}

/// Keep an existing UUID, or publish a new one by exclusive creation.
pub fn ensure_store_id(root: &Path) -> Result<String> {
    match store_id(root) {
        Ok(id) => return Ok(id),
        Err(crate::ContextError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if !root.is_dir() {
        return Err(StoreError::StoreIdentityInvalid(format!("`{}` is not an initialized store root", root.display())).into());
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| crate::ContextError::Invalid(format!("store UUID entropy unavailable: {e}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = |slice: &[u8]| slice.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let id = format!("{}-{}-{}-{}-{}", hex(&bytes[..4]), hex(&bytes[4..6]), hex(&bytes[6..8]), hex(&bytes[8..10]), hex(&bytes[10..]));
    crate::store::create_new_file(&root.join(STORE_ID_FILE), format!("{id}\n").as_bytes())?;
    store_id(root)
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
    ensure_store_id(&root)?;
    Ok(outcome)
}

/// `contextful init <name> --authoring-posture <posture>` (`store.init.posture`): prepend a
/// top-level `authoring_posture` to the declaration in `dir` when it declares none,
/// keeping every other byte. A declared posture stands. Returns whether the file changed.
pub fn declare_posture(dir: &Path, posture: AuthoringPosture) -> Result<bool> {
    let path = dir.join(DECLARATION_FILE);
    let text = fs::read_to_string(&path).at(&path)?;
    if declared_posture(&path, &text)?.is_some() {
        return Ok(false);
    }
    let staged = dir.join(format!(".{DECLARATION_FILE}.posture"));
    let mut file = fs::File::create(&staged).at(&staged)?;
    let line = format!("{} = \"{}\"\n", AuthoringPosture::KEY, posture.as_str());
    std::io::Write::write_all(&mut file, format!("{line}{text}").as_bytes()).at(&staged)?;
    file.sync_all().at(&staged)?;
    fs::rename(&staged, &path).at(&path)?;
    Ok(true)
}

/// The `authoring_posture` value a declaration spells, whatever it names.
pub fn declared_posture(path: &Path, text: &str) -> Result<Option<String>> {
    let value: toml::Value =
        toml::from_str(text).map_err(|e| crate::ContextError::Invalid(format!("{}: {}", path.display(), e.message())))?;
    Ok(value.get(AuthoringPosture::KEY).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string)))
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
