//! `topology.package` and `topology.compose`: the dependency rules `contextful-ci topology`
//! reads off `cargo metadata`, over scratch workspaces whose offending packages are local
//! stubs named like the crates the rules ban.

use crate::{manifest, repo_root, stderr, Repo};
use std::process::{Command, Output};

/// `contextful-ci topology` over `root`. A scratch workspace first has its `Cargo.lock`
/// brought in line with its manifests, since the rules resolve every graph `--locked`.
fn topology(root: &std::path::Path) -> Output {
    if root != repo_root() {
        lock(root);
    }
    topology_as_is(root)
}

/// Write the `Cargo.lock` the manifests under `root` resolve to.
fn lock(root: &std::path::Path) {
    let o = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "-q"])
        .current_dir(root)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
}

/// `contextful-ci topology` over `root` with its `Cargo.lock` left as it stands.
fn topology_as_is(root: &std::path::Path) -> Output {
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

/// `contextful-context` resolved without its `read` feature, on any target, and reaching `duckdb`, `libduckdb-sys`, `libsqlite3-sys`, an async runtime or an HTTP or TLS stack through a normal dependency raises `StoreWriteLinksEngine`, naming the package and path.
// spec: topology.package.store-write-engine-free@e0b06723
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

    // An async runtime or an HTTP or TLS stack in the write half is refused alike.
    stub(&r, "tokio", "", "");
    stub(&r, "rustls", "", "");
    stub(&r, "hyper", "rustls = { path = \"../rustls\" }\n", "");
    package(&r, "contextful-context", "tokio = { path = \"../../stubs/tokio\" }\nhyper = { path = \"../../stubs/hyper\" }\n");
    let err = refused(&topology(&r.root), "StoreWriteLinksEngine");
    assert!(err.contains("`contextful-context` without `read` links `tokio` through contextful-context -> tokio"), "{err}");
    assert!(err.contains("links `rustls` through contextful-context -> hyper -> rustls"), "{err}");

    // SQLite in the write half is refused and counted on every target, the host's or not.
    stub(&r, "libsqlite3-sys", "", "");
    stub(&r, "catalogkit", "libsqlite3-sys = { path = \"../libsqlite3-sys\" }\n", "");
    package(&r, "contextful-context", "");
    r.write(
        "crates/contextful-context/Cargo.toml",
        &format!(
            "{}\n[target.'cfg(windows)'.dependencies]\ncatalogkit = {{ path = \"../../stubs/catalogkit\" }}\n",
            manifest("contextful-context", "")
        ),
    );
    let o = topology(&r.root);
    assert!(stdout(&o).contains(", 1 forbidden"), "{}", stdout(&o));
    let err = refused(&o, "StoreWriteLinksEngine");
    assert!(err.contains("`contextful-context` without `read` links `libsqlite3-sys` through contextful-context -> catalogkit -> libsqlite3-sys"), "{err}");
}

/// `contextful-outbound` resolved without its `transport-ureq` feature and reaching `ureq`, `hyper`, `reqwest`, `rustls` or `curl` through a normal dependency raises `TransportStackLinked`, naming the path that pulled it.
// spec: topology.package.transport-optional@80817ba0
#[test]
fn an_outbound_crate_linking_an_http_stack_without_its_transport_feature_is_refused() {
    let r = Repo::init();
    stub(&r, "rustls", "", "");
    stub(&r, "ureq", "rustls = { path = \"../rustls\" }\n", "");
    stub(&r, "httpkit", "rustls = { path = \"../rustls\" }\n", "");
    // The HTTP stack behind a default-on `transport-ureq` feature leaves the crate stack-free with it off.
    package(&r, "contextful-outbound", "");
    r.write(
        "crates/contextful-outbound/Cargo.toml",
        &format!(
            "{}\n[features]\ndefault = [\"transport-ureq\"]\ntransport-ureq = [\"dep:ureq\"]\n",
            manifest("contextful-outbound", "ureq = { path = \"../../stubs/ureq\", optional = true }\n")
        ),
    );
    package(&r, "contextful-connectors", "contextful-outbound = { path = \"../contextful-outbound\" }\n");
    passes(&r);

    package(&r, "contextful-outbound", "ureq = { path = \"../../stubs/ureq\" }\n");
    let err = refused(&topology(&r.root), "TransportStackLinked");
    assert!(err.contains("`contextful-outbound` without `transport-ureq` links `ureq` through contextful-outbound -> ureq"), "{err}");
    assert!(err.contains("links `rustls` through contextful-outbound -> ureq -> rustls"), "{err}");

    package(&r, "contextful-outbound", "httpkit = { path = \"../../stubs/httpkit\" }\n");
    let err = refused(&topology(&r.root), "TransportStackLinked");
    assert!(err.contains("through contextful-outbound -> httpkit -> rustls"), "{err}");
}

