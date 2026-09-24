//! Direct guest tests over the committed fixture pack module.

use omni_wasm::{RuleFinding, WasmLimits, WasmRulePack};
use serde_json::json;

fn pack_module() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/wasm-plugin/rules/demo_gql_rules.wasm"
    )
}

#[test]
fn pack_workspace_rule_flags_unused_type() {
    let pack = WasmRulePack::load(std::path::Path::new(pack_module()), WasmLimits::default())
        .expect("pack loads");
    let facts = json!({
        "defs": [
            { "kind": "object", "name": "QueryRoot", "span": [30, 39], "subject": "type:QueryRoot", "fields": [] },
            { "kind": "object", "name": "Lonely", "span": [40, 46], "subject": "type:Lonely", "fields": [] }
        ],
        "uses": [],
        "directives_defined": [],
        "directives_used": [],
        "has_entry_block": true,
        "roots": [["query", "QueryRoot"]]
    });
    let workspace = json!({
        "defs": [
            { "kind": "object", "name": "QueryRoot", "span": [30, 39], "subject": "type:QueryRoot", "fields": [], "path": "schema/entry.graphql" },
            { "kind": "object", "name": "Lonely", "span": [40, 46], "subject": "type:Lonely", "fields": [], "path": "schema/entry.graphql" }
        ],
        "uses": [],
        "directives_defined": [],
        "directives_used": [],
        "has_entry_block": true,
        "roots": [["query", "QueryRoot"]]
    });
    let envelope = json!({
        "language": "graphql",
        "files": [{ "path": "schema/entry.graphql", "source": "...", "facts": facts }],
        "workspace": workspace,
        "options": serde_json::Value::Null,
        "rules": [{ "id": "demo-gql/no-unused-type", "options": serde_json::Value::Null }],
    });
    let out = pack.call_run_dbg(&envelope.to_string()).expect("runs");
    let findings: Vec<RuleFinding> = serde_json::from_str(&out).expect("valid");
    assert_eq!(findings.len(), 1, "Lonely must be flagged: {out}");
    assert_eq!(findings[0].subject.as_deref(), Some("type:Lonely"));
}
