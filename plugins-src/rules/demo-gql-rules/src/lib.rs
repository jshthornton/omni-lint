//! Rule pack `demo-gql` — one `.wasm` contributing a bundle of rules.
//!
//! This is the eslint/rubocop packaging model: an app author ships the pack,
//! the linter extends its ruleset with everything the pack declares, and each
//! rule inside is then enabled/disabled/re-graded/configured individually in
//! `omni-lint.toml`:
//!
//! ```toml
//! # extend the pack: dropping the .wasm in rules/ is the extend
//! [rules."demo-gql/no-undefined-type"]   # per-rule control
//! enabled = false
//!
//! [rules."demo-gql/no-unused-type"]      # an off-by-default rule opting in
//! enabled = true
//! severity = "warning"
//!
//! [rules."demo-gql/duplicate-type".options]
//! max_definitions = 1
//! ```
//!
//! The pack meta grades its rules (the "recommended" set); `severity: "off"`
//! marks an opt-in rule.
//!
//! The run entry point receives the envelope for an invocation; per-rule
//! adapter calls pass exactly one entry in `envelope.rules` (with that
//! rule's options), so dispatch on `envelope.rules[].id` — a pack *could*
//! also get called with several rules at once if the host batches later.

use omni_guest::{report, write_findings, Finding, RuleEnvelope};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// meta: the bundle and its rule grading
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn omni_pack_meta() -> i32 {
    omni_guest::write_json(
        r#"{
  "namespace": "demo-gql",
  "name": "Demo GraphQL rule pack (sandboxed WASM)",
  "description": "Type-hygiene rules over the graphql facts vocabulary.",
  "requires": "graphql.workspace",
  "rules": [
    {
      "id": "demo-gql/no-undefined-type",
      "description": "Referenced types must be defined in the schema.",
      "severity": "error"
    },
    {
      "id": "demo-gql/duplicate-type",
      "description": "Type names must be unique across the schema.",
      "severity": "error"
    },
    {
      "id": "demo-gql/no-unused-type",
      "description": "Types not referenced anywhere and not schema roots are dead code.",
      "severity": "off"
    }
  ]
}"#,
    )
}

// ---------------------------------------------------------------------------
// run: dispatch on the invocation's rule subset
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn omni_rule_run(ptr: i32, len: i32) -> i32 {
    let envelope = RuleEnvelope::read(ptr, len);
    let mut all = Vec::new();
    for rule in &envelope.rules {
        let findings = match rule.id.as_str() {
            "demo-gql/no-undefined-type" => no_undefined_type(&envelope),
            "demo-gql/duplicate-type" => duplicate_type(&envelope, rule.options.as_ref()),
            "demo-gql/no-unused-type" => no_unused_type(&envelope),
            // unknown rule of this namespace requested: nothing to run
            _ => continue,
        };
        all.extend(findings);
    }
    write_findings(&all)
}

// ---------------------------------------------------------------------------
// the three rules (plain functions over the facts vocabulary)
// ---------------------------------------------------------------------------

/// demo-gql/no-undefined-type: every referenced type must be defined.
fn no_undefined_type(envelope: &RuleEnvelope) -> Vec<Finding> {
    let defined: BTreeSet<&str> = envelope
        .files
        .iter()
        .flat_map(|f| f.facts.defs.iter().map(|d| d.name.as_str()))
        .collect();
    envelope
        .files
        .iter()
        .flat_map(|f| {
            f.facts.uses.iter().filter_map(|u| {
                (!defined.contains(u.name.as_str())).then(|| {
                    Finding::at(
                        f.path.clone(),
                        format!("type `{}` is used but not defined in the schema", u.name),
                        u.span,
                    )
                    .subject(format!("type-ref:{}", u.name))
                })
            })
        })
        .collect()
}

/// demo-gql/duplicate-type: a type is defined once. Demonstrates streaming
/// findings via `env.omni_report` instead of the returned array.
fn duplicate_type(envelope: &RuleEnvelope, options: Option<&serde_json::Value>) -> Vec<Finding> {
    // options: ignore = ["*Root"] — names never flagged for duplicates
    let ignore: Vec<String> = options
        .and_then(|o| o.get("ignore"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
    for file in &envelope.files {
        for def in &file.facts.defs {
            if ignore.iter().any(|n| n == &def.name) {
                continue;
            }
            if seen.insert(def.name.as_str(), ()).is_some() {
                // stream via omni_report (host accepts either channel)
                report(
                    &Finding::at(
                        file.path.clone(),
                        format!("type `{}` is defined more than once in the schema", def.name),
                        def.span,
                    )
                    .subject(def.subject.clone()),
                );
            }
        }
    }
    Vec::new() // findings already streamed
}

/// demo-gql/no-unused-type: dead types, once an entry (schema) block makes
/// the roots explicit. `severity: "off"` in the pack meta — users opt in.
fn no_unused_type(envelope: &RuleEnvelope) -> Vec<Finding> {
    let Some(ws) = envelope.workspace_facts() else {
        return Vec::new();
    };
    if !ws.has_entry_block {
        return Vec::new();
    }
    let referenced: BTreeSet<&str> = ws.uses.iter().map(|u| u.name.as_str()).collect();
    let roots: BTreeSet<&str> = ws.roots.iter().map(|(_, ty)| ty.as_str()).collect();
    let fallback_path = envelope.files.first().map(|f| f.path.clone());

    ws.defs
        .iter()
        .filter(|d| d.name != "schema" && !roots.contains(d.name.as_str()))
        .filter(|d| !referenced.contains(d.name.as_str()))
        .filter_map(|d| {
            let path = d.path.clone().or_else(|| fallback_path.clone())?;
            Some(
                Finding::at(
                    path,
                    format!("type `{}` is defined but never referenced", d.name),
                    d.span,
                )
                .subject(d.subject.clone()),
            )
        })
        .collect()
}