/// `contextful-sync` resolved without its `s3-sync` feature and reaching `ureq`, `hyper`, `reqwest`, `rustls` or `curl`
/// through a normal dependency raises `SyncStackLinked`, naming the path that pulled it.
// spec: topology.package.s3-sync-optional@58f37dde
#[test]
fn a_sync_crate_linking_an_http_stack_without_its_s3_feature_is_refused() {
    let r = Repo::init();
    stub(&r, "rustls", "", "");
    stub(&r, "ureq", "rustls = { path = \"../rustls\" }\n", "");
    stub(&r, "signer", "", "");
    // The S3 adapter's client behind the non-default `s3-sync` feature leaves the crate stack-free with it off.
    r.write(
        "crates/contextful-sync/Cargo.toml",
        &format!(
            "{}\n[features]\ns3-sync = [\"dep:ureq\"]\n",
            manifest("contextful-sync", "ureq = { path = \"../../stubs/ureq\", optional = true }\nsigner = { path = \"../../stubs/signer\" }\n")
        ),
    );
    r.write("crates/contextful-sync/src/lib.rs", "");
    r.write("crates/contextful-sync/tests/integration/main.rs", "");
    passes(&r);

    package(&r, "contextful-sync", "ureq = { path = \"../../stubs/ureq\" }\n");
    let err = refused(&topology(&r.root), "SyncStackLinked");
    assert!(err.contains("`contextful-sync` without `s3-sync` links `ureq` through contextful-sync -> ureq"), "{err}");
    assert!(err.contains("links `rustls` through contextful-sync -> ureq -> rustls"), "{err}");
}

