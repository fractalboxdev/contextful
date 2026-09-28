//! `topology.package` and `topology.compose`: the dependency rules `contextful-ci topology`
//! reads off `cargo metadata`, over scratch workspaces whose offending packages are local
//! stubs named like the crates the rules ban.

use crate::{manifest, repo_root, stderr, Repo};
use std::process::{Command, Output};

fn topology(root: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_contextful-ci"))
        .arg("topology")
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// A stub package at `stubs/<name>` depending on `deps`, with `features`.
fn stub(r: &Repo, name: &str, deps: &str, features: &str) {
    r.write(
        &format!("stubs/{name}/Cargo.toml"),
        &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}\n[features]\n{features}\n"),
    );
    r.write(&format!("stubs/{name}/src/lib.rs"), "");
}

fn package(r: &Repo, name: &str, deps: &str) {
    r.write(&format!("crates/{name}/Cargo.toml"), &manifest(name, deps));
    r.write(&format!("crates/{name}/src/lib.rs"), "");
    r.write(&format!("crates/{name}/tests/integration/main.rs"), "");
}

fn refused(o: &Output, error: &str) -> String {
    assert!(!o.status.success(), "expected {error}, got success: {}", stdout(o));
    let err = stderr(o);
    assert!(err.contains(error), "expected {error}, got: {err}");
    err
}

fn passes(r: &Repo) -> String {
    let o = topology(&r.root);
    assert!(o.status.success(), "{}", stderr(&o));
    stdout(&o)
}

/// `contextful-core` declaring an async runtime, a component host, an HTTP client or a columnar-format implementation raises `DomainCrateImpurity`, naming the dependency and the feature that pulled it.
// spec: topology.package.domain-impurity@d1a1edf3
#[test]
fn a_domain_crate_reaching_an_async_runtime_is_refused() {
    let r = Repo::init();
    stub(&r, "tokio", "", "");
    stub(&r, "netlayer", "tokio = { path = \"../tokio\", optional = true }\n", "runtime = [\"dep:tokio\"]\n");
    package(&r, "contextful-core", "netlayer = { path = \"../../stubs/netlayer\" }\n");
    // The clean graph passes: the optional runtime stays off.
    assert!(passes(&r).contains("contextful-core"));

    package(&r, "contextful-core", "netlayer = { path = \"../../stubs/netlayer\", features = [\"runtime\"] }\n");
    let err = refused(&topology(&r.root), "DomainCrateImpurity");
    assert!(err.contains("`tokio`") && err.contains("an async runtime"), "{err}");
    assert!(err.contains("contextful-core -> netlayer -> tokio (feature `runtime`)"), "{err}");
}

#[test]
fn a_domain_crate_reaching_a_columnar_format_or_http_client_is_refused() {
    let r = Repo::init();
    stub(&r, "arrow-array", "", "");
    stub(&r, "reqwest", "", "");
    package(&r, "contextful-core", "arrow-array = { path = \"../../stubs/arrow-array\" }\nreqwest = { path = \"../../stubs/reqwest\" }\n");
    let err = refused(&topology(&r.root), "DomainCrateImpurity");
    assert!(err.contains("`arrow-array`, a columnar-format implementation"), "{err}");
    assert!(err.contains("`reqwest`, an HTTP client"), "{err}");
}

/// A dependency edge from `contextful-core` toward an adapter crate raises `TopologyDependencyInversion`, naming both crates and the manifest line.
// spec: topology.package.dependency-direction@2a968544
#[test]
fn a_domain_crate_depending_on_an_adapter_is_refused() {
    let r = Repo::init();
    package(&r, "contextful-context", "");
    package(&r, "contextful-core", "");
    assert!(passes(&r).contains("contextful-core"));

    package(&r, "contextful-core", "serde_like = { path = \"../../stubs/serde_like\" }\ncontextful-context = { path = \"../contextful-context\" }\n");
    stub(&r, "serde_like", "", "");
    let err = refused(&topology(&r.root), "TopologyDependencyInversion");
    assert!(err.contains("`contextful-core` depends on adapter crate `contextful-context`"), "{err}");
    // The manifest's `[dependencies]` opens on line 7; the edge is its second entry.
    assert!(err.contains("crates/contextful-core/Cargo.toml:9"), "{err}");
    assert!(!err.contains("serde_like"), "a non-workspace dependency is no inversion: {err}");
}

