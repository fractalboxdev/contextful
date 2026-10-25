//! `contextful-ci image` (`assurance.build.release-image`): one profile's release image
//! assembled from its musl release binaries, one platform manifest per binary, under the
//! runtime stage of the repository's `Dockerfile`.
//!
//! The command writes, into `--out`:
//!
//! - `<profile>-<version>-linux-<arch>.layer.tar.gz` per binary: the gzip layer holding the
//!   binary at the entrypoint, the CA bundle and the store volume owned by the runtime user;
//! - `<profile>-<version>-linux-<arch>.image.json` per binary: the layer's digest, size and
//!   diff id, the config and manifest blobs verbatim, and the manifest's index `entry`;
//! - `<profile>-<version>.oci.tar`: an OCI image layout whose `index.json` names one image
//!   index, tagged `<version>`, listing the `entry` of each binary, amd64 before arm64.
//!
//! A release cell passes its own binary, so the descriptor it writes equals the one a run
//! over every binary writes; a publisher composes the index from the cells' entries.

use anyhow::{bail, Context, Result};
use flate2::{Compression, GzBuilder};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::refuse;

const INDEX: &str = "application/vnd.oci.image.index.v1+json";
const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// Where the runtime image carries the CA roots bucket endpoints verify against.
const CA_PATH: &str = "etc/ssl/certs/ca-certificates.crt";

/// One release target's image platform, in index order.
struct Platform {
    target: &'static str,
    architecture: &'static str,
    variant: Option<&'static str>,
    /// The ELF `e_machine` its binary carries.
    machine: u16,
}

const PLATFORMS: [Platform; 2] = [
    Platform { target: "x86_64-unknown-linux-musl", architecture: "amd64", variant: None, machine: 0x3e },
    Platform { target: "aarch64-unknown-linux-musl", architecture: "arm64", variant: Some("v8"), machine: 0xb7 },
];

impl Platform {
    fn json(&self) -> Value {
        let mut p = json!({ "os": "linux", "architecture": self.architecture });
        if let Some(v) = self.variant {
            p["variant"] = json!(v);
        }
        p
    }
}

/// The runtime stage of the `Dockerfile`: what its last `FROM` sets.
struct Runtime {
    user: String,
    uid: u64,
    gid: u64,
    workdir: String,
    volumes: Vec<String>,
    entrypoint: Vec<String>,
    cmd: Vec<String>,
}

fn runtime(root: &Path) -> Result<Runtime> {
    let path = root.join("Dockerfile");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut lines: Vec<String> = Vec::new();
    let mut open = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        open.push_str(line.trim_end_matches('\\').trim_end());
        open.push(' ');
        if !line.ends_with('\\') {
            lines.push(open.trim().to_string());
            open.clear();
        }
    }
    let start = lines.iter().rposition(|l| l.starts_with("FROM ")).context("the Dockerfile has no stage")?;
    let stage = &lines[start + 1..];
    let arg = |instruction: &str| -> Result<String> {
        stage
            .iter()
            .find_map(|l| l.strip_prefix(&format!("{instruction} ")))
            .map(str::to_string)
            .with_context(|| format!("the Dockerfile's runtime stage has no {instruction}"))
    };
    let array = |instruction: &str| -> Result<Vec<String>> {
        serde_json::from_str(&arg(instruction)?).with_context(|| format!("the runtime stage's {instruction} is no JSON array of strings"))
    };
    let user = arg("USER")?;
    let (uid, gid) = user.split_once(':').unwrap_or((&user, &user));
    let entrypoint = array("ENTRYPOINT")?;
    if entrypoint.first().is_none_or(|e| !e.starts_with('/')) {
        bail!("the runtime stage's ENTRYPOINT names no absolute binary");
    }
    Ok(Runtime {
        uid: uid.parse().context("the runtime stage's USER is no numeric uid")?,
        gid: gid.parse().context("the runtime stage's USER is no numeric gid")?,
        user,
        workdir: arg("WORKDIR")?,
        volumes: array("VOLUME")?,
        entrypoint,
        cmd: array("CMD")?,
    })
}