/// `contextful-decode` reaching `contextful-outbound`, `ureq`, `hyper`, `reqwest`, `rustls`, `curl` or `tokio` through a normal dependency raises `DecodeLinksNetwork`, naming the path that pulled it.
// spec: topology.package.decode-network-free@c8772a6c
#[test]
fn a_decode_package_linking_the_network_stack_is_refused() {
    let r = Repo::init();
    stub(&r, "rustls", "", "");
    stub(&r, "ureq", "rustls = { path = \"../rustls\" }\n", "");
    stub(&r, "tokio", "", "");
    stub(&r, "zipkit", "tokio = { path = \"../tokio\" }\n", "");
    package(&r, "contextful-core", "");
    // A network stack reached as a dev-dependency, or by a dependent, leaves the decoders network-free.
    r.write(
        "crates/contextful-decode/Cargo.toml",
        &format!(
            "{}\n[dev-dependencies]\nureq = {{ path = \"../../stubs/ureq\" }}\n",
            manifest("contextful-decode", "contextful-core = { path = \"../contextful-core\" }\n")
        ),
    );
    r.write("crates/contextful-decode/src/lib.rs", "");
    r.write("crates/contextful-decode/tests/integration/main.rs", "");
    package(&r, "contextful-connectors", "contextful-decode = { path = \"../contextful-decode\" }\nureq = { path = \"../../stubs/ureq\" }\n");
    passes(&r);

    package(&r, "contextful-outbound", "ureq = { path = \"../../stubs/ureq\" }\n");
    package(&r, "contextful-decode", "contextful-outbound = { path = \"../contextful-outbound\" }\n");
    let err = refused(&topology(&r.root), "DecodeLinksNetwork");
    assert!(err.contains("`contextful-decode` links `contextful-outbound` through contextful-decode -> contextful-outbound\n"), "{err}");
    assert!(err.contains("links `rustls` through contextful-decode -> contextful-outbound -> ureq -> rustls"), "{err}");

    package(&r, "contextful-decode", "zipkit = { path = \"../../stubs/zipkit\" }\n");
    let err = refused(&topology(&r.root), "DecodeLinksNetwork");
    assert!(err.contains("`contextful-decode` links `tokio` through contextful-decode -> zipkit -> tokio"), "{err}");
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

/// `contextful-context` reaching `libsqlite3-sys` through a normal dependency, with its default features, on any target, raises `StoreLinksSqlite`, naming the path that pulled it.
// spec: topology.package.store-sqlite-free@ae5daae8
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

    // A link behind another target's `cfg` is refused alike.
    package(&r, "contextful-context", "");
    r.write(
        "crates/contextful-context/Cargo.toml",
        &format!(
            "{}\n[target.'cfg(windows)'.dependencies]\ncatalogkit = {{ path = \"../../stubs/catalogkit\" }}\n",
            manifest("contextful-context", "")
        ),
    );
    let err = refused(&topology(&r.root), "StoreLinksSqlite");
    assert!(err.contains("`contextful-context` links `libsqlite3-sys` through contextful-context -> catalogkit -> rusqlite -> libsqlite3-sys"), "{err}");
}

/// `contextful-sqlite` alone declares the SQLite binding as a normal dependency and enables no link feature itself; only
/// `contextful-cli` turns on its `bundled` feature. Any other normal-dependency declaration or enablement raises
/// `SqliteLinkForced`, naming the manifest line.
// spec: topology.package.sqlite-adapter@0b106b19
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

    // A dev- or build-dependency on the binding, `bundled` included, stays out of every profile's normal graph.
    let binding = "rusqlite = { path = \"../../stubs/rusqlite\", features = [\"bundled\"] }\n";
    r.write(
        "crates/contextful-memory/Cargo.toml",
        &format!("{}\n[dev-dependencies]\n{binding}\n[build-dependencies]\n{binding}", manifest("contextful-memory", "")),
    );
    passes(&r);
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

/// `contextful-outbound` built with every feature off resolves no package of the HTTP stack.
#[test]
fn this_repository_outbound_crate_links_no_http_stack_without_its_transport() {
    let out = Command::new("cargo")
        .args(["tree", "-q", "-p", "contextful-outbound", "--no-default-features", "-e", "normal", "--prefix", "none", "--format", "{p}"])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let tree = String::from_utf8_lossy(&out.stdout).to_string();
    let mut linked: Vec<&str> = tree
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| ["ureq", "hyper", "reqwest", "rustls", "curl"].contains(name))
        .collect();
    linked.dedup();
    contextful_eval::record::emit("network-free-runtime", linked.len() as f64, tree.lines().count() as u64, 0);
    assert!(linked.is_empty(), "{linked:?}");
    assert!(tree.lines().any(|l| l.starts_with("contextful-core ")), "{tree}");
}