/// `contextful-context` resolved without its `read` feature and reaching `duckdb` or `libduckdb-sys` through a normal dependency raises `StoreWriteLinksEngine`, naming the package and the path that pulled it.
// spec: topology.package.store-write-engine-free@b7105e8e
#[test]
fn a_store_adapter_linking_the_sql_engine_without_read_is_refused() {
    let r = Repo::init();
    stub(&r, "libduckdb-sys", "", "");
    stub(&r, "duckdb", "libduckdb-sys = { path = \"../libduckdb-sys\" }\n", "");
    stub(&r, "sqlkit", "duckdb = { path = \"../duckdb\" }\n", "");
    // The engine behind a default-on `read` feature, and as a dev-dependency, leaves the write half engine-free.
    package(&r, "contextful-context", "");
    r.write(
        "crates/contextful-context/Cargo.toml",
        &format!(
            "{}\n[features]\ndefault = [\"read\"]\nread = [\"dep:duckdb\"]\n\n[dev-dependencies]\nduckdb = {{ path = \"../../stubs/duckdb\" }}\n",
            manifest("contextful-context", "duckdb = { path = \"../../stubs/duckdb\", optional = true }\n")
        ),
    );
    package(&r, "contextful-memory", "contextful-context = { path = \"../contextful-context\", features = [\"read\"] }\n");
    passes(&r);

    package(&r, "contextful-memory", "contextful-context = { path = \"../contextful-context\" }\n");
    package(&r, "contextful-context", "duckdb = { path = \"../../stubs/duckdb\" }\n");
    let err = refused(&topology(&r.root), "StoreWriteLinksEngine");
    assert!(err.contains("`contextful-context` without `read` links `duckdb` through contextful-context -> duckdb"), "{err}");
    assert!(err.contains("links `libduckdb-sys` through contextful-context -> duckdb -> libduckdb-sys"), "{err}");

    package(&r, "contextful-context", "sqlkit = { path = \"../../stubs/sqlkit\" }\n");
    let err = refused(&topology(&r.root), "StoreWriteLinksEngine");
    assert!(err.contains("through contextful-context -> sqlkit -> duckdb"), "{err}");
}

/// The dependency audit raises `VendorSdkLinked`, naming the crate and the dependency, for a crate declaring a model-vendor SDK or a second outbound path to a model.
// spec: topology.compose.vendor-sdk@ba72d1fc
#[test]
fn a_crate_declaring_a_model_vendor_sdk_is_refused() {
    let r = Repo::init();
    stub(&r, "async-openai", "", "");
    passes(&r);
    package(&r, "contextful-agent", "async-openai = { path = \"../../stubs/async-openai\" }\n");
    let err = refused(&topology(&r.root), "VendorSdkLinked");
    assert!(err.contains("`contextful-agent` declares model-vendor SDK `async-openai`"), "{err}");
}

