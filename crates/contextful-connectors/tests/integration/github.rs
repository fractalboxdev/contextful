//! The GitHub recipe, driven against responses recorded from the GitHub REST API.

use crate::support::{request, resolver, Never, Response, Server};
use contextful_connectors::http::{HttpConfig, HttpSource};
use contextful_core::pipeline::declare::{read_manifest, ManifestFile, PipelineSpec};
use contextful_core::pipeline::transform::apply;
use contextful_core::run::advance::{frontier, watermark};
use serde_json::{json, Value};

const RECIPE: &str = include_str!("../../../../recipes/github.toml");
const ISSUES: [&str; 2] = [include_str!("../fixtures/github/issues-page-1.json"), include_str!("../fixtures/github/issues-page-2.json")];
const COMMITS: [&str; 2] = [include_str!("../fixtures/github/commits-page-1.json"), include_str!("../fixtures/github/commits-page-2.json")];
const TABLE: &str = "octocat/Hello-World";

fn recipe() -> Vec<PipelineSpec> {
    let specs: Vec<PipelineSpec> = read_manifest(&ManifestFile { path: "recipes/github.toml".into(), text: RECIPE.into() }).unwrap().into_iter().map(|d| d.spec).collect();
    for spec in &specs {
        spec.validate().unwrap();
        for op in &spec.transforms {
            op.validate().unwrap();
        }
    }
    specs
}

/// A loopback server replaying two recorded pages at `path`, the first linking the second.
fn replay(path: &'static str, pages: [&'static str; 2]) -> Server {
    let port = std::sync::Arc::new(std::sync::OnceLock::<u16>::new());
    let seen = port.clone();
    let server = Server::start(move |r| match r.query("page").as_deref() {
        Some("2") => Response::json(200, pages[1]),
        _ => Response {
            status: 200,
            headers: vec![
                ("Content-Type".into(), "application/json; charset=utf-8".into()),
                ("Link".into(), format!("<http://127.0.0.1:{}{path}?page=2>; rel=\"next\"", seen.get().copied().unwrap_or_default())),
            ],
            body: pages[0].as_bytes().to_vec(),
        },
    });
    port.set(server.port).unwrap();
    server
}

/// The recipe's source for the one table, its host swapped for the loopback server.
fn source(spec: &PipelineSpec, server: &Server) -> HttpSource {
    let mut config = spec.source.config.clone();
    let endpoint = config["endpoint"].as_str().unwrap().replace("https://api.github.com", &server.url(""));
    config["endpoint"] = json!(endpoint);
    HttpSource::new(HttpConfig::parse(&config).unwrap(), TABLE, resolver(vec![("github-token", "ghp_recorded")])).unwrap()
}

/// `recipes/github.toml` lands issues without pull requests and commits per `<owner>/<repo>` table, following the
/// `Link` header, stamping `repo_full_name`, and clocking commits at `/commit/committer/date`.
// spec: connector.source.github-recipe@6d93e2d8
#[test]
fn the_recipe_lands_issues_without_pull_requests_and_stamps_the_repository() {
    let spec = recipe().into_iter().find(|s| s.id == "github_issues").unwrap();
    assert_eq!(spec.incremental.as_deref(), Some("updated_at"));
    let server = replay("/repos/octocat/Hello-World/issues", ISSUES);
    let s = source(&spec, &server);

    let position = watermark("updated_at", json!("2026-01-01T00:00:00Z"));
    let fetched = s.walk(&request(Some(position)), &Never).unwrap();
    assert_eq!(fetched.len(), 3, "both recorded pages, the pull request included");
    assert_eq!(frontier("updated_at", &fetched).unwrap(), Some(json!("2026-09-29T12:58:46Z")));

    let landed: Vec<Value> = apply(&spec.transforms, fetched, "github_issues_octocat_hello_world").unwrap().into_iter().map(Value::Object).collect();
    assert_eq!(landed.iter().map(|r| r["number"].clone()).collect::<Vec<_>>(), [json!(7), json!(12)], "pull request 1 is dropped");
    for r in &landed {
        assert_eq!(r["repo_full_name"], json!(TABLE));
        assert!(r.get("pull_request").is_none() && r.get("user").is_none());
    }
    assert_eq!(landed[0]["title"], json!("Hello World in all programming languages"));
    assert_eq!((landed[0]["author_login"].clone(), landed[1]["author_login"].clone()), (json!("mwumvaxgh"), json!("dako3256")));

    let seen = server.received("/repos/octocat/Hello-World/issues");
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].query("since").as_deref(), Some("2026-01-01T00:00:00Z"));
    assert_eq!((seen[0].query("state").as_deref(), seen[0].query("per_page").as_deref()), (Some("all"), Some("100")));
    for r in &seen {
        assert_eq!(r.header("authorization"), Some("Bearer ghp_recorded"));
        assert_eq!(r.header("x-github-api-version"), Some("2022-11-28"));
    }
}

#[test]
fn the_recipe_lands_commits_under_a_nested_committer_clock() {
    let spec = recipe().into_iter().find(|s| s.id == "github_commits").unwrap();
    let clock = spec.incremental.clone().unwrap();
    assert_eq!(clock, "/commit/committer/date");
    let server = replay("/repos/octocat/Hello-World/commits", COMMITS);
    let fetched = source(&spec, &server).walk(&request(None), &Never).unwrap();
    assert_eq!(frontier(&clock, &fetched).unwrap(), Some(json!("2012-03-06T23:06:50Z")));

    let landed: Vec<Value> = apply(&spec.transforms, fetched, "github_commits_octocat_hello_world").unwrap().into_iter().map(Value::Object).collect();
    assert_eq!(
        landed.iter().map(|r| r["sha"].as_str().unwrap()[..7].to_string()).collect::<Vec<_>>(),
        ["7fd1a60", "7629413", "553c207"]
    );
    assert_eq!(
        landed[2],
        json!({
            "sha": "553c2077f0edc3d5dc5d17262f6aa498e69d6f8e",
            "repo_full_name": TABLE,
            "committed_at": "2011-01-26T19:06:08Z",
            "message": "first commit",
            "author_name": "cameronmcefee",
            "author_login": "Cameron423698",
            "html_url": "https://github.com/octocat/Hello-World/commit/553c2077f0edc3d5dc5d17262f6aa498e69d6f8e",
        })
    );
    assert!(landed.iter().all(|r| r["author_name"].is_string()));
}
