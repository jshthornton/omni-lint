//! Rule module `demo-gql/duplicate-type`.
//!
//! Demonstrates the streaming report channel: findings are pushed to the
//! host via `env.omni_report` as they are found (the host also accepts a
//! returned JSON array — pick either, see `no-empty-type`).

use omni_guest::{report, write_json, Finding, RuleEnvelope};
use std::collections::BTreeMap;

#[no_mangle]
pub extern "C" fn omni_rule_meta() -> i32 {
    write_json(
        r#"{
  "id": "demo-gql/duplicate-type",
  "description": "Type names must be unique across the schema.",
  "severity": "error",
  "requires": "graphql.facts"
}"#,
    )
}

#[no_mangle]
pub extern "C" fn omni_rule_run(ptr: i32, len: i32) -> i32 {
    let envelope = RuleEnvelope::read(ptr, len);

    let mut seen: BTreeMap<&str, (String, [u32; 2])> = BTreeMap::new();
    for file in &envelope.files {
        for def in &file.facts.defs {
            match seen.get(def.name.as_str()) {
                None => {
                    seen.insert(def.name.as_str(), (file.path.clone(), def.span));
                }
                Some(_) => {
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
    }
    write_json("[]")
}