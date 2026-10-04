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
    use contextful_core::connector::attach::{scrub, Allowlist};
    use contextful_core::connector::package::{content_hash, Digest, Form, OciReference};
    use contextful_core::connector::reference::Template;
    use contextful_core::connector::ConnectorError;
    use contextful_core::run::plan::ConnectorSpec;
    use contextful_core::run::ports::Source;
    use contextful_core::run::Failure;
    use contextful_outbound::client::{Client, HeaderValue};
    use contextful_outbound::egress::Transport;
    use contextful_outbound::Resolver;
    use contextful_wasm::{ComponentHost, Connector, Grant, GuestSource, Hydrate, Limits, Session, Target, WORLD};
    use std::io::Write;
    use std::path::Path;
    use std::sync::Arc;
    use url::Url;

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
            Ok(if self.template.has_reference() { HeaderValue::Sensitive(v) } else { HeaderValue::Plain(v.reveal().to_string()) })
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

    fn artifact_error(error: ConnectorError) -> anyhow::Error {
        anyhow!(error)
    }

    fn fetch(client: &Client, url: &Url, headers: &[(String, HeaderValue)]) -> Result<Vec<u8>> {
        let response = client.send("GET", url, headers, None).map_err(|e| artifact_error(ConnectorError::ConnectorArtifactFetchFailed(format!("`{}`: {e}", scrub(url)))))?;
        if !(200..300).contains(&response.status) {
            return Err(artifact_error(ConnectorError::ConnectorArtifactFetchFailed(format!("`{}` answered {}", scrub(url), response.status))));
        }
        Ok(response.body)
    }

    fn client(url: &Url, transport: Option<Arc<dyn Transport>>) -> Result<Client> {
        let host = url.host_str().ok_or_else(|| artifact_error(ConnectorError::ConnectorArtifactFetchFailed("artifact reference has no host".into())))?;
        let client = Client::new(Allowlist::parse(&[host]).map_err(artifact_error)?, url.clone());
        Ok(match transport {
            Some(transport) => client.with_transport(transport),
            None => client,
        })
    }

    fn component_layer(manifest: &[u8], reference: &str) -> Result<Digest> {
        let manifest: serde_json::Value = serde_json::from_slice(manifest)
            .map_err(|e| artifact_error(ConnectorError::ConnectorOciArtifactUnsupported(e.to_string())))?;
        let layers = manifest.get("layers").and_then(serde_json::Value::as_array);
        if manifest.get("schemaVersion").and_then(serde_json::Value::as_u64) != Some(2) || layers.is_none_or(|a| a.len() != 1) {
            return Err(artifact_error(ConnectorError::ConnectorOciArtifactUnsupported(format!("`{reference}` requires one schema-2 component layer"))));
        }
        let layer = &layers.unwrap()[0];
        if layer.get("mediaType").and_then(serde_json::Value::as_str) != Some("application/vnd.wasm.content.layer.v1+wasm") {
            return Err(artifact_error(ConnectorError::ConnectorOciArtifactUnsupported(format!("`{reference}` has no component layer"))));
        }
        layer.get("digest").and_then(serde_json::Value::as_str).and_then(|s| s.strip_prefix("sha256:")).and_then(Digest::parse)
            .ok_or_else(|| artifact_error(ConnectorError::ConnectorOciArtifactUnsupported(format!("`{reference}` has no SHA-256 layer descriptor"))))
    }

    fn registry_headers(base: &Path, project: &str, authority: &str, resolver: &Resolver) -> Result<Vec<(String, HeaderValue)>> {
        let path = base.join(".contextful/context").join(project).join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading `{}`", path.display())),
        };
        let config: toml::Value = toml::from_str(&text).with_context(|| format!("reading `{}`", path.display()))?;
        let Some(raw) = config.get("connector").and_then(|v| v.get("registry")).and_then(|v| v.get(authority)).and_then(|v| v.get("authorization")) else {
            return Ok(Vec::new());
        };
        let value = raw.as_str().ok_or_else(|| anyhow!("registry authorization is a string"))?;
        if value.strip_prefix("Bearer ${secret://").and_then(|s| s.strip_suffix('}')).is_none_or(|name| name.is_empty() || name.contains('}')) {
            bail!("registry authorization uses `Bearer ${{secret://<name>}}`");
        }
        let template = Template::parse(value)?;
        if !template.has_reference() {
            bail!("registry authorization uses a secret reference");
        }
        resolver.preflight([&template])?;
        Ok(vec![("Authorization".to_string(), HeaderValue::Sensitive(resolver.render(&template)?))])
    }

    fn remote(name: &str, decl: &ComponentSource, base: &Path, project: &str, resolver: &Resolver, transport: Option<Arc<dyn Transport>>) -> Result<Vec<u8>> {
        let https = match &decl.artifact.form {
            Form::Https(raw) => {
                let url = Url::parse(raw).with_context(|| format!("artifact `{name}` is not a URL"))?;
                if !url.username().is_empty() || url.password().is_some() {
                    bail!("artifact URL carries userinfo");
                }
                Some(url)
            }
            _ => None,
        };
        let pin = decl.artifact.pin.as_ref().expect("remote references carry a pin at parse");
        let cache = base.join(".contextful/artifacts/sha256");
        let path = cache.join(pin.as_str());
        match std::fs::read(&path) {
            Ok(bytes) => {
                if Digest::of(&bytes) != *pin {
                    return Err(artifact_error(ConnectorError::ConnectorArtifactCacheCorrupt(format!("cached artifact `{pin}` differs from its digest key"))));
                }
                return Ok(bytes);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading cached artifact `{pin}`")),
        }
        let bytes = match &decl.artifact.form {
            Form::Https(_) => {
                let url = https.expect("HTTPS form parsed before cache lookup");
                fetch(&client(&url, transport.clone())?, &url, &[])?
            }
            Form::Oci(raw) => {
                let reference = OciReference::parse(raw).map_err(artifact_error)?;
                let root = Url::parse(&format!("https://{}/", reference.authority))?;
                let client = client(&root, transport.clone())?;
                let headers = registry_headers(base, project, &reference.authority, resolver)?;
                let manifest_url = root.join(&format!("v2/{}/manifests/{}", reference.repository, reference.selector))?;
                let mut manifest_headers = headers.clone();
                manifest_headers.push(("Accept".to_string(), HeaderValue::Plain("application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json".into())));
                let manifest = fetch(&client, &manifest_url, &manifest_headers)?;
                let descriptor = component_layer(&manifest, raw)?;
                let layer_url = root.join(&format!("v2/{}/blobs/sha256:{descriptor}", reference.repository))?;
                let bytes = fetch(&client, &layer_url, &headers)?;
                if Digest::of(&bytes) != descriptor {
                    return Err(artifact_error(ConnectorError::ConnectorOciLayerMismatch(format!("`{raw}` layer differs from descriptor {descriptor}"))));
                }
                bytes
            }
            _ => unreachable!("remote handles remote forms"),
        };
        decl.artifact.admit(&bytes, decl.requirement(false)).map_err(artifact_error)?;
        std::fs::create_dir_all(&cache).with_context(|| format!("creating artifact cache `{}`", cache.display()))?;
        let mut temporary = tempfile::NamedTempFile::new_in(&cache)?;
        temporary.write_all(&bytes)?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {}
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = std::fs::read(&path)?;
                if Digest::of(&existing) != *pin {
                    return Err(artifact_error(ConnectorError::ConnectorArtifactCacheCorrupt(format!("cached artifact `{pin}` differs from its digest key"))));
                }
            }
            Err(e) => return Err(e.error.into()),
        }
        Ok(bytes)
    }

    /// The artifact's bytes. A local path resolves against the project directory.
    fn resolve(name: &str, decl: &ComponentSource, base: &Path, project: &str, resolver: Option<&Resolver>) -> Result<Vec<u8>> {
        match &decl.artifact.form {
            Form::Local(path) => std::fs::read(base.join(path)).with_context(|| format!("reading component artifact `{path}`")),
            Form::Https(_) | Form::Oci(_) => remote(name, decl, base, project, resolver.expect("remote loads have a resolver"), None),
            Form::InTree(_) => bail!("connector `{name}` is an in-tree name, not an artifact"),
        }
    }

    /// Resolve, admit and compile `decl` for `target`, under the store-wide pin switch
    /// `store_pin`. Bytes off their pin never reach the compiler
    /// (`connector.package.digest-mismatch`).
    pub fn load(name: &str, decl: &ComponentSource, base: &Path, project: &str, resolver: Option<&Resolver>, target: ComponentTarget, store_pin: bool) -> Result<Loaded> {
        let limits = limits(decl)?;
        let target = match target {
            ComponentTarget::Native => Target::Native,
            ComponentTarget::Pulley => Target::Pulley,
        };
        let wasm = resolve(name, decl, base, project, resolver)?;
        let host = ComponentHost::with_target(target).map_err(failure)?;
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

    #[cfg(test)]
    mod tests {
        use super::{component_layer, fetch, registry_headers, remote};
        use contextful_core::connector::attach::Allowlist;
        use contextful_core::connector::component::ComponentSource;
        use contextful_core::connector::package::Digest;
        use contextful_core::ports::FixedClock;
        use contextful_core::time::Instant;
        use contextful_outbound::client::Client;
        use contextful_outbound::egress::{Inbound, Outbound, Transport, TransportFault};
        use contextful_outbound::assemble;
        use std::collections::BTreeMap;
        use std::net::SocketAddr;
        use std::sync::{Arc, Mutex};
        use url::Url;

        struct ArtifactTransport;

        struct OciTransport {
            manifest: Vec<u8>,
            layer: Vec<u8>,
            seen: Mutex<Vec<String>>,
        }

        impl Transport for OciTransport {
            fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
                assert_eq!((host, port), ("registry.example.test", 443));
                Ok(vec!["8.8.8.8:443".parse().unwrap()])
            }

            fn send(&self, request: &Outbound<'_>) -> Result<Inbound, TransportFault> {
                assert!(request.headers.iter().any(|(name, value)| name == "Authorization" && value.text() == "Bearer registry-secret"));
                let path = request.url.path().to_string();
                self.seen.lock().unwrap().push(path.clone());
                let body = if path.contains("/manifests/") { self.manifest.clone() } else { self.layer.clone() };
                Ok(Inbound { status: 200, headers: Vec::new(), body })
            }
        }

        impl Transport for ArtifactTransport {
            fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
                assert_eq!((host, port), ("artifacts.example.test", 443));
                Ok(vec!["8.8.8.8:443".parse().unwrap()])
            }

            fn send(&self, request: &Outbound<'_>) -> Result<Inbound, TransportFault> {
                assert_eq!(request.method, "GET");
                assert_eq!(request.url.as_str(), "https://artifacts.example.test/probe.wasm");
                Ok(Inbound { status: 200, headers: Vec::new(), body: b"component".to_vec() })
            }
        }

        #[test]
        fn artifact_fetch_uses_the_mediated_client() {
            let url = Url::parse("https://artifacts.example.test/probe.wasm").unwrap();
            let client = Client::new(Allowlist::parse(&["artifacts.example.test"]).unwrap(), url.clone()).with_transport(Arc::new(ArtifactTransport));
            assert_eq!(fetch(&client, &url, &[]).unwrap(), b"component");
        }

        #[test]
        fn registry_bearer_is_bound_to_the_exact_authority() {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join(".contextful/context/research/config.toml");
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(&config, "[connector.registry.\"registry.example.test\"]\nauthorization = \"Bearer ${secret://registry-token}\"\n").unwrap();
            let vars = BTreeMap::from([
                ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES".to_string(), "1".to_string()),
                ("REGISTRY_TOKEN".to_string(), "registry-secret".to_string()),
            ]);
            let resolver = assemble(&vars, Arc::new(FixedClock(Instant::parse("2030-01-01T00:00:00Z").unwrap()))).unwrap();
            assert!(registry_headers(dir.path(), "research", "other.example.test", &resolver).unwrap().is_empty());
            let headers = registry_headers(dir.path(), "research", "registry.example.test", &resolver).unwrap();
            assert_eq!(headers.len(), 1);
            assert_eq!(headers[0].1.text(), "Bearer registry-secret");
        }

        // spec: connector.package.remote-transport@5044139f
        // spec: connector.package.oci-component-layer@8b743c72
        // spec: connector.package.oci-layer-integrity@185ef002
        // spec: connector.package.oci-registry-bearer@0cf0801f
        #[test]
        fn oci_fetch_checks_the_layer_and_caches_only_admitted_bytes() {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join(".contextful/context/research/config.toml");
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(&config, "[connector.registry.\"registry.example.test\"]\nauthorization = \"Bearer ${secret://registry-token}\"\n").unwrap();
            let vars = BTreeMap::from([
                ("CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES".to_string(), "1".to_string()),
                ("REGISTRY_TOKEN".to_string(), "registry-secret".to_string()),
            ]);
            let resolver = assemble(&vars, Arc::new(FixedClock(Instant::parse("2030-01-01T00:00:00Z").unwrap()))).unwrap();
            let layer = b"component".to_vec();
            let pin = Digest::of(&layer);
            let name = "oci://registry.example.test/repo:stable";
            let decl = ComponentSource::parse(name, &serde_json::json!({"sha256": pin.as_str()})).unwrap().unwrap();
            let manifest = format!(r#"{{"schemaVersion":2,"layers":[{{"mediaType":"application/vnd.wasm.content.layer.v1+wasm","digest":"sha256:{pin}"}}]}}"#).into_bytes();
            let transport = Arc::new(OciTransport { manifest, layer: layer.clone(), seen: Mutex::new(Vec::new()) });
            let fetched = remote(name, &decl, dir.path(), "research", &resolver, Some(transport.clone())).unwrap();
            assert_eq!(fetched, layer);
            assert_eq!(transport.seen.lock().unwrap().as_slice(), ["/v2/repo/manifests/stable", &format!("/v2/repo/blobs/sha256:{pin}")]);
            let cached = remote(name, &decl, dir.path(), "research", &resolver, Some(transport.clone())).unwrap();
            assert_eq!(cached, layer);
            assert_eq!(transport.seen.lock().unwrap().len(), 2);

            let other = tempfile::tempdir().unwrap();
            let other_config = other.path().join(".contextful/context/research/config.toml");
            std::fs::create_dir_all(other_config.parent().unwrap()).unwrap();
            std::fs::copy(&config, &other_config).unwrap();
            let wrong = Arc::new(OciTransport {
                manifest: format!(r#"{{"schemaVersion":2,"layers":[{{"mediaType":"application/vnd.wasm.content.layer.v1+wasm","digest":"sha256:{}"}}]}}"#, Digest::of(b"other")).into_bytes(),
                layer,
                seen: Mutex::new(Vec::new()),
            });
            let err = remote(name, &decl, other.path(), "research", &resolver, Some(wrong)).unwrap_err();
            assert!(err.to_string().contains("ConnectorOciLayerMismatch"), "{err}");
            assert!(!other.path().join(".contextful/artifacts/sha256").exists());
        }

        #[test]
        fn oci_manifest_requires_one_component_layer_with_a_sha256_descriptor() {
            let digest = "ab".repeat(32);
            let valid = format!(r#"{{"schemaVersion":2,"layers":[{{"mediaType":"application/vnd.wasm.content.layer.v1+wasm","digest":"sha256:{digest}"}}]}}"#);
            assert_eq!(component_layer(valid.as_bytes(), "oci://example.test/repo").unwrap().as_str(), digest);
            let two = valid.replace("]}", format!(",{{\"mediaType\":\"application/vnd.wasm.content.layer.v1+wasm\",\"digest\":\"sha256:{digest}\"}}]}}").as_str());
            assert!(component_layer(two.as_bytes(), "oci://example.test/repo").unwrap_err().to_string().contains("ConnectorOciArtifactUnsupported"));
            let wrong_type = valid.replace("application/vnd.wasm.content.layer.v1+wasm", "application/octet-stream");
            assert!(component_layer(wrong_type.as_bytes(), "oci://example.test/repo").unwrap_err().to_string().contains("ConnectorOciArtifactUnsupported"));
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

    pub fn load(name: &str, _decl: &ComponentSource, _base: &Path, _project: &str, _resolver: Option<&Resolver>, _target: ComponentTarget, _store_pin: bool) -> Result<Loaded> {
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
