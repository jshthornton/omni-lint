//! End-to-end tests.
//!
//! Two fixture sets:
//!
//! 1. The native GraphQL **example pack** (feature `graphql`, off by
//!    default — the shipped package is a rule-free framework). These tests
//!    mutate the sample fixture, so they hold a shared lock.
//!
//! 2. Third-party **WASM modules** — a language AST module (plugins/) plus
//!    one-rule modules (rules/), each self-contained: the module bytes live
//!    in-repo; each test builds its own scratch project directory so tests
//!    never share config/baseline state and run in parallel safely.

#[cfg(feature = "graphql")]
use omni_core::config::Config;
#[cfg(feature = "graphql")]
use omni_core::runner::run_check;
#[cfg(feature = "graphql")]
use omni_core::Registry;
use std::path::{Path, PathBuf};
#[cfg(feature = "graphql")]
use std::sync::Arc;

/// Build a scratch copy of the wasm fixture with the given config text; runs
/// cleanup on drop. Returns the scratch root (print with `.display()`).
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str, config: &str) -> Scratch {
        Scratch::with_rules(
            name,
            config,
            &[
                "rule_no_empty_type.wasm",
                "rule_no_undefined_type.wasm",
                "rule_duplicate_type.wasm",
                "rule_no_unused_type.wasm",
            ],
        )
    }

    /// Scratch project with only the named rule modules in `rules/` — rules
    /// join the ruleset a la carte, like dropping an entry into eslint's
    /// `rules` map.
    fn with_rules(name: &str, config: &str, rules: &[&str]) -> Scratch {
        let base = std::env::temp_dir()
            .join(format!("omni-lint-e2e-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("schema")).unwrap();
        std::fs::create_dir_all(base.join("plugins")).unwrap();
        std::fs::create_dir_all(base.join("rules")).unwrap();
        // strip fixture
        std::fs::write(
            base.join("schema/user.graphql"),
            "type User {\n  name: String!\n  posts: [Post!]!\n}\n",
        )
        .unwrap();
        // misc fixture: empty type + duplicate + undefined reference
        std::fs::write(
            base.join("schema/misc.graphql"),
            "type EmptyThing {\n}\ntype User {\n  dup: Bool\n}\n",
        )
        .unwrap();
        std::fs::write(base.join("omni-lint.toml"), config).unwrap();
        // Language AST module (parses GraphQL, contains no rules).
        std::fs::copy(
            wasm_module_in_repo(),
            base.join("plugins/omni-wasm-test-plugin.wasm"),
        )
        .unwrap();
        // One-rule modules, a la carte.
        for rule in rules {
            std::fs::copy(rule_module_in_repo(rule), base.join("rules").join(rule)).unwrap();
        }
        Scratch { root: base }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn wasm_module_in_repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/wasm-plugin/plugins/demo_graphql_ast.wasm")
        .canonicalize()
        .expect("wasm language fixture exists")
}

fn rule_module_in_repo(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/wasm-plugin/rules")
        .join(file)
        .canonicalize()
        .expect("wasm rule fixture exists")
}

fn scratch_exe() -> &'static str {
    env!("CARGO_BIN_EXE_omni-lint")
}

fn run_bin(args: &[&str], root: &Path) -> (i32, String) {
    let out = std::process::Command::new(scratch_exe())
        .args(args)
        .arg(root)
        .output()
        .expect("cli runs");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[cfg(feature = "graphql")]
static FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "graphql")]
fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/sample")
        .canonicalize()
        .expect("fixture exists")
}