/// `contextful-decode` resolves no mediated-request crate and no package of the network stack.
#[test]
fn this_repository_decode_package_links_no_network_stack() {
    let out = Command::new("cargo")
        .args(["tree", "-q", "-p", "contextful-decode", "-e", "normal", "--prefix", "none", "--format", "{p}"])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let tree = String::from_utf8_lossy(&out.stdout).to_string();
    let mut linked: Vec<&str> = tree
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| ["contextful-outbound", "ureq", "hyper", "reqwest", "rustls", "curl", "tokio"].contains(name))
        .collect();
    linked.sort_unstable();
    linked.dedup();
    contextful_eval::record::emit("network-free-decoder", linked.len() as f64, tree.lines().count() as u64, 0);
    assert!(linked.is_empty(), "{linked:?}");
    assert!(tree.lines().any(|l| l.starts_with("contextful-core ")), "{tree}");
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

/// Seventeen crates compose the workspace. `contextful-cli` is the binary and wires every adapter per profile by dependency injection.
// spec: topology.package.crate-map@398fd784
#[test]
fn this_repository_crate_map_names_every_crate() {
    let o = topology(repo_root());
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("crate map: 17 crates"), "{}", stdout(&o));
}

/// The crate-graph stage runs the dependency rules and refuses a run-path crate reaching a
/// read-path crate outside the crossings.
// spec: assurance.gate.crate-graph@98b70ec8
#[test]
fn the_crate_graph_stage_refuses_an_undeclared_crossing() {
    let stages = Command::new(env!("CARGO_BIN_EXE_contextful-ci")).arg("stages").output().unwrap();
    assert!(stdout(&stages).lines().any(|l| l == "crate-graph"), "{}", stdout(&stages));

    let r = Repo::init();
    package(&r, "contextful-core", "");
    package(&r, "contextful-context", "contextful-core = { path = \"../contextful-core\" }\n");
    package(&r, "contextful-engine", "contextful-context = { path = \"../contextful-context\" }\n");
    lock(&r.root);
    r.commit("crossing");
    let o = r.gate(&["--stage", "crate-graph"]);
    let err = refused(&o, "CrateGraphViolation");
    assert!(err.contains("run-path crate `contextful-engine` reaches read-path crate `contextful-context`"), "{err}");
}

/// A `Cargo.lock` the manifests disagree with stops the rules before any runs, and so does
/// one rewritten before the stage starts, as `cargo run` without `--locked` rewrites a
/// stale one: it differs from the committed lock.
// spec: assurance.gate.locked-resolve@08cdd401
#[test]
fn a_lock_file_behind_its_manifests_stops_the_crate_graph() {
    let r = Repo::init();
    package(&r, "contextful-core", "");
    lock(&r.root);
    assert!(stdout(&topology_as_is(&r.root)).contains("contextful-core"));

    stub(&r, "leftpad", "", "");
    package(&r, "contextful-core", "leftpad = { path = \"../../stubs/leftpad\" }\n");
    let o = topology_as_is(&r.root);
    assert!(!o.status.success(), "a stale lock file resolved: {}", stdout(&o));
    assert!(stderr(&o).contains("--locked"), "{}", stderr(&o));
    // The rules left the lock file as it stood.
    let lock_file = std::fs::read_to_string(r.root.join("Cargo.lock")).unwrap();
    assert!(!lock_file.contains("leftpad"), "{lock_file}");

    r.commit("a dependency the committed lock does not record");
    lock(&r.root);
    let o = r.gate(&["--stage", "crate-graph"]);
    assert!(!o.status.success(), "a rewritten lock file passed: {}", stdout(&o));
    assert!(stderr(&o).contains("`Cargo.lock` differs from the committed one"), "{}", stderr(&o));
}

/// This repository's store write half resolves no forbidden package, and its unique
/// package count across every target records against the ledger's ceiling.
#[test]
fn this_repository_store_write_half_links_no_forbidden_package() {
    let o = topology(repo_root());
    let out = stdout(&o);
    let line = out
        .lines()
        .find_map(|l| l.strip_prefix("store write half: "))
        .unwrap_or_else(|| panic!("no store write half line: {out}{}", stderr(&o)));
    let figures: Vec<u64> = line.split_whitespace().filter_map(|w| w.parse().ok()).collect();
    let [unique, forbidden] = figures[..] else { panic!("{line}") };
    contextful_eval::record::emit("store-write-deny-set", forbidden as f64, unique, 0);
    contextful_eval::record::emit("store-write-package-count", unique as f64, 1, 0);
    assert_eq!(forbidden, 0, "{line}\n{}", stderr(&o));
    assert!(unique > 0, "{line}");
}