/// Refuse a binary that is not a static ELF64 executable of `platform`'s machine.
fn check(platform: &Platform, binary: &[u8]) -> Result<()> {
    let mismatch = |why: String| Err(refuse("ImageBinaryMismatch", format!("`{}` {why}", platform.target)));
    if binary.len() < 64 || binary[..4] != *b"\x7fELF" || binary[4] != 2 || binary[5] != 1 {
        return mismatch("binary is no ELF64 little-endian file".into());
    }
    let u16_at = |at: usize| u16::from_le_bytes([binary[at], binary[at + 1]]);
    let machine = u16_at(18);
    if machine != platform.machine {
        return mismatch(format!("binary carries ELF machine {machine:#x}, not {:#x}", platform.machine));
    }
    let phoff = u64::from_le_bytes(binary[32..40].try_into().unwrap()) as usize;
    let (phentsize, phnum) = (u16_at(54) as usize, u16_at(56) as usize);
    for i in 0..phnum {
        let at = phoff.saturating_add(i.saturating_mul(phentsize));
        let Some(header) = binary.get(at..at.saturating_add(4)) else {
            return mismatch(format!("binary's program header {i} lies past its end"));
        };
        if u32::from_le_bytes(header.try_into().unwrap()) == 3 {
            return mismatch("binary names a dynamic interpreter (PT_INTERP); the runtime image holds no C library".into());
        }
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A tar archive whose entries carry no time, owner name or host metadata, in the order added.
struct Tar(tar::Builder<Vec<u8>>);

impl Tar {
    fn new() -> Self {
        let mut b = tar::Builder::new(Vec::new());
        b.mode(tar::HeaderMode::Deterministic);
        Self(b)
    }

    fn add(&mut self, path: &str, kind: tar::EntryType, mode: u32, owner: (u64, u64), data: &[u8]) -> Result<()> {
        let mut h = tar::Header::new_ustar();
        h.set_entry_type(kind);
        h.set_size(data.len() as u64);
        h.set_mode(mode);
        h.set_uid(owner.0);
        h.set_gid(owner.1);
        h.set_mtime(0);
        self.0.append_data(&mut h, path, data).with_context(|| format!("archiving {path}"))
    }

    fn dir(&mut self, path: &str, owner: (u64, u64)) -> Result<()> {
        self.add(&format!("{path}/"), tar::EntryType::Directory, 0o755, owner, &[])
    }

    fn file(&mut self, path: &str, mode: u32, data: &[u8]) -> Result<()> {
        self.add(path, tar::EntryType::Regular, mode, (0, 0), data)
    }

    fn finish(self) -> Result<Vec<u8>> {
        Ok(self.0.into_inner()?)
    }
}

/// Every proper ancestor of `path`, outermost first.
fn ancestors(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').collect();
    (1..parts.len()).map(|n| parts[..n].join("/")).collect()
}

/// The uncompressed layer: the binary at the entrypoint, the CA bundle, and each volume
/// owned by the runtime user, every path in sorted order.
fn layer(rt: &Runtime, binary: &[u8], ca: &[u8]) -> Result<Vec<u8>> {
    let bin = rt.entrypoint[0].trim_start_matches('/').to_string();
    let volumes: Vec<String> = rt.volumes.iter().map(|v| v.trim_matches('/').to_string()).collect();
    let mut entries: std::collections::BTreeMap<String, Option<(u32, &[u8])>> = Default::default();
    for (path, mode, data) in [(bin.as_str(), 0o755, binary), (CA_PATH, 0o644, ca)] {
        ancestors(path).into_iter().for_each(|d| {
            entries.insert(d, None);
        });
        entries.insert(path.to_string(), Some((mode, data)));
    }
    for v in &volumes {
        ancestors(v).into_iter().for_each(|d| {
            entries.insert(d, None);
        });
        entries.insert(v.clone(), None);
    }
    let mut tar = Tar::new();
    for (path, entry) in entries {
        match entry {
            Some((mode, data)) => tar.file(&path, mode, data)?,
            None if volumes.contains(&path) => tar.dir(&path, (rt.uid, rt.gid))?,
            None => tar.dir(&path, (0, 0))?,
        }
    }
    tar.finish()
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut gz = GzBuilder::new().mtime(0).write(Vec::new(), Compression::best());
    gz.write_all(bytes)?;
    Ok(gz.finish()?)
}

fn blob(media: &str, bytes: &[u8]) -> Value {
    json!({ "mediaType": media, "digest": format!("sha256:{}", sha256(bytes)), "size": bytes.len() })
}

/// One platform's image: its layer, config and manifest, and its descriptor.
struct Built {
    arch: &'static str,
    layer: Vec<u8>,
    config: Vec<u8>,
    manifest: Vec<u8>,
    descriptor: Value,
}

fn build(rt: &Runtime, profile: &str, version: &str, platform: &Platform, binary: &[u8], ca: &[u8]) -> Result<Built> {
    let tar = layer(rt, binary, ca)?;
    let diff_id = format!("sha256:{}", sha256(&tar));
    let layer = gzip(&tar)?;
    let volumes: serde_json::Map<String, Value> = rt.volumes.iter().map(|v| (v.clone(), json!({}))).collect();
    let mut config = json!({
        "os": "linux",
        "architecture": platform.architecture,
        "config": { "User": rt.user, "WorkingDir": rt.workdir, "Entrypoint": rt.entrypoint, "Cmd": rt.cmd, "Volumes": volumes },
        "rootfs": { "type": "layers", "diff_ids": [diff_id] },
        "history": [{ "created_by": format!("contextful-ci image {profile} {version}") }],
    });
    if let Some(v) = platform.variant {
        config["variant"] = json!(v);
    }
    let config = serde_json::to_vec(&config)?;
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST,
        "config": blob(CONFIG, &config),
        "layers": [blob(LAYER, &layer)],
    }))?;
    let mut entry = blob(MANIFEST, &manifest);
    entry["platform"] = platform.json();
    let mut layer_desc = blob(LAYER, &layer);
    layer_desc["diffId"] = json!(diff_id);
    layer_desc["path"] = json!(format!("{profile}-{version}-linux-{}.layer.tar.gz", platform.architecture));
    let mut config_desc = blob(CONFIG, &config);
    config_desc["content"] = json!(String::from_utf8(config.clone())?);
    let mut manifest_desc = blob(MANIFEST, &manifest);
    manifest_desc["content"] = json!(String::from_utf8(manifest.clone())?);
    let descriptor = json!({
        "profile": profile,
        "version": version,
        "target": platform.target,
        "platform": platform.json(),
        "layer": layer_desc,
        "config": config_desc,
        "manifest": manifest_desc,
        "entry": entry,
    });
    Ok(Built { arch: platform.architecture, layer, config, manifest, descriptor })
}