/// A JavaScript runtime linked into any profile raises `ScriptRuntimeLinked`. The authoring surface is a build-time compiler emitting a serialized, content-hashed plan.
// spec: topology.compose.script-runtime@e6fa491b
#[test]
fn a_crate_linking_a_javascript_runtime_is_refused() {
    let r = Repo::init();
    stub(&r, "boa_engine", "", "");
    stub(&r, "scripting", "boa_engine = { path = \"../boa_engine\" }\n", "");
    // A dev-dependency links into no profile.
    r.write(
        "crates/demo/Cargo.toml",
        &format!("{}\n[dev-dependencies]\nscripting = {{ path = \"../../stubs/scripting\" }}\n", manifest("demo", "")),
    );
    passes(&r);

    package(&r, "contextful-outbound", "scripting = { path = \"../../stubs/scripting\" }\n");
    let err = refused(&topology(&r.root), "ScriptRuntimeLinked");
    assert!(err.contains("`contextful-outbound` links JavaScript runtime `boa_engine` through contextful-outbound -> scripting -> boa_engine"), "{err}");
    assert!(!err.contains("`demo`"), "{err}");
}

/// A run-path crate reaches a read-path crate only through a crate carrying one of the three crossings; {{assurance.gate.crate-graph}} checks every edge.
// spec: topology.compose.undeclared-crossing@7d589387
#[test]
fn a_run_path_crate_reaching_a_read_path_crate_is_refused() {
    let r = Repo::init();
    package(&r, "contextful-core", "");
    package(&r, "contextful-context", "contextful-core = { path = \"../contextful-core\" }\n");
    package(&r, "contextful-engine", "contextful-core = { path = \"../contextful-core\" }\n");
    // Both halves reaching the domain crate is no crossing.
    passes(&r);

    package(&r, "contextful-helpers", "contextful-context = { path = \"../contextful-context\" }\n");
    package(&r, "contextful-engine", "contextful-helpers = { path = \"../contextful-helpers\" }\n");
    let err = refused(&topology(&r.root), "CrateGraphViolation");
    assert!(
        err.contains("run-path crate `contextful-engine` reaches read-path crate `contextful-context` through contextful-engine -> contextful-helpers -> contextful-context"),
        "{err}"
    );
}

/// A `contextful-policy` stub whose `exchange` feature (default when `default` is set)
/// turns on a stub `jsonwebtoken` that links a stub `rsa`.
fn policy(r: &Repo, default: bool) {
    stub(r, "rsa", "", "");
    stub(r, "jsonwebtoken", "rsa = { path = \"../rsa\" }\n", "");
    let default = if default { "default = [\"exchange\"]\n" } else { "" };
    r.write(
        "crates/contextful-policy/Cargo.toml",
        &format!(
            "{}\n[features]\n{default}exchange = [\"dep:jsonwebtoken\"]\n",
            manifest("contextful-policy", "jsonwebtoken = { path = \"../../stubs/jsonwebtoken\", optional = true }\n")
        ),
    );
    r.write("crates/contextful-policy/src/lib.rs", "");
    r.write("crates/contextful-policy/tests/integration/main.rs", "");
}

/// `contextful-policy` links the external-assertion stack, `jsonwebtoken` and `rsa`, only under its non-default `exchange` feature, which only the binary may enable, and only alongside wiring {{authority.exchange.surface}}. Another `crates/` package whose resolved graph reaches either raises `ExchangeDependencyLeak`, naming the path.
// spec: topology.package.exchange-optional@c7196189
#[test]
fn a_library_reaching_the_exchange_stack_is_refused() {
    let r = Repo::init();
    policy(&r, false);
    package(&r, "contextful-cli", "contextful-policy = { path = \"../contextful-policy\", features = [\"exchange\"] }\n");
    r.write("crates/contextful-cli/src/lib.rs", "mod auth;\n");
    r.write("crates/contextful-cli/src/auth.rs", "pub use contextful_policy::exchange;\n");
    package(&r, "contextful-context", "contextful-policy = { path = \"../contextful-policy\" }\n");
    // The binary wiring the exchange does not switch it on for a library beside it.
    passes(&r);

    package(&r, "contextful-context", "contextful-policy = { path = \"../contextful-policy\", features = [\"exchange\"] }\n");
    let err = refused(&topology(&r.root), "ExchangeDependencyLeak");
    assert!(err.contains("`contextful-context` links `jsonwebtoken` through contextful-context -> contextful-policy -> jsonwebtoken"), "{err}");
    assert!(!err.contains("`contextful-cli`"), "the binary wires the exchange: {err}");
    assert!(!err.contains("`contextful-policy` links"), "{err}");

    // A default-on feature reaches every dependent taking default features.
    package(&r, "contextful-context", "contextful-policy = { path = \"../contextful-policy\" }\n");
    policy(&r, true);
    let err = refused(&topology(&r.root), "ExchangeDependencyLeak");
    assert!(err.contains("`contextful-policy` links `jsonwebtoken` through contextful-policy -> jsonwebtoken"), "{err}");
    assert!(err.contains("`contextful-context` links `jsonwebtoken`"), "{err}");
}

