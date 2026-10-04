//! Component sources on the command line: a pipeline source naming an artifact runs as a
//! guest under `contextful-wasm`'s component host, reaching the network only through the
//! mediated client (`connector.package.component-load`). A build without the
//! `component-host` feature links no host and refuses every component source by name
//! (`topology.package.host-missing`).

use anyhow::Result;
use contextful_core::connector::component::ComponentSource;

/// The instruction set components compile for (`connector.package.component-target`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ComponentTarget {
    /// Machine code for the host, the default.
    #[default]
    Native,
    /// Pulley bytecode, for a process that may not map executable memory; needs the `pulley` feature.
    Pulley,
}

#[cfg(feature = "component-host")]
pub use hosted::*;

#[cfg(not(feature = "component-host"))]
pub use absent::*;

#[cfg(feature = "component-host")]
mod hosted {
    use super::ComponentTarget;
    use anyhow::{anyhow, bail, Context, Result};
    use contextful_core::connector::component::ComponentSource;
    use contextful_core::connector::package::{content_hash, Form};
    use contextful_core::run::plan::ConnectorSpec;
    use contextful_core::run::ports::Source;
    use contextful_core::run::Failure;
    use contextful_outbound::client::HeaderValue;
    use contextful_outbound::Resolver;
    use contextful_core::connector::reference::Template;
    use contextful_wasm::{ComponentHost, Connector, Grant, GuestSource, Hydrate, Limits, Session, Target, WORLD};
    use std::path::Path;
    use std::sync::Arc;

    fn failure(f: Failure) -> anyhow::Error {
        anyhow!("{f}")
    }

    /// The component worlds this build's host answers.
    pub fn worlds() -> Vec<String> {
        vec![WORLD.to_string()]
    }

    /// Nothing to refuse: this build links a component host.
    pub fn absent(_name: &str) -> Result<()> {
        Ok(())
    }

    /// The bounds a session of `decl` runs under, its memory override held to the ceiling.
    pub fn limits(decl: &ComponentSource) -> Result<Limits> {
        match decl.memory_bytes {
            Some(bytes) => Limits::default().with_memory(bytes).map_err(failure),
            None => Ok(Limits::default()),
        }
    }

    /// One `attach` template, rendered through the source's resolver on every request.
    struct Rendered {
        resolver: Arc<Resolver>,
        template: Template,
    }

    impl Hydrate for Rendered {
        fn hydrate(&self) -> Result<HeaderValue, Failure> {
            let v = self.resolver.render(&self.template)?;
            Ok(if self.template.has_reference() { HeaderValue::Sensitive(v.into()) } else { HeaderValue::Plain(v.reveal().to_string()) })
        }
    }

    /// A component resolved, admitted against its pin and compiled, ready to open sessions.
    pub struct Loaded {
        name: String,
        host: ComponentHost,
        connector: Connector,
        limits: Limits,
        content_hash: String,
    }

    /// The artifact's bytes. A local path resolves against the project directory; a remote
    /// artifact is not fetched.
    fn resolve(name: &str, decl: &ComponentSource, base: &Path) -> Result<Vec<u8>> {
        match &decl.artifact.form {
            Form::Local(path) => std::fs::read(base.join(path)).with_context(|| format!("reading component artifact `{path}`")),
            Form::Https(_) | Form::Oci(_) => bail!(
                "connector `{name}` is a remote artifact; this build resolves local artifacts, so place its bytes under the project and pin them with `sha256`"
            ),
            Form::InTree(_) => bail!("connector `{name}` is an in-tree name, not an artifact"),
        }
    }

    /// Resolve, admit and compile `decl` for `target`, under the store-wide pin switch
    /// `store_pin`. Bytes off their pin never reach the compiler
    /// (`connector.package.digest-mismatch`).
    pub fn load(name: &str, decl: &ComponentSource, base: &Path, target: ComponentTarget, store_pin: bool) -> Result<Loaded> {
        let limits = limits(decl)?;
        let target = match target {
            ComponentTarget::Native => Target::Native,
            ComponentTarget::Pulley => Target::Pulley,
        };
        let wasm = resolve(name, decl, base)?;
        let host = ComponentHost::with_cache_dir(target, base.join(".contextful/cache/components")).map_err(failure)?;
        let (connector, digest) = host.load_artifact(&decl.artifact, &wasm, decl.requirement(store_pin)).map_err(failure)?;
        let content_hash = content_hash(&digest, decl.guest.as_ref());
        Ok(Loaded { name: name.to_string(), host, connector, limits, content_hash })
    }

