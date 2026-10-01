//! `store.init`: the declaration file an init writes, and finding it from a working directory.

use contextful_context::project::{declare_posture, discover, init, Initialized, Project, DECLARATION_FILE};
use contextful_core::issue::AuthoringPosture;
use contextful_context::ContextError;
use contextful_core::store::StoreError;
use std::fs;

fn store_err(e: ContextError) -> StoreError {
    e.store().cloned().unwrap_or_else(|| panic!("expected a store refusal, got {e}"))
}

/// `contextful init <name>` writes `contextful.toml` in the working directory declaring `[project]` with `name = "<name>"`, and creates the store root beside it.
// spec: store.init.declaration-file@715513c7
#[test]
fn an_init_writes_the_declaration_and_the_store_root() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(init(dir.path(), "research").unwrap(), Initialized::Created);
    let text = fs::read_to_string(dir.path().join(DECLARATION_FILE)).unwrap();
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(parsed["project"]["name"].as_str(), Some("research"));
    assert!(dir.path().join(".contextful/context/research").is_dir());
}

/// A project name that is not path-safe segments raises `StoreProjectNameInvalid` before any file is read or written.
// spec: store.init.name-shape@93c80b33
#[test]
fn a_traversing_or_unsafe_name_refuses_before_any_write() {
    for bad in ["", "..", "a/../b", "a b", "/abs", "x/", "caf\u{e9}"] {
        let dir = tempfile::tempdir().unwrap();
        match store_err(init(dir.path(), bad).unwrap_err()) {
            StoreError::StoreProjectNameInvalid(m) => assert!(m.contains(&format!("`{bad}`")), "{m}"),
            other => panic!("{bad:?}: {other}"),
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "{bad:?} wrote a file");
    }
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(init(dir.path(), "team/research").unwrap(), Initialized::Created);
    assert!(dir.path().join(".contextful/context/team/research").is_dir());
}

/// An init against a `contextful.toml` already declaring the same name rewrites nothing and succeeds.
// spec: store.init.repeat@db5b16da
#[test]
fn a_repeated_init_rewrites_nothing() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path(), "research").unwrap();
    let path = dir.path().join(DECLARATION_FILE);
    let edited = format!("{}\n[[pipeline.tables]]\nname = \"filings\"\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, &edited).unwrap();
    assert_eq!(init(dir.path(), "research").unwrap(), Initialized::Unchanged);
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
}

/// An init against a `contextful.toml` declaring no `project` key appends the `[project]` table and keeps every existing byte.
// spec: store.init.adopt@5091180c
#[test]
fn an_init_adopts_an_existing_declaration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(DECLARATION_FILE);
    let before = "[[pipeline.tables]]\nname = \"filings\"\nprimary_key = [\"doc\"]";
    fs::write(&path, before).unwrap();
    assert_eq!(init(dir.path(), "research").unwrap(), Initialized::Adopted);
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.starts_with(before), "{after}");
    let parsed: toml::Value = toml::from_str(&after).unwrap();
    assert_eq!(parsed["project"]["name"].as_str(), Some("research"));
    assert_eq!(parsed["pipeline"]["tables"][0]["name"].as_str(), Some("filings"));
    assert_eq!(discover(dir.path()).unwrap().name, "research");
}

/// An init against a `contextful.toml` declaring another name, or no string name, raises `StoreProjectConflict`, naming the declared name when one exists and the init's name, and writes nothing.
// spec: store.init.name-conflict@6f87ee7d
#[test]
fn an_init_naming_another_project_refuses() {
    let cases = [("[project]\nname = \"research\"\n", Some("`research`")), ("[project]\nowner = \"ops\"\n", None), ("project = \"research\"\n", None)];
    for (existing, declared) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DECLARATION_FILE);
        fs::write(&path, existing).unwrap();
        match store_err(init(dir.path(), "archive").unwrap_err()) {
            StoreError::StoreProjectConflict(m) => {
                assert!(m.contains("`archive`"), "{m}");
                if let Some(name) = declared {
                    assert!(m.contains(name), "{m}");
                }
            }
            other => panic!("{existing:?}: {other}"),
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), existing);
        assert!(!dir.path().join(".contextful").exists());
    }
}

