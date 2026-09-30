//! `contextful-ci deploy probe` — `topology.publish-hostname`'s deploy-time posture check.
//! Every published hostname carries a descriptor under `deploy/hostnames/`; the deploy's
//! probe table, `deploy/probe.toml`, names the same hostname-and-gate pairs; each hostname
//! answers one anonymous `GET /` inside its declared gate, or the deploy fails.

use crate::refuse;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

/// The descriptor contract version this tool decodes.
const DESCRIPTOR_VERSION: i64 = 1;
/// The keys descriptor contract version 1 models (`topology.publish-hostname.unknown-field`).
const DESCRIPTOR_KEYS: [&str; 5] = ["version", "hostname", "worker", "gate", "acknowledged"];
const PROBE_KEYS: [&str; 2] = ["hostname", "gate"];
const DESCRIPTOR_DIR: &str = "hostnames";
const PROBE_TABLE: &str = "probe.toml";
/// Wall clock one probe request runs for before its hostname counts unreachable.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The posture a hostname declares, judged against the status of an anonymous `GET /`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Gate {
    /// An identity proxy fronts the hostname: the anonymous request is redirected to sign in.
    Access,
    /// The hostname answers its own token check: any answer short of a server error.
    AdminToken,
    /// The hostname serves anyone.
    Public,
}

impl Gate {
    fn parse(s: &str) -> Option<Gate> {
        match s {
            "access" => Some(Gate::Access),
            "adminToken" => Some(Gate::AdminToken),
            "public" => Some(Gate::Public),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Gate::Access => "access",
            Gate::AdminToken => "adminToken",
            Gate::Public => "public",
        }
    }

    fn admits(self, status: u16) -> bool {
        match self {
            Gate::Access => status == 302,
            Gate::AdminToken => status < 500,
            Gate::Public => status == 200,
        }
    }

    fn expected(self) -> &'static str {
        match self {
            Gate::Access => "302",
            Gate::AdminToken => "a status below 500",
            Gate::Public => "200",
        }
    }
}

struct Descriptor {
    hostname: String,
    gate: Gate,
}

/// Check the probe table against the descriptors under `dir`, then probe each hostname.
/// `resolve` maps a hostname to the base URL its request goes to in place of
/// `https://<hostname>`.
pub fn run(dir: &Path, resolve: &[String]) -> Result<()> {
    let resolve = resolve
        .iter()
        .map(|r| r.split_once('=').map(|(h, u)| (h.to_string(), u.trim_end_matches('/').to_string())))
        .collect::<Option<BTreeMap<_, _>>>()
        .context("--resolve takes <hostname>=<base url>")?;
    let descriptors = descriptors(&dir.join(DESCRIPTOR_DIR))?;
    let table = probe_table(&dir.join(PROBE_TABLE))?;
    drift(&descriptors, &table)?;

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(PROBE_TIMEOUT))
        .build()
        .into();
    let mut mismatches = Vec::new();
    for d in &descriptors {
        let base = resolve.get(&d.hostname).cloned().unwrap_or_else(|| format!("https://{}", d.hostname));
        let observed = match agent.get(format!("{base}/")).call() {
            Ok(response) => {
                let status = response.status().as_u16();
                if d.gate.admits(status) {
                    println!("{}\t{}\t{status}\tok", d.hostname, d.gate.name());
                    continue;
                }
                format!("answered {status}, where the gate expects {}", d.gate.expected())
            }
            Err(e) => format!("unreachable ({e})"),
        };
        println!("{}\t{}\t{observed}\tmismatch", d.hostname, d.gate.name());
        mismatches.push(format!("{} declares `{}` and {observed}", d.hostname, d.gate.name()));
    }
    if !mismatches.is_empty() {
        return Err(refuse("HostnamePostureMismatch", mismatches.join("; ")));
    }
    Ok(())
}

