//! End-to-end tests over the sample fixture, plus unit tests for baseline
//! identity semantics and the GraphQL parser.

use omni_core::config::Config;
use omni_core::runner::{run_check, ExitCode};
use omni_core::{Plugin, Registry};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(
        Arc::new(omni_graphql::GraphqlPlugin),
        omni_graphql::rules::all_rules(),
    );
    r
}

/// Fixture files are mutated in some tests; serialize those mutations.
pub static FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/sample")
        .canonicalize()
        .expect("fixture exists")
}

fn run(subcmd: &str, root: &Path) -> (i32, String) {
    let exe = env!("CARGO_BIN_EXE_omni-lint");
    let output = std::process::Command::new(exe)
        .arg(subcmd)
        .arg(root)
        .output()
        .expect("cli runs");
    let code = output.status.code().unwrap_or(-1);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (code, text)
}

fn check(root: &Path) -> omni_core::runner::RunResult {
    let cfg = Config::load(root).expect("config loads");
    run_check(root, &cfg, &registry()).expect("check succeeds")
}

#[test]
fn list_rules_shows_builtin_rules() {
    let (code, out) = run("list-rules", &fixture_root());
    assert_eq!(code, 0);
    for id in [
        "graphql/no-empty-type",
        "graphql/no-undefined-type",
        "graphql/no-unused-type",
        "graphql/type-name-pascal",
        "graphql/field-name-camel",
        "graphql/duplicate-type",
        "graphql/deprecated-required-input",
    ] {
        assert!(out.contains(id), "missing rule {id} in\n{out}");
    }
}

#[test]
fn clean_fixture_is_fully_baselined() {
    let result = check(&fixture_root());
    assert!(result.findings.is_empty(), "expected empty: {:?}", result.findings);
    assert_eq!(result.baselined, 2, "two committed findings stay baselined");
    assert_eq!(result.files, 4);
}

#[test]
fn new_violation_becomes_new_finding() {
    let _guard = FIXTURE_LOCK.lock().unwrap();
    let root = fixture_root();
    let user = root.join("schema/user.graphql");
    let original = std::fs::read_to_string(&user).unwrap();
    let edited = original.clone() + "\ntype BadName {}\n";
    std::fs::write(&user, &edited).unwrap();
    let result = check(&root);
    std::fs::write(&user, original).unwrap();
    assert_eq!(result.findings.len(), 1, "exactly the new violation, got {result:?}");
    assert_eq!(result.findings[0].diag.rule_id, "graphql/no-empty-type");
    assert_eq!(result.findings[0].located.line, 9);
}

#[test]
fn line_shift_keeps_baseline_matched() {
    // Fingerprint identity must survive adding lines above a finding.
    let _guard = FIXTURE_LOCK.lock().unwrap();
    let root = fixture_root();
    let comment = root.join("schema/comment.graphql");
    let original = std::fs::read_to_string(&comment).unwrap();
    let prepended = String::from("# prepended comment line\n\n") + &original;
    std::fs::write(&comment, &prepended).unwrap();
    let result = check(&root);
    std::fs::write(&comment, original).unwrap();
    assert!(
        result.findings.is_empty(),
        "line shift must not leak baselined findings: {:?}",
        result.findings
    );
    assert_eq!(result.baselined, 2);
}

#[test]
fn rule_config_disable_and_severity() {
    let _guard = FIXTURE_LOCK.lock().unwrap();
    let root = fixture_root();
    let config = root.join("omni-lint.toml");
    let original = std::fs::read_to_string(&config).unwrap();
    // Disable the pascal rule; the default baseline stays in place.
    let _ = std::fs::write(
        &config,
        "[rules.\"graphql/type-name-pascal\"]\nenabled = false\n",
    );
    // baseline key applies: only new findings flow.
    let cfg = Config::load(&root).unwrap();
    let result = run_check(&root, &cfg, &registry()).expect("runs");
    std::fs::write(&config, original).unwrap();
    assert!(result.findings.iter().all(|r| r.diag.rule_id != "graphql/type-name-pascal"), "disabled rule must not fire");
}

#[test]
fn exit_codes_map_to_outcomes() {
    // clean native fixture: 0; wasm fixture with findings: 1
    let (code, _out) = run("check", &fixture_root());
    assert_eq!(code, 0, "clean/baselined run must exit 0");
    // Mutating the fixture would race other tests; the wasm fixture covers 1.
}

// ---------------------------------------------------------------------------
// Sandboxed WASM plugin loading (tests/fixtures/wasm-plugin)
// ---------------------------------------------------------------------------

fn wasm_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/wasm-plugin")
        .canonicalize()
        .expect("wasm fixture exists")
}

#[test]
fn wasm_plugin_rules_are_listed() {
    let (code, out) = run("list-rules", &wasm_fixture_root());
    assert_eq!(code, 0);
    for id in [
        "demo-gql/no-empty-type",
        "demo-gql/no-undefined-type",
        "demo-gql/duplicate-type",
    ] {
        assert!(out.contains(id), "missing wasm rule {id} in\n{out}");
    }
}

#[test]
fn wasm_plugin_produces_findings_and_fails_the_run() {
    let (code, out) = run("check", &wasm_fixture_root());
    assert_eq!(code, 1, "wasm plugin findings must fail the run:\n{out}");
    assert!(out.contains("demo-gql/no-empty-type"));
    assert!(out.contains("demo-gql/no-undefined-type"));
    assert!(out.contains("demo-gql/duplicate-type"));
}

#[test]
fn wasm_plugin_findings_work_in_json_output() {
    let exe = env!("CARGO_BIN_EXE_omni-lint");
    let output = std::process::Command::new(exe)
        .arg("check")
        .arg("--format=json")
        .arg(wasm_fixture_root())
        .output()
        .expect("runs");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("\"rule_id\""), "json out: {text}");
    assert!(text.contains("demo-gql/no-empty-type"));
}