/// A command given no `--project` reads the nearest `contextful.toml` in the working directory or an ancestor and bases every project path on its directory.
// spec: store.init.discovery@7bd9957a
#[test]
fn discovery_takes_the_nearest_declaration_upward() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path(), "research").unwrap();
    let deep = dir.path().join("notes/2030/q1");
    fs::create_dir_all(&deep).unwrap();
    let found = discover(&deep).unwrap();
    assert_eq!(found, Project { dir: dir.path().to_path_buf(), name: "research".into() });
    assert_eq!(found.declaration(), dir.path().join(DECLARATION_FILE));
    assert_eq!(found.store_root(), dir.path().join(".contextful/context/research"));

    // A nearer declaration wins over the outer one.
    init(&dir.path().join("notes"), "notes").unwrap();
    let found = discover(&deep).unwrap();
    assert_eq!(found.dir, dir.path().join("notes"));
    assert_eq!(found.name, "notes");
}

/// A working directory whose ancestors hold no `contextful.toml`, or whose nearest one names no project, raises `StoreProjectUndiscovered`.
// spec: store.init.undiscovered@73f2ca58
#[test]
fn discovery_without_a_named_declaration_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("inner");
    fs::create_dir_all(&inner).unwrap();
    match store_err(discover(&inner).unwrap_err()) {
        StoreError::StoreProjectUndiscovered(m) => assert!(m.contains(&inner.display().to_string()), "{m}"),
        other => panic!("{other}"),
    }
    // A nameless nearest declaration refuses rather than reaching past it.
    fs::write(inner.join(DECLARATION_FILE), "[[pipeline.tables]]\nname = \"filings\"\n").unwrap();
    match store_err(discover(&inner).unwrap_err()) {
        StoreError::StoreProjectUndiscovered(m) => assert!(m.contains(DECLARATION_FILE), "{m}"),
        other => panic!("{other}"),
    }
    // A declared name outside the path-safe shape refuses as an invalid name.
    fs::write(inner.join(DECLARATION_FILE), "[project]\nname = \"../escape\"\n").unwrap();
    assert!(matches!(store_err(discover(&inner).unwrap_err()), StoreError::StoreProjectNameInvalid(_)));
}

/// The project paths are the store root, the run state `.contextful/run/<project>/` and the memory pass state `.contextful/memory/<project>/`.
// spec: store.init.project-paths@5a23a3db
#[test]
fn every_project_path_is_based_on_the_project_directory() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path(), "research").unwrap();
    let p = discover(dir.path()).unwrap();
    assert_eq!(p.store_root(), dir.path().join(".contextful/context/research"));
    assert_eq!(p.run_dir(), dir.path().join(".contextful/run/research"));
    assert_eq!(p.memory_dir(), dir.path().join(".contextful/memory/research"));
}

/// `--authoring-posture` prepends a top-level `authoring_posture` to a declaration declaring none, keeping every other byte; a declared posture stands.
#[test]
fn an_init_posture_is_prepended_once_and_a_declared_one_stands() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(DECLARATION_FILE);
    init(dir.path(), "research").unwrap();
    let before = format!("{}\n[[pipeline.tables]]\nname = \"filings\"\n", fs::read_to_string(&path).unwrap());
    fs::write(&path, &before).unwrap();

    assert!(declare_posture(dir.path(), AuthoringPosture::PerRequest).unwrap());
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.ends_with(&before), "{after}");
    assert_eq!(AuthoringPosture::from_manifest(&after).unwrap(), AuthoringPosture::PerRequest);
    let parsed: toml::Value = toml::from_str(&after).unwrap();
    assert_eq!(parsed["project"]["name"].as_str(), Some("research"));
    assert_eq!(parsed["pipeline"]["tables"][0]["name"].as_str(), Some("filings"));

    // A declared posture stands against another flag, and a repeat rewrites nothing.
    assert!(!declare_posture(dir.path(), AuthoringPosture::Session).unwrap());
    assert!(!declare_posture(dir.path(), AuthoringPosture::PerRequest).unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), after);
}