/// Decode every `*.toml` under `dir`, in file-name order; an absent directory holds none.
fn descriptors(dir: &Path) -> Result<Vec<Descriptor>> {
    let mut paths: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| dir.display().to_string()),
    };
    paths.sort();
    let mut out: Vec<Descriptor> = Vec::new();
    for path in paths {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(&path).with_context(|| shown.clone())?;
        let table: toml::Table = toml::from_str(&text).with_context(|| shown.clone())?;
        let version = table.get("version").and_then(|v| v.as_integer()).with_context(|| format!("{shown}: no integer `version`"))?;
        if version != DESCRIPTOR_VERSION {
            bail!("{shown}: descriptor contract version {version}; this tool decodes version {DESCRIPTOR_VERSION}");
        }
        if let Some(key) = table.keys().find(|k| !DESCRIPTOR_KEYS.contains(&k.as_str())) {
            return Err(refuse(
                "DescriptorUnknownField",
                format!("{shown}: key `{key}` is not modelled by descriptor contract version {version}"),
            ));
        }
        let text_of = |key: &str| table.get(key).and_then(|v| v.as_str()).with_context(|| format!("{shown}: no string `{key}`"));
        let hostname = text_of("hostname")?.to_string();
        text_of("worker")?;
        let gate = gate_of(text_of("gate")?, &shown)?;
        if table.get("acknowledged").and_then(|v| v.as_bool()) != Some(true) {
            bail!("{shown}: `acknowledged = true` records that the gate `{}` is deliberate", gate.name());
        }
        if out.iter().any(|d| d.hostname == hostname) {
            bail!("{shown}: hostname `{hostname}` has a second descriptor");
        }
        out.push(Descriptor { hostname, gate });
    }
    Ok(out)
}

/// The probe table's `[[probe]]` entries as hostname-and-gate pairs; an absent file holds none.
fn probe_table(path: &Path) -> Result<BTreeSet<(String, Gate)>> {
    let shown = path.display().to_string();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(e).context(shown),
    };
    let table: toml::Table = toml::from_str(&text).with_context(|| shown.clone())?;
    if let Some(key) = table.keys().find(|k| *k != "probe") {
        bail!("{shown}: key `{key}` is not a probe table key");
    }
    let entries = match table.get("probe") {
        None => return Ok(BTreeSet::new()),
        Some(v) => v.as_array().with_context(|| format!("{shown}: `probe` is an array of tables"))?,
    };
    let mut out = BTreeSet::new();
    for entry in entries {
        let entry = entry.as_table().with_context(|| format!("{shown}: `probe` is an array of tables"))?;
        if let Some(key) = entry.keys().find(|k| !PROBE_KEYS.contains(&k.as_str())) {
            bail!("{shown}: key `{key}` is not a probe entry key");
        }
        let text_of = |key: &str| entry.get(key).and_then(|v| v.as_str()).with_context(|| format!("{shown}: a probe entry has no string `{key}`"));
        out.insert((text_of("hostname")?.to_string(), gate_of(text_of("gate")?, &shown)?));
    }
    Ok(out)
}

fn gate_of(s: &str, shown: &str) -> Result<Gate> {
    Gate::parse(s).with_context(|| format!("{shown}: gate `{s}` is none of `access`, `adminToken`, `public`"))
}

/// `topology.publish-hostname.probe-table`: both sides name the same pairs before any probe.
fn drift(descriptors: &[Descriptor], table: &BTreeSet<(String, Gate)>) -> Result<()> {
    let declared: BTreeSet<(String, Gate)> = descriptors.iter().map(|d| (d.hostname.clone(), d.gate)).collect();
    let pair = |(h, g): &(String, Gate)| format!("{h} ({})", g.name());
    let mut findings: Vec<String> = table.difference(&declared).map(|p| format!("{} is missing from the descriptor set", pair(p))).collect();
    findings.extend(declared.difference(table).map(|p| format!("{} is missing from the probe table", pair(p))));
    if !findings.is_empty() {
        return Err(refuse("ProbeTableDrift", findings.join("; ")));
    }
    Ok(())
}