#[test]
fn only_the_binary_of_this_workspace_reaches_the_exchange_stack() {
    // The stack resolves under the exchange feature, so a count of zero measures the rule, not an absent crate.
    let o = Command::new("cargo")
        .args(["tree", "-q", "-p", "contextful-policy", "--features", "exchange", "-e", "normal", "-i", "jsonwebtoken"])
        .current_dir(repo_root())
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    assert!(o.status.success() && stdout(&o).contains("jsonwebtoken"), "{}", stderr(&o));
    let o = topology(repo_root());
    let leaks = stderr(&o).lines().filter(|l| l.contains("ExchangeDependencyLeak")).count();
    contextful_eval::record::emit("policy-no-jwt", leaks as f64, 1, 0);
    assert_eq!(leaks, 0, "{}", stderr(&o));
    assert!(o.status.success(), "{}", stderr(&o));
}

#[test]
fn the_binary_enabling_the_exchange_without_wiring_it_is_refused() {
    let r = Repo::init();
    policy(&r, false);
    package(&r, "contextful-cli", "contextful-policy = { path = \"../contextful-policy\", features = [\"exchange\"] }\n");
    let err = refused(&topology(&r.root), "ExchangeDependencyLeak");
    assert!(
        err.contains("`contextful-cli` links `jsonwebtoken` through contextful-cli -> contextful-policy -> jsonwebtoken, and no source under crates/contextful-cli/src names `contextful_policy::exchange`"),
        "{err}"
    );

    r.write("crates/contextful-cli/src/lib.rs", "mod auth;\n");
    r.write("crates/contextful-cli/src/auth.rs", "pub use contextful_policy::exchange;\n");
    passes(&r);
}

/// A stub SQLite binding: `rusqlite` over `libsqlite3-sys`, each with a `bundled` feature;
/// the adapter `contextful-sqlite` over it, forwarding `bundled`; and the binary enabling it.
fn sqlite(r: &Repo, adapter_rusqlite: &str, adapter_default: &str) {
    stub(r, "libsqlite3-sys", "", "bundled = []\n");
    stub(r, "rusqlite", "libsqlite3-sys = { path = \"../libsqlite3-sys\" }\n", "bundled = [\"libsqlite3-sys/bundled\"]\n");
    r.write(
        "crates/contextful-sqlite/Cargo.toml",
        &format!(
            "{}\n[features]\n{adapter_default}bundled = [\"rusqlite/bundled\"]\n\n[dev-dependencies]\nrusqlite = {{ path = \"../../stubs/rusqlite\", features = [\"bundled\"] }}\n",
            manifest("contextful-sqlite", &format!("rusqlite = {{ path = \"../../stubs/rusqlite\"{adapter_rusqlite} }}\n"))
        ),
    );
    r.write("crates/contextful-sqlite/src/lib.rs", "");
    r.write("crates/contextful-sqlite/tests/integration/main.rs", "");
    package(r, "contextful-cli", "contextful-sqlite = { path = \"../contextful-sqlite\", features = [\"bundled\"] }\n");
}

