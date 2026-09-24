//! Rule module `demo-gql/no-unused-type`.
//!
//! Demonstrates the cross-file model: `"requires": "graphql.workspace"` means
//! the engine negotiates the language's workspace capability (built by the
//! plugin, or synthesized by the host from the per-file facts) and hands it
//! over as `envelope.workspace`. The rule is skipped when no language module
//! can provide one.

use omni_guest::{write_findings, Finding, RuleEnvelope};
use std::collections::BTreeSet;

#[no_mangle]
pub extern "C" fn omni_rule_meta() -> i32 {
    omni_guest::write_json(
        r#"{
  "id": "demo-gql/no-unused-type",
  "description": "Types not referenced anywhere and not schema roots are dead code.",
  "severity": "info",
  "requires": "graphql.workspace"
}"#,
    )
}

#[no_mangle]
pub extern "C" fn omni_rule_run(ptr: i32, len: i32) -> i32 {
    let envelope = RuleEnvelope::read(ptr, len);
    let Some(ws) = envelope.workspace_facts() else {
        return write_findings(&[]);
    };
    // Without an entry (schema) block the roots are convention-inferred,
    // which a rule cannot know — skip when ambiguous.
    if !ws.has_entry_block {
        return write_findings(&[]);
    }

    let referenced: BTreeSet<&str> = ws.uses.iter().map(|u| u.name.as_str()).collect();
    let roots: BTreeSet<&str> = ws.roots.iter().map(|(_, ty)| ty.as_str()).collect();
    let fallback_path = envelope.files.first().map(|f| f.path.clone());

    let mut findings = Vec::new();
    for def in &ws.defs {
        if def.name == "schema" || roots.contains(def.name.as_str()) {
            continue;
        }
        if referenced.contains(def.name.as_str()) {
            continue;
        }
        if let Some(path) = def.path.clone().or_else(|| fallback_path.clone()) {
            findings.push(
                Finding::at(
                    path,
                    format!("type `{}` is defined but never referenced", def.name),
                    def.span,
                )
                .subject(def.subject.clone()),
            );
        }
    }
    write_findings(&findings)
}