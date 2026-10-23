//! `topology.deploy`: per-provider target files and the page rendered from them.

use crate::{codes, Scratch};

const CF: &str = "spec/targets/cloudflare.toml";

#[test]
fn provider_runtime_diagrams_and_notes_come_from_the_target_file() {
    let s = Scratch::copy();
    let mut target: toml::Value = s.read(CF).parse().unwrap();
    let diagram = "flowchart LR\n  CLIENT([\"caller\"]) -- \"HTTPS\" --> ROUTER[\"routing Worker\"]\n";
    target.as_table_mut().unwrap().insert("diagram".into(), toml::Value::String(diagram.into()));
    target.as_table_mut().unwrap().insert("runtime".into(), toml::Value::String("The routing Worker executes TypeScript in a V8 isolate.".into()));
    s.write(CF, &toml::to_string(&target).unwrap());
    assert!(s.cmd(&["state"]).status.success());
    let page = s.read("spec/targets.md");
    let cloudflare = page.split("## Cloudflare\n").nth(1).unwrap().split("\n## ").next().unwrap();
    assert!(cloudflare.contains(diagram), "the provider diagram retains the execution boundary: {cloudflare}");
    assert!(cloudflare.contains("The routing Worker executes TypeScript in a V8 isolate."), "{cloudflare}");
    assert!(!cloudflare.contains("COMP[\"heavy compute\"]"), "the custom diagram replaces the generic role diagram: {cloudflare}");
    assert!(cloudflare.contains("| heavy compute |"), "the role table remains generated: {cloudflare}");
}

fn drop_line(s: &Scratch, rel: &str, prefix: &str) {
    let text = s.read(rel);
    let line = text.lines().find(|l| l.trim_start().starts_with(prefix)).unwrap_or_else(|| panic!("no `{prefix}` in {rel}")).to_string();
    s.write(rel, &text.replacen(&format!("{line}\n"), "", 1));
}

#[test]
fn the_live_targets_are_complete() {
    let s = Scratch::copy();
    let found = s.lint("targets");
    assert!(found.is_empty(), "{found:?}");
    for provider in ["cloudflare", "aws", "local"] {
        assert!(s.read(&format!("spec/targets/{provider}.toml")).contains("[[shape]]"));
    }
}

// spec: corpus.targets.file@5c435fab
#[test]
fn each_provider_file_holds_named_shapes_with_one_default() {
    let s = Scratch::copy();
    assert!(s.lint("targets").is_empty());
    for provider in ["cloudflare", "aws", "local"] {
        let text: toml::Value = s.read(&format!("spec/targets/{provider}.toml")).parse().unwrap();
        assert_eq!(text["provider"].as_str(), Some(provider));
        let shapes = text["shape"].as_array().expect("named shapes");
        assert!(!shapes.is_empty(), "{provider}");
        assert_eq!(shapes.iter().filter(|s| s.get("default").and_then(toml::Value::as_bool) == Some(true)).count(), 1, "{provider}");
        for shape in shapes {
            assert!(shape["name"].is_str() && shape["kind"].is_str() && shape["profiles"].is_array() && shape["roles"].is_table(), "{shape:?}");
        }
    }
    drop_line(&s, CF, "kind ");
    let found = codes(&s.lint("targets"), "SpecTargetIncomplete");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("kind"), "{found:?}");
}

// spec: corpus.targets.roles@fff6e767
#[test]
fn a_shape_fills_eight_roles_and_may_fill_a_realtime_projection_and_a_queue() {
    let s = Scratch::copy();
    assert!(s.read(CF).contains("realtime "), "the live Cloudflare shape fills the realtime projection");
    assert!(s.read(CF).contains("queue "), "the live Cloudflare shape fills the queue");
    assert!(s.cmd(&["state"]).status.success());
    assert!(s.read("spec/targets.md").contains("| queue |"), "the targets page renders the queue role");
    drop_line(&s, CF, "realtime ");
    drop_line(&s, CF, "queue ");
    assert!(s.lint("targets").is_empty(), "{:?}", s.lint("targets"));
    for role in ["cron_tick", "reconciler", "orchestrator", "catalog", "object_store", "query_face", "compute", "secrets"] {
        let s = Scratch::copy();
        drop_line(&s, CF, &format!("{role} "));
        let found = codes(&s.lint("targets"), "SpecTargetIncomplete");
        assert_eq!(found.len(), 1, "{role}: {found:?}");
        assert!(found[0].contains(&format!("fills no `{role}` role")), "{found:?}");
    }
}