/// `contextful-context` reaching `libsqlite3-sys` through a normal dependency, with its default features, raises `StoreLinksSqlite`, naming the path that pulled it.
// spec: topology.package.store-sqlite-free@56b044c9
#[test]
fn a_store_adapter_reaching_the_sqlite_link_package_is_refused() {
    let r = Repo::init();
    sqlite(&r, "", "");
    // A host linking its own bundled SQLite beside the store's write half resolves one copy.
    package(&r, "contextful-context", "");
    package(&r, "contextful-memory", "contextful-context = { path = \"../contextful-context\" }\ncontextful-sqlite = { path = \"../contextful-sqlite\" }\n");
    passes(&r);

    stub(&r, "catalogkit", "rusqlite = { path = \"../rusqlite\" }\n", "");
    package(&r, "contextful-context", "catalogkit = { path = \"../../stubs/catalogkit\" }\n");
    let err = refused(&topology(&r.root), "StoreLinksSqlite");
    assert!(err.contains("`contextful-context` links `libsqlite3-sys` through contextful-context -> catalogkit -> rusqlite -> libsqlite3-sys"), "{err}");
    assert!(!err.contains("SqliteLinkForced"), "a stub outside the workspace declares nothing the adapter rule reads: {err}");
}

/// `contextful-sqlite` alone declares the SQLite binding and enables no link feature itself; only `contextful-cli` turns
/// on its `bundled` feature. Any other declaration or enablement raises `SqliteLinkForced`, naming the manifest line.
// spec: topology.package.sqlite-adapter@c4f8e1cd
#[test]
fn a_sqlite_link_forced_outside_the_binary_is_refused() {
    let r = Repo::init();
    sqlite(&r, "", "");
    passes(&r);

    // A second package declaring the binding.
    package(&r, "contextful-memory", "rusqlite = { path = \"../../stubs/rusqlite\" }\n");
    let err = refused(&topology(&r.root), "SqliteLinkForced");
    assert!(err.contains("`contextful-memory` declares `rusqlite` at crates/contextful-memory/Cargo.toml:8"), "{err}");

    // A library enabling the adapter's `bundled`.
    package(&r, "contextful-memory", "contextful-sqlite = { path = \"../contextful-sqlite\", features = [\"bundled\"] }\n");
    let err = refused(&topology(&r.root), "SqliteLinkForced");
    assert!(err.contains("`contextful-memory` enables `contextful-sqlite/bundled`"), "{err}");
    assert!(!err.contains("`contextful-cli` enables"), "the binary compiles SQLite in: {err}");
    package(&r, "contextful-memory", "");

    // The adapter choosing the build itself, on its declaration or by default.
    sqlite(&r, ", features = [\"bundled\"]", "");
    let err = refused(&topology(&r.root), "SqliteLinkForced");
    assert!(err.contains("`contextful-sqlite` turns on `rusqlite/bundled` at crates/contextful-sqlite/Cargo.toml:8"), "{err}");
    sqlite(&r, "", "default = [\"bundled\"]\n");
    let err = refused(&topology(&r.root), "SqliteLinkForced");
    assert!(err.contains("`contextful-sqlite` turns on `bundled` by default"), "{err}");

    // The same choices reached through a feature chain rather than spelled on the line.
    sqlite(&r, "", "default = [\"fast\"]\nfast = [\"bundled\"]\n");
    let err = refused(&topology(&r.root), "SqliteLinkForced");
    assert!(err.contains("`contextful-sqlite` turns on `bundled` by default (default -> fast -> bundled, crates/contextful-sqlite/Cargo.toml:"), "{err}");
    sqlite(&r, "", "turbo = [\"rusqlite/bundled\"]\n");
    let err = refused(&topology(&r.root), "SqliteLinkForced");
    assert!(err.contains("`contextful-sqlite` turns on `rusqlite/bundled` through feature `turbo` (turbo -> rusqlite/bundled, crates/contextful-sqlite/Cargo.toml:"), "{err}");
    sqlite(&r, "", "");
    let fast = |default: &str| {
        let deps = "contextful-sqlite = { path = \"../contextful-sqlite\" }\n";
        r.write(
            "crates/contextful-memory/Cargo.toml",
            &format!("{}\n[features]\n{default}fast = [\"contextful-sqlite/bundled\"]\n", manifest("contextful-memory", deps)),
        );
    };
    fast("default = [\"fast\"]\n");
    let err = refused(&topology(&r.root), "SqliteLinkForced");
    assert!(err.contains("`contextful-memory` enables `contextful-sqlite/bundled` by default (default -> fast -> contextful-sqlite/bundled, crates/contextful-memory/Cargo.toml:"), "{err}");
    assert_eq!(err.matches("`contextful-memory` enables").count(), 1, "one finding per package and choice: {err}");
    fast("");
    let err = refused(&topology(&r.root), "SqliteLinkForced");
    assert!(err.contains("`contextful-memory` enables `contextful-sqlite/bundled` through feature `fast` (fast -> contextful-sqlite/bundled, crates/contextful-memory/Cargo.toml:"), "{err}");
    assert!(!err.contains("`contextful-sqlite` turns on"), "{err}");
}

