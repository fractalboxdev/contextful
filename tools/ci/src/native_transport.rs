//! The sole GitHub Actions transport carries native Windows work admitted by FlareDispatch.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;

fn fields(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value.as_object().context("native executor field is not an object")?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        bail!("native executor contains undeclared configuration or credentials");
    }
    Ok(())
}

pub fn check(root: &Path) -> Result<()> {
    let dir = root.join(".github/workflows");
    let entries = std::fs::read_dir(&dir)?.collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() != 1 || entries[0].file_name() != "native-windows.yml" {
        bail!("native transport requires exactly the dispatch-only native-windows.yml executor");
    }
    // JSON is a YAML subset; the fixed executor needs no second parser or trigger dialect.
    let workflow: Value = serde_json::from_slice(&std::fs::read(dir.join("native-windows.yml"))?)
        .context("native executor must use the JSON subset of YAML")?;
    fields(&workflow, &["name", "run-name", "on", "permissions", "jobs"])?;
    if workflow["run-name"] != "native-${{ fromJSON(inputs.request).nonce }}" {
        bail!("native executor run identity is not nonce-bound");
    }
    if workflow["on"] != json!({"workflow_dispatch": {"inputs": {"request": {"required": true, "type": "string"}}}})
        || workflow["permissions"] != json!({"contents": "read"})
    {
        bail!("native executor requires workflow_dispatch alone and read-only contents permission");
    }
    let jobs = workflow["jobs"].as_object().context("native executor has no jobs")?;
    if jobs.len() != 2 { bail!("native executor must bind both fixed Windows runner jobs"); }
    for (name, label, target) in [
        ("x86_64", "windows-2025", "x86_64-pc-windows-msvc"),
        ("aarch64", "windows-11-arm", "aarch64-pc-windows-msvc"),
    ] {
        let job = jobs.get(name).context("native executor is missing a fixed runner job")?;
        fields(job, &["if", "runs-on", "timeout-minutes", "steps"])?;
        if job["if"] != format!("${{{{ fromJSON(inputs.request).target == '{target}' }}}}") {
            bail!("native executor does not select its fixed target");
        }
        if job["runs-on"] != label || job["timeout-minutes"].as_u64().is_none_or(|n| n == 0 || n > 45)
            || job.get("permissions").is_some()
        { bail!("native executor runner, timeout or permissions mismatch for {name}"); }
        let steps = job["steps"].as_array().context("native executor has no steps")?;
        if steps.len() != 4
            || steps[0]["uses"] != "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683"
            || steps[0]["with"] != json!({"ref":"${{ github.sha }}","path":"executor","persist-credentials":false})
            || steps[1]["uses"] != "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683"
            || steps[1]["with"] != json!({"ref":"${{ fromJSON(inputs.request).head }}","path":"workload","persist-credentials":false,"fetch-depth":0})
            || steps[3]["uses"] != "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"
            || steps[3]["if"] != "${{ always() }}"
            || steps[3]["with"] != json!({"name":"native-${{ fromJSON(inputs.request).nonce }}","path":"native-output","if-no-files-found":"error","retention-days":7})
        { bail!("native executor checkout or artifact identity is unbound"); }
        let mut runner = false;
        for step in steps {
            if let Some(run) = step.get("run") {
                fields(step, &["run", "env"])?;
                fields(&step["env"], &["NATIVE_REQUEST", "NATIVE_TARGET"])?;
                if runner || run != "pwsh -File executor/.github/native-windows.ps1"
                    || step["env"]["NATIVE_REQUEST"] != "${{ inputs.request }}"
                    || step["env"]["NATIVE_TARGET"] != target
                { bail!("native executor contains an unbound or arbitrary command"); }
                runner = true;
            } else {
                fields(step, &["uses", "with", "if"])?;
                let uses = step["uses"].as_str().context("native executor has an unknown step")?;
                let (action, revision) = uses.split_once('@').context("native action is not immutable")?;
                if !matches!(action, "actions/checkout" | "actions/upload-artifact")
                    || revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_hexdigit())
                { bail!("native action is untrusted or not pinned"); }
                if action == "actions/checkout" && step["with"]["persist-credentials"] != false {
                    bail!("native checkout retains credentials");
                }
                if action == "actions/checkout" {
                    fields(&step["with"], &["ref", "path", "persist-credentials", "fetch-depth"])?;
                } else {
                    fields(&step["with"], &["name", "path", "if-no-files-found", "retention-days"])?;
                    if step["with"]["path"] != "native-output" { bail!("native artifact leaves its owned output directory"); }
                }
            }
        }
        if !runner { bail!("native executor has no bound native command"); }
    }
    if !root.join(".github/native-windows.ps1").is_file() {
        bail!("native executor wrapper is absent");
    }
    Ok(())
}
