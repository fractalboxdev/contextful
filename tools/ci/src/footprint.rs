//! `contextful-ci footprint` (`assurance.gate.footprint`): one profile's release artifact
//! compressed and held to the profile's budget from the assurance fragment, and its dynamic
//! dependencies held to the platform C library; with `--build`, each profile's
//! static-linked Linux artifact built first.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::refuse;

/// The fragment carrying each profile's `assurance-<profile>-compressed` bound.
const FRAGMENT: &str = "spec/terms/assurance.toml";

/// The zstd level the release archive compresses with.
const LEVEL: i32 = 19;

const MIB: f64 = 1024.0 * 1024.0;

/// Shared objects of the platform C library, by name prefix: glibc's split libraries and
/// its loader, and musl's single library and loader.
const PLATFORM_C: [&str; 8] = ["libc.so", "libm.so", "libpthread.so", "libdl.so", "librt.so", "ld-linux", "ld-musl-", "libc.musl-"];

/// The directory the footprint step builds into, apart from every other stage's.
pub const TARGET_DIR: &str = "target/footprint";

/// The builder a host other than Linux runs the build in: the container recipe's own,
/// Rust 1.97 on Alpine pinned by digest, whose native C library is musl.
const BUILDER: &str = "rust:1.97-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900";

/// Where the builder mounts the repository.
const MOUNT: &str = "/src";

/// The static-linked Linux target of this host's architecture: glibc linked statically on a
/// Linux host, musl inside [`BUILDER`] elsewhere. Either links the SQL engine's C++ runtime
/// into the artifact.
fn triple() -> String {
    let libc = if cfg!(target_os = "linux") { "gnu" } else { "musl" };
    format!("{}-unknown-linux-{libc}", std::env::consts::ARCH)
}

/// The cargo invocation building `profile` alone as the static-linked Linux release artifact.
fn cargo_build(profile: &str) -> Vec<String> {
    let args = ["build", "--release", "--locked", "-p", "contextful-cli", "--bin", "contextful", "--no-default-features", "--features", profile, "--target"];
    args.iter().map(|a| a.to_string()).chain([triple()]).collect()
}

/// Memory one build job may take: the SQL engine's C++ units peak near it at `-O3`.
const JOB_MEMORY: u64 = 3 << 30;

/// Parallel build jobs: the memory the build sees divided by [`JOB_MEMORY`], at least one
/// and at most the processors available.
// mirrors: assurance.gate.parallel-jobs
fn jobs() -> u64 {
    let memory = if cfg!(target_os = "linux") { linux_memory() } else { builder_memory() };
    let cpus = std::thread::available_parallelism().map(|n| n.get() as u64).unwrap_or(1);
    memory.map_or(1, |m| m / JOB_MEMORY).clamp(1, cpus)
}

/// The cgroup memory ceiling, else the physical memory, in bytes.
fn linux_memory() -> Option<u64> {
    let ceiling = std::fs::read_to_string("/sys/fs/cgroup/memory.max").ok().and_then(|t| t.trim().parse().ok());
    ceiling.or_else(|| {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kib: u64 = info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
        Some(kib * 1024)
    })
}