/// A binary `contextful-cli` declaring `deps`, each optional, and the three profile bundles
/// with the entries `edge`, `full` and `control`.
fn profiles(r: &Repo, deps: &str, edge: &str, full: &str, control: &str) {
    package(r, "contextful-cli", "");
    r.write(
        "crates/contextful-cli/Cargo.toml",
        &format!(
            "{}\n[features]\ncontextful-edge = [{edge}]\ncontextful-full = [{full}]\ncontextful-control = [{control}]\n",
            manifest("contextful-cli", deps)
        ),
    );
}

/// The component host, the embedded SQL engine and a CRDT library as stubs, each optional on the binary.
fn profile_stubs(r: &Repo) -> &'static str {
    stub(r, "wasmtime", "", "");
    stub(r, "libduckdb-sys", "", "");
    stub(r, "duckdb", "libduckdb-sys = { path = \"../libduckdb-sys\" }\n", "");
    stub(r, "automerge", "", "");
    "wasmtime = { path = \"../../stubs/wasmtime\", optional = true }\n\
     duckdb = { path = \"../../stubs/duckdb\", optional = true }\n\
     automerge = { path = \"../../stubs/automerge\", optional = true }\n"
}

/// A profile resolving a package outside its role raises `ProfileRoleLeak`, naming the profile and the path: a component host, the run path or the evaluation runner in `contextful-edge` or `contextful-control`; the SQL engine in `contextful-control`; build tooling anywhere.
// spec: topology.package.profile-leak@f1ad3643
#[test]
fn a_profile_linking_a_dependency_outside_its_role_is_refused() {
    let r = Repo::init();
    let deps = profile_stubs(&r);
    profiles(&r, deps, "\"dep:duckdb\"", "\"dep:duckdb\", \"dep:wasmtime\"", "\"dep:automerge\"");
    let out = passes(&r);
    assert!(out.contains("profiles: `contextful-control`, `contextful-edge`, `contextful-full` hold to their roles"), "{out}");

    profiles(&r, deps, "\"dep:duckdb\", \"dep:wasmtime\"", "\"dep:duckdb\", \"dep:wasmtime\"", "");
    let err = refused(&topology(&r.root), "ProfileRoleLeak");
    assert!(err.contains("`contextful-edge` links `wasmtime`, a component host, through contextful-cli -> wasmtime"), "{err}");
    assert!(!err.contains("`contextful-full` links"), "{err}");

    profiles(&r, deps, "\"dep:duckdb\"", "\"dep:duckdb\", \"dep:wasmtime\"", "\"dep:duckdb\"");
    let err = refused(&topology(&r.root), "ProfileRoleLeak");
    assert!(err.contains("`contextful-control` links `duckdb`, the embedded SQL engine, through contextful-cli -> duckdb"), "{err}");
    assert!(err.contains("`contextful-control` links `libduckdb-sys`, the embedded SQL engine, through contextful-cli -> duckdb -> libduckdb-sys"), "{err}");

    package(&r, "contextful-engine", "");
    package(&r, "contextful-eval", "");
    let deps = format!(
        "{deps}contextful-engine = {{ path = \"../contextful-engine\", optional = true }}\n\
         contextful-eval = {{ path = \"../contextful-eval\", optional = true }}\n"
    );
    profiles(&r, &deps, "\"dep:duckdb\"", "\"dep:duckdb\", \"dep:wasmtime\", \"dep:contextful-engine\", \"dep:contextful-eval\"", "");
    passes(&r);

    profiles(&r, &deps, "\"dep:duckdb\", \"dep:contextful-engine\"", "\"dep:duckdb\", \"dep:wasmtime\"", "\"dep:contextful-eval\"");
    let err = refused(&topology(&r.root), "ProfileRoleLeak");
    assert!(err.contains("`contextful-edge` links `contextful-engine`, the run path, through contextful-cli -> contextful-engine"), "{err}");
    assert!(err.contains("`contextful-control` links `contextful-eval`, the evaluation runner, through contextful-cli -> contextful-eval"), "{err}");
}