#[test]
fn a_shape_leaving_a_role_unfilled_is_refused() {
    let s = Scratch::copy();
    drop_line(&s, CF, "catalog ");
    let found = codes(&s.lint("targets"), "SpecTargetIncomplete");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("catalog") && found[0].contains("cloudflare"), "{found:?}");
}

#[test]
fn a_capped_shape_not_recording_its_exclusions_is_refused() {
    let s = Scratch::copy();
    let aws = s.read("spec/targets/aws.toml");
    assert!(aws.contains("wall_clock_cap_min = 15"), "the live AWS file carries a capped shape");
    let line = aws.lines().find(|l| l.trim_start().starts_with("excludes") && l.contains("first-time-backfill")).unwrap().to_string();
    s.write("spec/targets/aws.toml", &aws.replacen(&line, r#"excludes = ["first-time-backfill"]"#, 1));
    let found = codes(&s.lint("targets"), "SpecTargetCapUnrecorded");
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn a_function_class_shape_hosting_the_full_profile_is_refused() {
    let s = Scratch::copy();
    let aws = s.read("spec/targets/aws.toml");
    let line = aws.lines().find(|l| l.trim_start().starts_with("profiles = [\"edge\"]")).expect("a function-class shape").to_string();
    s.write("spec/targets/aws.toml", &aws.replacen(&line, "profiles = [\"edge\", \"full\"]", 1));
    let found = codes(&s.lint("targets"), "SpecTargetFunctionProfile");
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn the_targets_page_is_rendered_from_the_files() {
    let s = Scratch::copy();
    assert!(s.cmd(&["state"]).status.success());
    let page = s.read("spec/targets.md");
    assert!(page.contains("Durable Object"), "{page}");
    assert!(page.contains("Step Functions"), "{page}");
    assert_eq!(page.matches("```mermaid").count(), 3, "one diagram per provider:\n{page}");
    // the deploying account and its planes are containers; the caller stands outside them
    assert_eq!(page.matches("  subgraph ACCOUNT[").count(), 3, "{page}");
    assert!(page.contains("    subgraph WORKERS[\"Workers V8 isolates\"]"), "{page}");
    assert!(page.find("CALLER([").unwrap() < page.find("subgraph ACCOUNT[").unwrap(), "{page}");

    let cf = s.read(CF).replace("Workflows", "Queues");
    s.write(CF, &cf);
    let stale = codes(&s.lint("render"), "SpecStaleRender");
    assert!(stale.iter().any(|m| m.contains("spec/targets.md")), "{stale:?}");
}

#[test]
fn the_default_shape_diagrams_name_provider_runtimes_and_processes() {
    let s = Scratch::copy();
    assert!(s.cmd(&["state"]).status.success());
    let page = s.read("spec/targets.md");
    for label in ["Fargate Linux tasks", "Step Functions", "Workers V8 isolates", "Linux Container running contextful-full", "Linux host", "contextful pipeline serve process", "Wasmtime component host", "DuckDB library"] {
        assert!(page.contains(label), "missing runtime or process {label}: {page}");
    }
    assert!(!page.contains("COMP[\"heavy compute\"]"), "{page}");
}

#[test]
fn the_default_shape_diagrams_obey_the_diagram_rules() {
    let s = Scratch::copy();
    assert!(s.cmd(&["state"]).status.success());
    let page = s.read("spec/targets.md");
    let before = s.lint("diagram");
    let fences: Vec<&str> = page.split("```mermaid\n").skip(1).map(|b| b.split("```").next().unwrap()).collect();
    let guide = s.read("spec/guide/store.md");
    let appended: String = fences.iter().map(|f| format!("\n```mermaid\n{f}```\n")).collect();
    s.write("spec/guide/store.md", &format!("{guide}{appended}"));
    let mut after = s.lint("diagram");
    for f in before {
        if let Some(i) = after.iter().position(|a| *a == f) {
            after.remove(i);
        }
    }
    assert!(after.is_empty(), "{after:?}");
}

/// Remove `item` from the `key = [...]` line of the file's provider header.
fn drop_item(s: &Scratch, rel: &str, key: &str, item: &str) {
    let text = s.read(rel);
    let line = text.lines().find(|l| l.starts_with(key)).unwrap_or_else(|| panic!("no `{key}` in {rel}")).to_string();
    let quoted = format!("\"{item}\"");
    assert!(line.contains(&quoted), "{line}");
    let kept: Vec<&str> = line.split_once('[').unwrap().1.trim_end_matches(']').split(',').map(str::trim).filter(|p| !p.is_empty() && *p != quoted).collect();
    s.write(rel, &text.replacen(&line, &format!("{key} = [{}]", kept.join(", ")), 1));
}

// spec: corpus.targets.primitives@87b01eb0
#[test]
fn a_target_file_lists_its_primitives_and_each_shape_the_ones_it_uses() {
    let s = Scratch::copy();
    for provider in ["cloudflare", "aws", "local"] {
        let text: toml::Value = s.read(&format!("spec/targets/{provider}.toml")).parse().unwrap();
        let offered: Vec<&str> = text["primitives"].as_array().expect("provider primitives").iter().map(|p| p.as_str().unwrap()).collect();
        assert!(!offered.is_empty(), "{provider}");
        for shape in text["shape"].as_array().unwrap() {
            let uses = shape["uses"].as_array().unwrap_or_else(|| panic!("{provider} {shape:?} lists no primitives it uses"));
            assert!(!uses.is_empty(), "{provider} {shape:?}");
            assert!(uses.iter().all(|u| offered.contains(&u.as_str().unwrap())), "{provider} {shape:?}");
        }
    }
    drop_line(&s, CF, "uses ");
    let found = codes(&s.lint("targets"), "SpecTargetIncomplete");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("`cloudflare`") && found[0].contains("`worker-container-d1`"), "a missing `uses` reads as an empty one: {found:?}");
    assert!(s.cmd(&["state"]).status.success());
    assert!(s.read("spec/targets.md").contains("## Cloudflare"), "the Cloudflare section still renders");
}

// spec: topology.deploy.unsupported-shape@d34fd162
#[test]
fn a_shape_claiming_containers_on_a_provider_without_them_is_refused() {
    let s = Scratch::copy();
    assert!(codes(&s.lint("targets"), "TargetShapeUnsupported").is_empty());
    drop_item(&s, CF, "primitives", "containers");
    let found = codes(&s.lint("targets"), "TargetShapeUnsupported");
    assert_eq!(found.len(), 1, "the Cloudflare deployment runs on a container: {found:?}");
    assert!(found.iter().all(|m| m.contains("cloudflare") && m.contains("`containers`")), "{found:?}");
    assert!(found.iter().any(|m| m.contains("`worker-container-d1`")), "{found:?}");

    let s = Scratch::copy();
    drop_item(&s, "spec/targets/aws.toml", "primitives", "functions");
    let found = codes(&s.lint("targets"), "TargetShapeUnsupported");
    assert!(found.iter().any(|m| m.contains("`lambda-only`") && m.contains("`functions`")), "{found:?}");
    assert!(!found.iter().any(|m| m.contains("`fargate-daemon`")), "a daemon shape runs no function: {found:?}");
}