/// Assemble `profile`'s image at `version` from `binaries`, each `<target>=<path>`.
pub fn image(root: &Path, profile: &str, version: &str, binaries: &[String], ca_bundle: &Path, out: &Path) -> Result<()> {
    if !crate::topology::PROFILES.contains(&profile) {
        bail!("`{profile}` is no profile; one of {}", crate::topology::PROFILES.join(", "));
    }
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        bail!("`{version}` is no image tag");
    }
    let mut chosen: Vec<(&Platform, PathBuf)> = Vec::new();
    for spec in binaries {
        let (target, path) = spec.split_once('=').with_context(|| format!("`{spec}` is no `<target>=<path>`"))?;
        let platform = PLATFORMS
            .iter()
            .find(|p| p.target == target)
            .with_context(|| format!("`{target}` has no image platform; one of {}", PLATFORMS.map(|p| p.target).join(", ")))?;
        if chosen.iter().any(|(p, _)| p.target == target) {
            bail!("`{target}` given twice");
        }
        chosen.push((platform, PathBuf::from(path)));
    }
    if chosen.is_empty() {
        bail!("pass at least one `--binary <target>=<path>`");
    }
    let rt = runtime(root)?;
    let ca = std::fs::read(ca_bundle).with_context(|| format!("reading the CA bundle {}", ca_bundle.display()))?;
    let mut read = Vec::new();
    for (platform, path) in &chosen {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        check(platform, &bytes)?;
        read.push((*platform, bytes));
    }
    read.sort_by_key(|(p, _)| PLATFORMS.iter().position(|q| q.target == p.target));

    let built: Vec<Built> = read.iter().map(|(p, bytes)| build(&rt, profile, version, p, bytes, &ca)).collect::<Result<_>>()?;
    let index = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": INDEX,
        "manifests": built.iter().map(|b| b.descriptor["entry"].clone()).collect::<Vec<_>>(),
    }))?;
    let mut top = blob(INDEX, &index);
    top["annotations"] = json!({ "org.opencontainers.image.ref.name": version });
    let top = serde_json::to_vec(&json!({ "schemaVersion": 2, "mediaType": INDEX, "manifests": [top] }))?;

    let mut blobs: std::collections::BTreeMap<String, &[u8]> = Default::default();
    for b in &built {
        for bytes in [&b.layer, &b.config, &b.manifest] {
            blobs.insert(sha256(bytes), bytes);
        }
    }
    blobs.insert(sha256(&index), &index);
    let mut layout = Tar::new();
    layout.dir("blobs", (0, 0))?;
    layout.dir("blobs/sha256", (0, 0))?;
    for (digest, bytes) in &blobs {
        layout.file(&format!("blobs/sha256/{digest}"), 0o644, bytes)?;
    }
    layout.file("index.json", 0o644, &top)?;
    layout.file("oci-layout", 0o644, br#"{"imageLayoutVersion":"1.0.0"}"#)?;
    let layout = layout.finish()?;

    std::fs::create_dir_all(out).with_context(|| format!("creating {}", out.display()))?;
    for b in &built {
        let stem = format!("{profile}-{version}-linux-{}", b.arch);
        std::fs::write(out.join(format!("{stem}.layer.tar.gz")), &b.layer)?;
        let mut descriptor = serde_json::to_vec_pretty(&b.descriptor)?;
        descriptor.push(b'\n');
        std::fs::write(out.join(format!("{stem}.image.json")), descriptor)?;
        println!(
            "image: `{profile}` linux/{} manifest {} layer {} {} bytes",
            b.arch,
            b.descriptor["entry"]["digest"].as_str().unwrap_or_default(),
            b.descriptor["layer"]["digest"].as_str().unwrap_or_default(),
            b.layer.len()
        );
    }
    let archive = out.join(format!("{profile}-{version}.oci.tar"));
    std::fs::write(&archive, layout)?;
    println!("image: `{profile}` index sha256:{} in {}", sha256(&index), archive.display());
    Ok(())
}
