//! Rule module `demo-gql/no-empty-type` — a complete rule in one file.
//!
//! To write your own rule, copy this crate, change `omni_rule_meta` and the
//! check in `omni_rule_run`, build with `cargo build --release --target
//! wasm32-wasip1`, and drop the `.wasm` into `<config dir>/rules/`. Done —
//! it joins the ruleset exactly like an eslint rule and is tuned in
//! `omni-lint.toml` like every other rule.
//!
//! Reads per-file facts (`"requires": "graphql.facts"`) produced by any
//! GraphQL AST plugin (native or WASM).

use omni_guest::{write_findings, write_json, Finding, RuleEnvelope};

#[no_mangle]
pub extern "C" fn omni_rule_meta() -> i32 {
    write_json(
        r#"{
  "id": "demo-gql/no-empty-type",
  "description": "Object, interface and input definitions must define at least one field.",
  "severity": "error",
  "requires": "graphql.facts"
}"#,
    )
}

#[no_mangle]
pub extern "C" fn omni_rule_run(ptr: i32, len: i32) -> i32 {
    let envelope = RuleEnvelope::read(ptr, len);

    // Rule options come from `omni-lint.toml`:
    //   [rules."demo-gql/no-empty-type".options]
    //   allow = ["Mutation"]       # never flag these type names
    let allow: Vec<String> = envelope
        .options
        .as_ref()
        .and_then(|o| o.get("allow"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut findings = Vec::new();
    for file in &envelope.files {
        for def in &file.facts.defs {
            if !matches!(def.kind.as_str(), "object" | "interface" | "input") {
                continue;
            }
            if allow.iter().any(|n| n == &def.name) {
                continue;
            }
            if def.fields.is_empty() {
                findings.push(
                    Finding::at(
                        file.path.clone(),
                        format!("type `{}` is empty; it must define at least one field", def.name),
                        def.span,
                    )
                    .subject(def.subject.clone()),
                );
            }
        }
    }
    write_findings(&findings)
}