    impl Loaded {
        /// The connector's content hash (`connector.import.config-hashing`).
        pub fn content_hash(&self) -> String {
            self.content_hash.clone()
        }

        /// The connector a run of this component pins.
        pub fn connector_spec(&self) -> ConnectorSpec {
            ConnectorSpec {
                id: self.name.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                world: WORLD.to_string(),
                command: vec![format!("component:{}", self.name)],
            }
        }

        fn open(&self, decl: &ComponentSource, hydrate: Vec<(String, Arc<dyn Hydrate>)>, run_id: Option<String>) -> Result<Session> {
            let grant = Grant { allow: decl.allow.clone(), attach: Vec::new(), hydrate, gate: None, hook: None, class: None, run_id, transport: None };
            self.host.open(&self.connector, grant, &self.limits, decl.guest.as_ref()).map_err(failure)
        }

        /// A session reading `table` for `run_id`, each `attach` header hydrated from
        /// `resolver` per request, while the request is built.
        pub fn source(&self, decl: &ComponentSource, table: &str, resolver: &Arc<Resolver>, run_id: &str) -> Result<Box<dyn Source>> {
            let hydrate = decl
                .attach
                .iter()
                .map(|(header, t)| (header.clone(), Arc::new(Rendered { resolver: resolver.clone(), template: t.clone() }) as Arc<dyn Hydrate>))
                .collect();
            Ok(Box::new(GuestSource::new(self.open(decl, hydrate, Some(run_id.to_string()))?, table)))
        }

        /// The table names the guest's discovery answers, under a session attaching no credential.
        pub fn discover(&self, decl: &ComponentSource) -> Result<Vec<String>> {
            let mut session = self.open(decl, Vec::new(), None)?;
            Ok(session.discover().map_err(failure)?.into_iter().map(|s| s.name).collect())
        }
    }
}

#[cfg(not(feature = "component-host"))]
mod absent {
    use super::ComponentTarget;
    use anyhow::Result;
    use contextful_core::connector::component::ComponentSource;
    use contextful_core::run::plan::ConnectorSpec;
    use contextful_core::run::ports::Source;
    use contextful_core::topology::TopologyError;
    use contextful_outbound::Resolver;
    use std::path::Path;

    /// No component world: this build links no host.
    pub fn worlds() -> Vec<String> {
        Vec::new()
    }

    /// Refuse the component source `name` (`topology.package.host-missing`).
    pub fn absent(name: &str) -> Result<()> {
        Err(TopologyError::ComponentHostMissing(format!(
            "connector `{name}` is a component, and this build's profile links no component host: it was built without the `component-host` feature, and no native source stands in for it"
        ))
        .into())
    }

    /// A component this build cannot hold.
    pub enum Loaded {}

    pub fn load(name: &str, _decl: &ComponentSource, _base: &Path, _target: ComponentTarget, _store_pin: bool) -> Result<Loaded> {
        absent(name).and_then(|()| unreachable!("`absent` refuses on this build"))
    }

    impl Loaded {
        pub fn content_hash(&self) -> String {
            match *self {}
        }

        pub fn connector_spec(&self) -> ConnectorSpec {
            match *self {}
        }

        pub fn source(&self, _decl: &ComponentSource, _table: &str, _resolver: &std::sync::Arc<Resolver>, _run_id: &str) -> Result<Box<dyn Source>> {
            match *self {}
        }

        pub fn discover(&self, _decl: &ComponentSource) -> Result<Vec<String>> {
            match *self {}
        }
    }
}

/// Check `decl` as far as this build can before any I/O: the component host is linked, and
/// the memory override sits under its ceiling.
pub fn check(name: &str, decl: &ComponentSource) -> Result<()> {
    absent(name)?;
    #[cfg(feature = "component-host")]
    limits(decl)?;
    #[cfg(not(feature = "component-host"))]
    let _ = decl;
    Ok(())
}

/// Whether `decl`'s artifact sits in the project, so validation may load it without the network.
pub fn is_local(decl: &ComponentSource) -> bool {
    matches!(decl.artifact.form, contextful_core::connector::package::Form::Local(_))
}