/// This repository's store adapter resolves no SQLite, and its binary alone compiles one in.
#[test]
fn this_repository_links_sqlite_only_through_its_adapter() {
    let tree = |package: &str, all: bool| {
        let mut args = vec!["tree", "-q", "-p", package, "-e", "normal", "--prefix", "none"];
        if all {
            args.push("--all-features");
        }
        let o = Command::new("cargo").args(&args).current_dir(repo_root()).output().unwrap();
        assert!(o.status.success(), "{}", stderr(&o));
        stdout(&o).lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect::<Vec<_>>()
    };
    let links = tree("contextful-context", true).iter().filter(|n| *n == "libsqlite3-sys" || *n == "rusqlite").count();
    contextful_eval::record::emit("no-sqlite-link", links as f64, 1, 0);
    assert_eq!(links, 0, "the store adapter resolves the SQLite binding");
    assert!(tree("contextful-sqlite", false).iter().any(|n| n == "libsqlite3-sys"));
    let features = Command::new("cargo")
        .args(["tree", "-q", "-p", "contextful-cli", "-e", "features", "-i", "libsqlite3-sys", "--prefix", "none"])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(stdout(&features).contains("libsqlite3-sys feature \"bundled\""), "{}", stdout(&features));
}

/// Every workspace package under `crates/` or `tools/` declares `license = "Apache-2.0"`, inherited from `[workspace.package]`; a package declaring another value or none raises `PackageLicenceMissing`, naming its manifest.
// spec: assurance.build.licence-field@8babd985
#[test]
fn a_workspace_package_without_the_apache_licence_is_refused() {
    let r = Repo::init();
    // A path dependency outside the workspace carries no obligation.
    stub(&r, "unlicensed", "", "");
    package(&r, "contextful-core", "unlicensed = { path = \"../../stubs/unlicensed\" }\n");
    passes(&r);

    let bare = manifest("contextful-context", "").replace("license = \"Apache-2.0\"\n", "");
    r.write("crates/contextful-context/Cargo.toml", &bare);
    r.write("crates/contextful-context/src/lib.rs", "");
    r.write("crates/contextful-engine/Cargo.toml", &manifest("contextful-engine", "").replace("Apache-2.0", "MIT"));
    r.write("crates/contextful-engine/src/lib.rs", "");
    let err = refused(&topology(&r.root), "PackageLicenceMissing");
    assert!(err.contains("`contextful-context` declares no licence (crates/contextful-context/Cargo.toml)"), "{err}");
    assert!(err.contains("`contextful-engine` declares licence `MIT`, not `Apache-2.0` (crates/contextful-engine/Cargo.toml)"), "{err}");
    assert!(!err.contains("unlicensed"), "{err}");

    // Inheriting the field from `[workspace.package]` satisfies the rule.
    r.write("Cargo.toml", "[workspace]\nresolver = \"2\"\nmembers = [\"crates/*\"]\n\n[workspace.package]\nlicense = \"Apache-2.0\"\n");
    r.write("crates/contextful-context/Cargo.toml", &bare.replace("edition = \"2021\"\n", "edition = \"2021\"\nlicense.workspace = true\n"));
    r.write("crates/contextful-engine/Cargo.toml", &bare.replace("contextful-context", "contextful-engine").replace("edition = \"2021\"\n", "edition = \"2021\"\nlicense.workspace = true\n"));
    passes(&r);
}