/// No run-time detection widens a running binary into another profile's feature set. Build, release and automation tooling links into no profile.
// spec: topology.package.fixed-at-build@e97f9bd0
#[test]
fn a_profile_linking_build_tooling_is_refused() {
    let r = Repo::init();
    let deps = profile_stubs(&r);
    package(&r, "contextful-spec", "");
    let deps = format!("{deps}contextful-spec = {{ path = \"../contextful-spec\", optional = true }}\n");
    profiles(&r, &deps, "\"dep:duckdb\"", "\"dep:duckdb\", \"dep:wasmtime\"", "");
    passes(&r);

    profiles(&r, &deps, "\"dep:duckdb\"", "\"dep:duckdb\", \"dep:wasmtime\", \"dep:contextful-spec\"", "");
    let err = refused(&topology(&r.root), "ProfileRoleLeak");
    assert!(err.contains("`contextful-full` links `contextful-spec`, build tooling, through contextful-cli -> contextful-spec"), "{err}");
}

/// The CRDT library in the resolved dependency graph of the edge or full profile raises `ProfileDependencyLeak`, naming the profile and the path that pulled it. A daemon or replica reads materialized text.
// spec: topology.package.crdt-leak@d30f83fa
#[test]
fn a_crdt_library_outside_the_control_profile_is_refused() {
    let r = Repo::init();
    let deps = profile_stubs(&r);
    profiles(&r, deps, "\"dep:duckdb\"", "\"dep:duckdb\", \"dep:wasmtime\"", "\"dep:automerge\"");
    passes(&r);

    profiles(&r, deps, "\"dep:duckdb\"", "\"dep:duckdb\", \"dep:wasmtime\", \"dep:automerge\"", "\"dep:automerge\"");
    let err = refused(&topology(&r.root), "ProfileDependencyLeak");
    assert!(err.contains("`contextful-full` links CRDT library `automerge` through contextful-cli -> automerge"), "{err}");
    assert!(!err.contains("`contextful-control` links"), "{err}");
}

/// The normal-dependency package names of this repository's binary built with `profile` alone.
fn profile_graph(profile: &str) -> Vec<String> {
    let o = Command::new("cargo")
        .args(["tree", "-q", "--locked", "-p", "contextful-cli", "--no-default-features", "--features", profile])
        .args(["-e", "normal", "--prefix", "none", "--format", "{p}"])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let mut names: Vec<String> = stdout(&o).lines().filter_map(|l| l.split_whitespace().next()).map(str::to_string).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Three profiles compile from the workspace, each a Cargo feature bundle selected at build time: `contextful-edge`, `contextful-full`, `contextful-control`. Each links the dependencies its role names and nothing else.
// spec: topology.package.profile@f8fabc6b
#[test]
fn this_repository_binary_declares_the_three_profiles_each_linking_its_role() {
    let o = topology(repo_root());
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("profiles: `contextful-control`, `contextful-edge`, `contextful-full` hold to their roles"), "{}", stdout(&o));

    let full = profile_graph("contextful-full");
    for daemon in ["contextful-engine", "wasmtime", "duckdb", "contextful-agent", "contextful-eval", "rusty-s3", "pdf-extract"] {
        assert!(full.iter().any(|n| n == daemon), "`contextful-full` links no `{daemon}`");
    }
    let control = profile_graph("contextful-control");
    assert!(control.iter().any(|n| n == "contextful-policy"), "{control:?}");
    for absent in ["contextful-engine", "contextful-context", "contextful-sync", "duckdb", "wasmtime", "ureq", "libsqlite3-sys"] {
        assert!(!control.iter().any(|n| n == absent), "`contextful-control` links `{absent}`");
    }
}

