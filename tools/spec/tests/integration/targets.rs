//! `topology.deploy`: per-provider target files and the page rendered from them.

use crate::{codes, Scratch};

const CF: &str = "spec/targets/cloudflare.toml";

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

    let cf = s.read(CF).replace("Workflows", "Queues");
    s.write(CF, &cf);
    let stale = codes(&s.lint("render"), "SpecStaleRender");
    assert!(stale.iter().any(|m| m.contains("spec/targets.md")), "{stale:?}");
}