#[test]
fn a_library_declaring_rsa_directly_is_refused() {
    let r = Repo::init();
    stub(&r, "rsa", "", "");
    package(&r, "contextful-agent", "rsa = { path = \"../../stubs/rsa\" }\n");
    let err = refused(&topology(&r.root), "ExchangeDependencyLeak");
    assert!(err.contains("`contextful-agent` links `rsa` through contextful-agent -> rsa"), "{err}");
}

/// The policy package without features resolves neither `jsonwebtoken` nor `rsa`, and the
/// binary resolves them exactly when its source names `contextful_policy::exchange`.
#[test]
fn this_repository_links_the_exchange_stack_only_where_the_binary_wires_it() {
    let tree = |package: &str| {
        let o = Command::new("cargo")
            .args(["tree", "-q", "-p", package, "-e", "normal", "--prefix", "none"])
            .current_dir(repo_root())
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", stderr(&o));
        stdout(&o).lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect::<Vec<_>>()
    };
    let policy = tree("contextful-policy");
    assert!(policy.iter().any(|n| n == "biscuit-auth"), "{policy:?}");
    for banned in ["jsonwebtoken", "rsa"] {
        assert!(!policy.iter().any(|n| n == banned), "contextful-policy resolves `{banned}` without `exchange`");
    }
    fn wires(dir: &std::path::Path) -> bool {
        std::fs::read_dir(dir).unwrap().flatten().any(|e| {
            let p = e.path();
            if p.is_dir() {
                wires(&p)
            } else {
                std::fs::read_to_string(&p).is_ok_and(|t| t.contains("contextful_policy::exchange"))
            }
        })
    }
    let wired = wires(&repo_root().join("crates/contextful-cli/src"));
    let links = tree("contextful-cli").iter().any(|n| n == "jsonwebtoken");
    assert_eq!(links, wired, "contextful-cli links the exchange stack: {links}; its source wires the exchange: {wired}");
}

#[test]
fn every_package_of_this_repository_declares_the_apache_licence() {
    let meta = Command::new("cargo").args(["metadata", "--no-deps", "--format-version", "1", "-q"]).current_dir(repo_root()).output().unwrap();
    let meta: serde_json::Value = serde_json::from_slice(&meta.stdout).unwrap();
    let packages = meta["packages"].as_array().unwrap();
    assert!(packages.len() >= 2);
    for p in packages {
        assert_eq!(p["license"], "Apache-2.0", "{} declares {}", p["name"], p["license"]);
    }
}

#[test]
fn this_repository_holds_to_the_dependency_rules() {
    let meta = Command::new("cargo").args(["metadata", "--no-deps", "--format-version", "1", "-q"]).current_dir(repo_root()).output().unwrap();
    let meta: serde_json::Value = serde_json::from_slice(&meta.stdout).unwrap();
    assert!(meta["packages"].as_array().unwrap().iter().any(|p| p["name"] == "contextful-core"));
    let o = topology(repo_root());
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("`contextful-core` is pure and depends on no adapter"), "{}", stdout(&o));
}

