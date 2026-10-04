use super::Repo;

fn refused(out: &std::process::Output, code: &str) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "source lint passed unexpectedly: {stderr}");
    assert!(stderr.contains(code), "{stderr}");
    stderr
}

/// A source lint over the runtime crates finding a subject claim formatted into SQL text raises `EnforceInterpolatedSubjectClaim`, naming the file and line.
#[test]
fn interpolated_subject_claim_names_the_source_line() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn query(subject: &str) -> String {\n    format!(\"SELECT * FROM rows WHERE subject = '{subject}'\")\n}\n",
    );

    let err = refused(&r.run_ci(&["source-lint"]), "EnforceInterpolatedSubjectClaim");
    assert!(err.contains("crates/demo/src/lib.rs:2"), "{err}");
}

#[test]
fn subject_binding_and_non_sql_formatting_pass_source_lint() {
    let r = Repo::init();
    r.write(
        "crates/demo/src/lib.rs",
        "pub fn query(subject: &str) -> (&'static str, &str) {\n    (\"SELECT * FROM rows WHERE subject = ?\", subject)\n}\npub fn log(subject: &str) -> String { format!(\"subject: {subject}\") }\n",
    );

    let out = r.run_ci(&["source-lint"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