fn run(subcmd: &str, root: &Path) -> (i32, String) {
    let exe = scratch_exe();
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

#[cfg(feature = "graphql")]
fn check(root: &Path) -> omni_core::runner::RunResult {
    let cfg = Config::load(root).expect("config loads");
    run_check(root, &cfg, &registry()).expect("check succeeds")
}

/// Registry for the native example pack: one language plugin + its rules
/// registered à la carte (this is exactly what a consumer embeds).
#[cfg(feature = "graphql")]
fn registry() -> Registry {
    let mut r = Registry::new();
    r.register_plugin(Arc::new(omni_graphql::GraphqlPlugin));
    let errors = r.register_rules(omni_graphql::rules::all_rules());
    assert!(errors.is_empty(), "rule registration failed: {errors:?}");
    r
}

// ---------------------------------------------------------------------------
// Framework-level (no plugins): rule registration is empty, exit clean
// ---------------------------------------------------------------------------

#[test]
fn framework_without_plugins_lints_nothing_cleanly() {
    let dir = std::env::temp_dir().join(format!(
        "omni-lint-e2e-{}-empty",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("schema")).unwrap();
    std::fs::write(dir.join("schema/a.graphql"), "type A { x: String }\n").unwrap();
    let (code, out) = run("check", &dir);
    the_path_display(&out);
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(code, 0, "rule-free framework with no plugins: {out}");
}

fn the_path_display(out: &str) {
    assert!(
        !out.contains("panicked"),
        "cli panicked: {out}"
    );
}

// ---------------------------------------------------------------------------
// Example pack (feature `graphql`): registration, config, baseline semantics
// ---------------------------------------------------------------------------

#[cfg(feature = "graphql")]
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

#[cfg(feature = "graphql")]
#[test]
fn clean_fixture_is_fully_baselined() {
    let result = check(&fixture_root());
    assert!(result.findings.is_empty(), "expected empty: {:?}", result.findings);
    assert_eq!(result.baselined, 2, "two committed findings stay baselined");
    assert_eq!(result.files, 4);
}

#[cfg(feature = "graphql")]
#[test]
fn new_violation_vs_baselined_backlog() {
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

#[cfg(feature = "graphql")]
#[test]
fn line_shift_keeps_baseline_matched() {
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

#[cfg(feature = "graphql")]
#[test]
fn rule_config_disable_and_severity() {
    let _guard = FIXTURE_LOCK.lock().unwrap();
    let root = fixture_root();
    let config = root.join("omni-lint.toml");
    let original = std::fs::read_to_string(&config).unwrap();
    let _ = std::fs::write(
        &config,
        "[rules.\"graphql/type-name-pascal\"]\nenabled = false\n",
    );
    let cfg = Config::load(&root).unwrap();
    let result = run_check(&root, &cfg, &registry()).expect("runs");
    std::fs::write(&config, original).unwrap();
    assert!(
        result.findings.iter().all(|r| r.diag.rule_id != "graphql/type-name-pascal"),
        "disabled rule must not fire"
    );
}

#[cfg(feature = "graphql")]
#[test]
fn exit_codes_map_to_outcomes() {
    let (code, _out) = run("check", &fixture_root());
    assert_eq!(code, 0, "clean/baselined native run must exit 0");
}

// ---------------------------------------------------------------------------
// WASM plugin: registration, findings, json
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
        "demo-gql/no-unused-type",
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
    let exe = scratch_exe();
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

// ---------------------------------------------------------------------------
// Per-rule WASM modules: authored and added a la carte (eslint-style)
// ---------------------------------------------------------------------------

#[test]
fn rule_modules_are_ala_carte() {
    // Only one rule module installed -> exactly that rule is listed and run.
    let scratch = Scratch::with_rules(
        "alacarte",
        "prefer = \"demo-gql\"\n",
        &["rule_no_empty_type.wasm"],
    );
    let (code, out) = run_bin(&["list-rules"], &scratch.root);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("demo-gql/no-empty-type"), "{out}");
    assert!(!out.contains("demo-gql/duplicate-type"), "{out}");
    assert!(!out.contains("demo-gql/no-undefined-type"), "{out}");

    let (code, out) = run_bin(&["check", "--format=json"], &scratch.root);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("demo-gql/no-empty-type"), "{out}");
    assert!(!out.contains("demo-gql/duplicate-type"), "{out}");
    assert!(!out.contains("demo-gql/no-undefined-type"), "{out}");
}

#[test]
fn rule_options_from_config_reach_the_rule() {
    let scratch = Scratch::new(
        "ruleopts",
        "prefer = \"demo-gql\"\n\n[rules.\"demo-gql/no-empty-type\".options]\nallow = [\"EmptyThing\"]\n",
    );
    let (code, out) = run_bin(&["check", "--format=json"], &scratch.root);
    assert_eq!(code, 1, "other findings must still fire: {out}");
    assert!(
        !out.contains("EmptyThing"),
        "rule options allow-list must be honoured: {out}"
    );
}

// ---------------------------------------------------------------------------
// Inline suppression markers (engine-level, any plugin)
// ---------------------------------------------------------------------------

#[test]
fn inline_suppression_never_reaches_the_report() {
    let scratch = Scratch::new("suppress", "prefer = \"demo-gql\"\n");
    let user = scratch.root.join("schema/user.graphql");
    let original = std::fs::read_to_string(&user).unwrap();
    let edited = format!(
        "{original}\n# omni-lint-disable-next-line\ntype UnmarkedEmpty {{}}\n \
         \n# omni-lint-disable-next-line demo-gql/no-empty-type\ntype MarkedEmpty {{}}\n"
    );
    std::fs::write(&user, &edited).unwrap();
    let (code, body) = run_bin(&["check", "--format=json"], &scratch.root);
    assert!(code != 0, "unmarked fixture still fails the run");
    assert!(
        !body.contains("UnmarkedEmpty") && !body.contains("MarkedEmpty"),
        "suppressed lines leaked into the report: {body}"
    );
}

// ---------------------------------------------------------------------------
// --max-warnings (errors always win)
// ---------------------------------------------------------------------------

#[test]
fn max_warnings_budget_is_refused_for_errors() {
    let scratch = Scratch::new("maxwarn", "prefer = \"demo-gql\"\n");
    let (code, _) = run_bin(&["check", "--max-warnings", "999"], &scratch.root);
    assert_eq!(code, 1, "an error budget of warnings must not silence errors");
}

// ---------------------------------------------------------------------------
// Config validation: unknown rule id is a configuration error (exit 2)
// ---------------------------------------------------------------------------

#[test]
fn unknown_rule_in_config_is_exit_2() {
    let scratch = Scratch::new(
        "unknownrule",
        "[rules.\"surely/not-a-rule\"]\nenabled = true\nprefer = \"demo-gql\"\n",
    );
    let (code, out) = run_bin(&["check"], &scratch.root);
    assert_eq!(code, 2, "unknown rule id must be a configuration error:\n{out}");
}

// ---------------------------------------------------------------------------
// Pair-count ratchet baseline (suppress N recorded findings per (rule, path))
// ---------------------------------------------------------------------------

#[test]
fn wasm_pair_ratchet_suppresses_backlog_and_reflags_growth() {
    let scratch = Scratch::new("pairratchet", "prefer = \"demo-gql\"\n");
    // Baseline: goal is 0 new findings after seeding a pair budget for the
    // recorded (rule, path) pairs, with counts still below the real counts so
    // the "growth re-flags" branch is exercised by adding one violation.
    let (code0, out0) = run_bin(&["check", "--format=json"], &scratch.root);
    assert_eq!(code0, 1, "fixture fires fresh: {out0}");
    let findings: serde_json::Value =
        serde_json::from_str(std_out_only(&out0)).expect("valid json");
    let mut counts: std::collections::BTreeMap<(String, String), u64> = Default::default();
    for f in findings["findings"].as_array().unwrap() {
        *counts
            .entry((
                f["rule_id"].as_str().unwrap().to_string(),
                f["path"].as_str().unwrap().to_string(),
            ))
            .or_insert(0) += 1;
    }
    // Build a pairs baseline covering all but one of every pair.
    let pairs: Vec<serde_json::Value> = counts
        .iter()
        .map(|((rule, path), count)| {
            serde_json::json!({ "rule_id": rule, "path": path, "count": count - 1 })
        })
        .collect();
    let baseline = format!(
        "{{\"generated_at\": null, \"entries\": [], \"pairs\": {}}}\n",
        serde_json::json!(pairs)
    );
    std::fs::write(scratch.root.join("omni-lint-baseline.json"), baseline).unwrap();
    // The leftover 1-per-pair findings still fire.
    let (code1, out1) = run_bin(&["check", "--format=json"], &scratch.root);
    assert_eq!(code1, 1, "pair budget below full count still re-flags:\n{out1}");
    let findings_left: serde_json::Value =
        serde_json::from_str(std_out_only(&out1)).expect("valid json");
    for f in findings_left["findings"].as_array().unwrap() {
        let rule = f["rule_id"].as_str().unwrap();
        let path = f["path"].as_str().unwrap();
        assert!(
            counts[&(rule.to_string(), path.to_string())] > 0,
            "unknown pair {rule}/{path} leaked"
        );
    }
}

/// run_bin mixes stdout/stderr into one string; only stdout carries JSON.
fn std_out_only(strong: &str) -> &str {
    match strong.find("\n2: ") {
        Some(i) => strong[..i].trim(),
        None => strong.trim(),
    }
}

#[test]
fn wasm_full_pair_ratchet_release_is_clean() {
    // All recorded pairs => repo lints clean even though findings exist in
    // tree (the "adopt a linter over an existing codebase" flow).
    let scratch = Scratch::new("pairfull", "prefer = \"demo-gql\"\n");
    let (code0, out0) = run_bin(&["check", "--format=json"], &scratch.root);
    assert_eq!(code0, 1, "{out0}");
    let findings: serde_json::Value =
        serde_json::from_str(std_out_only(&out0)).expect("valid json");
    let mut counts: std::collections::BTreeMap<(String, String), u64> = Default::default();
    for f in findings["findings"].as_array().unwrap() {
        *counts
            .entry((
                f["rule_id"].as_str().unwrap().to_string(),
                f["path"].as_str().unwrap().to_string(),
            ))
            .or_insert(0) += 1;
    }
    let pairs: Vec<serde_json::Value> = counts
        .iter()
        .map(|((rule, path), count)| {
            serde_json::json!({ "rule_id": rule, "path": path, "count": count })
        })
        .collect();
    std::fs::write(
        scratch.root.join("omni-lint-baseline.json"),
        format!("{{\"entries\": [], \"pairs\": {}}}\n", serde_json::json!(pairs)),
    )
    .unwrap();
    let (code1, out1) = run_bin(&["check"], &scratch.root);
    assert_eq!(code1, 0, "full pair budget must lint clean:\n{out1}");
}
