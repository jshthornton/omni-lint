//! Rule module `demo-gql/no-undefined-type`.
//!
//! Cross-file rule without a language-side model: the envelope carries every
//! file's facts, so the union of definitions is just a fold over `files`.
//! Reads per-file facts (`"requires": "graphql.facts"`).

use omni_guest::{write_findings, Finding, RuleEnvelope};
use std::collections::BTreeSet;

#[no_mangle]
pub extern "C" fn omni_rule_meta() -> i32 {
    omni_guest::write_json(
        r#"{
  "id": "demo-gql/no-undefined-type",
  "description": "Referenced types must be defined in the schema.",
  "severity": "error",
  "requires": "graphql.facts"
}"#,
    )
}

#[no_mangle]
pub extern "C" fn omni_rule_run(ptr: i32, len: i32) -> i32 {
    let envelope = RuleEnvelope::read(ptr, len);

    let defined: BTreeSet<&str> = envelope
        .files
        .iter()
        .flat_map(|f| f.facts.defs.iter().map(|d| d.name.as_str()))
        .collect();

    let mut findings = Vec::new();
    for file in &envelope.files {
        for u in &file.facts.uses {
            if defined.contains(u.name.as_str()) {
                continue;
            }
            findings.push(
                Finding::at(
                    file.path.clone(),
                    format!("type `{}` is used but not defined in the schema", u.name),
                    u.span,
                )
                .subject(format!("type-ref:{}", u.name)),
            );
        }
    }
    write_findings(&findings)
}