/// One Cargo workspace compiles both halves. Neither half ships as its own binary, package scope or release; "read path" and "run path" name a boundary, not a product.
// spec: topology.compose.workspace@4940e8e2
#[test]
fn one_workspace_compiles_every_package_and_ships_one_binary() {
    let root = repo_root().canonicalize().unwrap();
    let meta = Command::new("cargo").args(["metadata", "--no-deps", "--format-version", "1", "-q"]).current_dir(&root).output().unwrap();
    let meta: serde_json::Value = serde_json::from_slice(&meta.stdout).unwrap();
    assert_eq!(std::path::Path::new(meta["workspace_root"].as_str().unwrap()).canonicalize().unwrap(), root);

    // Every package directory under crates/ and tools/ is a member of the one root workspace.
    let mut dirs = Vec::new();
    for top in ["crates", "tools"] {
        for e in std::fs::read_dir(root.join(top)).unwrap() {
            let d = e.unwrap().path();
            if d.join("Cargo.toml").exists() {
                dirs.push(d.canonicalize().unwrap());
            }
        }
    }
    assert!(dirs.len() >= 2, "{dirs:?}");
    let members: Vec<std::path::PathBuf> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| std::path::Path::new(p["manifest_path"].as_str().unwrap()).parent().unwrap().canonicalize().unwrap())
        .collect();
    for d in &dirs {
        assert!(members.contains(d), "{} is outside the workspace", d.display());
    }

    // The shipped packages under crates/ build one binary, `contextful`; neither half has its own.
    let bins: Vec<String> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["manifest_path"].as_str().unwrap().contains("/crates/"))
        .flat_map(|p| p["targets"].as_array().unwrap().clone())
        .filter(|t| t["kind"].as_array().unwrap().iter().any(|k| k == "bin"))
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(bins, ["contextful"]);
}

/// A topology page whose crate-map clause states `count` and whose `## Shapes` tree lists `entries`.
fn crate_map(r: &Repo, count: &str, entries: &[&str]) {
    let tree: String = entries.iter().map(|e| format!("  {e}/   a package\n")).collect();
    r.write(
        "spec/01-topology.md",
        &format!(
            "# System topology\n\n## package\n\n- `crate-map` — {count} crates compose the workspace. `contextful-cli` is the binary.\n\n## Shapes\n\nThe workspace:\n\n```\ncrates/\n{tree}contextful.toml            the deployment declaration\n```\n\n```toml\n[features]\nedge = []\n```\n"
        ),
    );
}

/// A `crates/` package absent from the crate tree under `## Shapes`, or a {{topology.package.crate-map}} count differing from that tree's entries, raises `CrateMapDrift`, naming the package or both counts.
// spec: topology.package.crate-map-drift@bc8872b2
#[test]
fn a_crate_missing_from_the_crate_map_is_refused() {
    let r = Repo::init();
    package(&r, "contextful-core", "");
    // A tree may name a crate the workspace does not hold yet.
    crate_map(&r, "Three", &["contextful-core", "demo", "contextful-control"]);
    assert!(passes(&r).contains("crate map: 3 crates"));

    package(&r, "contextful-fs", "");
    let err = refused(&topology(&r.root), "CrateMapDrift");
    assert!(err.contains("`crates/contextful-fs` is absent from the crate tree in spec/01-topology.md"), "{err}");

    crate_map(&r, "Three", &["contextful-core", "contextful-fs", "demo", "contextful-control"]);
    let err = refused(&topology(&r.root), "CrateMapDrift");
    assert!(err.contains("`topology.package.crate-map` states 3 crates; the crate tree lists 4"), "{err}");
    assert!(!err.contains("is absent"), "{err}");

    crate_map(&r, "Four", &["contextful-core", "contextful-fs", "demo", "contextful-control"]);
    passes(&r);
}

/// Sixteen crates compose the workspace. `contextful-cli` is the binary and wires every adapter per profile by dependency injection.
// spec: topology.package.crate-map@3bb0f4bc
#[test]
fn this_repository_crate_map_names_every_crate() {
    let o = topology(repo_root());
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("crate map: 16 crates"), "{}", stdout(&o));
}