/// `contextful-full` is the daemon: the durable-execution core, the in-process scheduler, the component host, the SQL query face, transforms, the full-text and vector sidecars and the tool server.
///
/// The engine crate carries the journal and the scheduler, the component host is
/// `contextful-wasm` over `wasmtime`, the query face is the store adapter over the SQL
/// engine with the sidecars beside it, the transform chain runs in the binary's pipeline
/// module, compiled under the data plane the profile selects, and the tool server is
/// `contextful-agent`.
// spec: topology.package.full-profile@1366e642
#[test]
fn this_repository_full_profile_links_every_daemon_role() {
    let full = profile_graph("contextful-full");
    let roles = [
        ("durable-execution core and scheduler", "contextful-engine"),
        ("run store", "contextful-sqlite"),
        ("component host", "contextful-wasm"),
        ("component runtime", "wasmtime"),
        ("query face and sidecars", "contextful-context"),
        ("SQL engine", "duckdb"),
        ("memory synthesis", "contextful-memory"),
        ("tool server", "contextful-agent"),
    ];
    for (role, package) in roles {
        assert!(full.iter().any(|n| n == package), "`contextful-full` links no `{package}` for its {role}");
    }
    let src = repo_root().join("crates");
    for (module, path) in [("scheduler", "contextful-engine/src/scheduler.rs"), ("journal", "contextful-engine/src/journal.rs"), ("full-text sidecar", "contextful-context/src/fulltext/mod.rs"), ("vector sidecar", "contextful-context/src/vector/mod.rs")] {
        assert!(src.join(path).is_file(), "no {module} at crates/{path}");
    }
    let manifest: toml::Value = toml::from_str(&std::fs::read_to_string(src.join("contextful-cli/Cargo.toml")).unwrap()).unwrap();
    let full_features: Vec<&str> = manifest["features"]["contextful-full"].as_array().unwrap().iter().filter_map(|f| f.as_str()).collect();
    assert!(full_features.contains(&"data-plane"), "{full_features:?}");
    let lib = std::fs::read_to_string(src.join("contextful-cli/src/lib.rs")).unwrap();
    assert!(lib.contains("#[cfg(feature = \"data-plane\")]\nmod pipeline;"), "the transform chain's pipeline module compiles under another feature");
    let pipeline = std::fs::read_to_string(src.join("contextful-cli/src/pipeline.rs")).unwrap();
    assert!(pipeline.contains("use contextful_core::pipeline::transform::Chain;"), "the pipeline module applies no transform chain");
}

/// `contextful-edge` is the read replica: it syncs parts and manifests from a bucket and serves a read-only SQL replica. It links no scheduler, run path, script runtime or component host.
// spec: topology.package.edge-profile@06bc004e
#[test]
fn this_repository_edge_profile_serves_reads_and_links_no_component_host() {
    let edge = profile_graph("contextful-edge");
    for replica in ["contextful-sync", "rusty-s3", "duckdb", "contextful-context", "contextful-agent"] {
        assert!(edge.iter().any(|n| n == replica), "`contextful-edge` links no `{replica}`");
    }
    let run_path = ["contextful-engine", "contextful-connectors", "contextful-sqlite", "contextful-memory", "contextful-eval", "libsqlite3-sys"];
    for absent in run_path.into_iter().chain(["contextful-wasm", "wasmtime"]) {
        assert!(!edge.iter().any(|n| n == absent), "`contextful-edge` links `{absent}`");
    }
}
