//! Compile-only parts (`assurance.gate.compile-only`) and their compile groups
//! (`assurance.gate.compile-groups`): a part's own `cargo test` invocations with `--no-run`,
//! in its own target directory, so a compile cache warmed by one serves the real part.

use anyhow::Result;
use std::path::Path;

/// The `cargo test` invocations `stage`'s `only` parts compile, in run order and each
/// once: every invocation the parts run, its arguments after `--` dropped and `--no-run`
/// appended. Every CLI shard compiles the integration target and the unit tests, because
/// which shard runs the unit tests follows from a test listing; cargo refuses `--doc`
/// with `--no-run`, so no doctest invocation appears.
pub fn invocations(root: &Path, stage: &str, only: Option<&[String]>) -> Result<Vec<Vec<String>>> {
    let owned = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<String>>();
    let runs = match stage {
        "workspace" => {
            let (mut runs, shards) = crate::workspace_runs(root, only)?;
            if !shards.is_empty() {
                runs.push(owned(&crate::CLI_INTEGRATION));
                if let Some(units) = crate::cli_units(&crate::target_kinds(root, crate::topology::BINARY)?) {
                    runs.push(owned(&units));
                }
            }
            runs
        }
        "acceptance" => vec![owned(&crate::ACCEPTANCE_RUN)],
        "evaluate" => crate::measure::test_runs(root, only)?,
        "features" => crate::feature_runs(&crate::featured_packages(root)?, only).into_iter().map(|(_, args)| args).collect(),
        _ => Vec::new(),
    };
    let mut out: Vec<Vec<String>> = Vec::new();
    for mut args in runs {
        if let Some(end) = args.iter().position(|a| a == "--") {
            args.truncate(end);
        }
        if !args.iter().any(|a| a == "--no-run") {
            args.push("--no-run".into());
        }
        if !out.contains(&args) {
            out.push(args);
        }
    }
    Ok(out)
}

/// Run `stage`'s compile-only invocations in its own target directory.
pub fn run(root: &Path, stage: &str, only: Option<&[String]>) -> Result<()> {
    let runs = invocations(root, stage, only)?;
    if runs.is_empty() {
        eprintln!("compile-only: stage `{stage}` runs no `cargo test`");
    }
    for args in &runs {
        crate::run_staged(root, stage, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    Ok(())
}

/// The first of `parts` with each distinct compile signature, in their order: a part's
/// stage, which fixes its target directory, and its compile-only invocations. A part that
/// compiles nothing names no group.
pub fn groups(root: &Path, parts: Vec<String>) -> Result<Vec<String>> {
    let mut seen: Vec<(String, Vec<Vec<String>>)> = Vec::new();
    let mut out = Vec::new();
    for part in parts {
        let (stage, only) = part.split_once('.').map_or((part.as_str(), None), |(stage, p)| (stage, Some(vec![p.to_string()])));
        let signature = (stage.to_string(), invocations(root, stage, only.as_deref())?);
        if signature.1.is_empty() || seen.contains(&signature) {
            continue;
        }
        seen.push(signature);
        out.push(part);
    }
    Ok(out)
}