/// The memory of the container engine's virtual machine, in bytes.
fn builder_memory() -> Option<u64> {
    let out = Command::new("docker").args(["info", "--format", "{{.MemTotal}}"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The environment of that invocation, the target directory under `root`. A glibc target
/// links statically only under `crt-static`; musl links statically by default.
fn cargo_env(root: &str) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("CARGO_TARGET_DIR", format!("{root}/{TARGET_DIR}")),
        ("CARGO_INCREMENTAL", "0".to_string()),
        ("CARGO_BUILD_JOBS", jobs().to_string()),
    ];
    if cfg!(target_os = "linux") {
        env.push(("RUSTFLAGS", "-C target-feature=+crt-static".to_string()));
    }
    env
}

/// The command building `profile`: cargo on a Linux host, cargo inside [`BUILDER`] with its
/// C and C++ toolchain added elsewhere.
fn build_command(root: &Path, profile: &str) -> Command {
    if cfg!(target_os = "linux") {
        let mut c = Command::new("cargo");
        c.args(cargo_build(profile)).envs(cargo_env(&root.display().to_string())).current_dir(root);
        return c;
    }
    let mut c = Command::new("docker");
    c.args(["run", "--rm", "-v", &format!("{}:{MOUNT}", root.display()), "-v", "contextful-footprint-registry:/usr/local/cargo/registry", "-w", MOUNT]);
    for (k, v) in cargo_env(MOUNT) {
        c.args(["-e", &format!("{k}={v}")]);
    }
    c.args([BUILDER, "sh", "-c", &format!("apk add --no-cache -q build-base && cargo {}", cargo_build(profile).join(" "))]);
    c
}

/// The artifact a build of `profile` leaves under `root`.
pub fn artifact(root: &Path) -> PathBuf {
    root.join(TARGET_DIR).join(triple()).join("release").join("contextful")
}

/// Build each of `profiles` as the static-linked Linux target and hold each artifact to its
/// budget; with `plan`, print each build command and build nothing. Every profile is
/// measured before the first refusal propagates, and the count over budget prints last.
pub fn build(root: &Path, profiles: &[String], plan: bool) -> Result<()> {
    let mut over = Vec::new();
    for profile in profiles {
        budget_mib(root, profile)?;
        let mut c = build_command(root, profile);
        let env: Vec<String> = c.get_envs().filter_map(|(k, v)| Some(format!("{}={}", k.to_string_lossy(), v?.to_string_lossy()))).collect();
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let shown = format!("{} {} {}", env.join(" "), c.get_program().to_string_lossy(), args.join(" "));
        println!("footprint: `{profile}` builds with {}", shown.trim());
        if plan {
            continue;
        }
        let status = c.status().with_context(|| format!("running {}", c.get_program().to_string_lossy()))?;
        if !status.success() {
            let what = format!("{} {}", c.get_program().to_string_lossy(), args.join(" "));
            return Err(crate::exited(what, status).context(format!("building `{profile}`")));
        }
        if let Err(e) = check(root, profile, &artifact(root)) {
            eprintln!("{e:#}");
            over.push(format!("`{profile}`"));
        }
    }
    if plan {
        return Ok(());
    }
    println!("footprint: {} of {} profile(s) over budget", over.len(), profiles.len());
    if !over.is_empty() {
        return Err(refuse("FootprintBudgetExceeded", format!("{} over budget", over.join(", "))));
    }
    Ok(())
}

/// Measure `artifact` as `profile` and refuse it over budget or linking beyond the C library.
pub fn check(root: &Path, profile: &str, artifact: &Path) -> Result<()> {
    let budget = budget_mib(root, profile)?;
    let bytes = std::fs::read(artifact).with_context(|| format!("reading {}", artifact.display()))?;
    let needed = needed(&bytes).with_context(|| format!("{} is no ELF64 file", artifact.display()))?;
    let compressed = zstd::bulk::compress(&bytes, LEVEL).context("compressing the artifact")?.len() as f64;
    let listed = if needed.is_empty() { "none".to_string() } else { needed.join(", ") };
    println!(
        "footprint: `{profile}` {:.1} MiB, {:.1} MiB compressed of {budget} MiB, NEEDED {listed}",
        bytes.len() as f64 / MIB,
        compressed / MIB
    );
    if compressed > budget as f64 * MIB {
        return Err(refuse(
            "FootprintBudgetExceeded",
            format!("`{profile}` compresses to {:.1} MiB, over its {budget} MiB budget", compressed / MIB),
        ));
    }
    let beyond: Vec<String> = needed.iter().filter(|n| !PLATFORM_C.iter().any(|c| n.starts_with(c))).map(|n| format!("`{n}`")).collect();
    if !beyond.is_empty() {
        return Err(refuse(
            "FootprintBudgetExceeded",
            format!("`{profile}` links {} beyond the platform C library", beyond.join(", ")),
        ));
    }
    Ok(())
}

/// The compressed budget of `profile` in MiB: the `assurance-<short>-compressed` bound,
/// `<short>` being the profile name after `contextful-`.
fn budget_mib(root: &Path, profile: &str) -> Result<u64> {
    let short = profile.strip_prefix("contextful-").unwrap_or(profile);
    let key = format!("assurance-{short}-compressed");
    let text = std::fs::read_to_string(root.join(FRAGMENT)).with_context(|| format!("reading {FRAGMENT}"))?;
    let fragment: toml::Value = toml::from_str(&text).with_context(|| format!("parsing {FRAGMENT}"))?;
    let Some(bound) = fragment.get("limit").and_then(|l| l.get(&key)) else {
        bail!("{FRAGMENT} holds no `{key}` bound, so `{profile}` has no budget");
    };
    if bound.get("unit").and_then(toml::Value::as_str) != Some("MiB") {
        bail!("`{key}` is not stated in MiB");
    }
    let value = bound.get("value").and_then(toml::Value::as_integer).filter(|v| *v > 0);
    value.map(|v| v as u64).with_context(|| format!("`{key}` carries no positive integer value"))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The `DT_NEEDED` entries of a little-endian ELF64 file's dynamic section, in order; empty
/// for a static executable.
fn needed(b: &[u8]) -> Result<Vec<String>> {
    if b.get(..6) != Some(&[0x7f, b'E', b'L', b'F', 2, 1][..]) {
        bail!("the header is no little-endian ELF64 header");
    }
    let parse = || -> Option<Vec<String>> {
        let shoff = usize::try_from(u64_at(b, 0x28)?).ok()?;
        let shentsize = usize::from(u16_at(b, 0x3a)?);
        let shnum = usize::from(u16_at(b, 0x3c)?);
        let section = |i: usize| -> Option<(u32, usize, usize, u32)> {
            let at = shoff.checked_add(i.checked_mul(shentsize)?)?;
            let kind = u32_at(b, at + 4)?;
            let offset = usize::try_from(u64_at(b, at + 0x18)?).ok()?;
            let size = usize::try_from(u64_at(b, at + 0x20)?).ok()?;
            let link = u32_at(b, at + 0x28)?;
            Some((kind, offset, size, link))
        };
        let mut out = Vec::new();
        for i in 0..shnum {
            let (kind, offset, size, link) = section(i)?;
            if kind != 6 {
                continue;
            }
            let (_, strings, strings_size, _) = section(usize::try_from(link).ok()?)?;
            let table = b.get(strings..strings.checked_add(strings_size)?)?;
            for entry in b.get(offset..offset.checked_add(size)?)?.chunks_exact(16) {
                let tag = u64_at(entry, 0)?;
                if tag == 0 {
                    break;
                }
                if tag == 1 {
                    let name = table.get(usize::try_from(u64_at(entry, 8)?).ok()?..)?;
                    let end = name.iter().position(|c| *c == 0)?;
                    out.push(String::from_utf8_lossy(&name[..end]).into_owned());
                }
            }
        }
        Some(out)
    };
    parse().context("a section or dynamic entry runs past the end of the file")
}
