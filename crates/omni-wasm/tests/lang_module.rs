//! Probe the language module's facts over the scratch sources.

use omni_wasm::{WasmLimits, WasmPlugin};
use serde_json::json;

#[test]
fn language_module_facts_for_entry_block() {
    let module = WasmPlugin::load(
        "../../tests/fixtures/wasm-plugin/plugins/demo_graphql_ast.wasm".as_ref(),
        WasmLimits::default(),
    )
    .expect("loads");
    let src = "schema {\n  query: QueryRoot\n}\ntype QueryRoot { x: String }\ntype Lonely { y: String }\n";
    let (facts, _errors) = module.module().call_parse("schema/entry.graphql", src).expect("parses");
    let v: serde_json::Value = serde_json::from_str(&facts).expect("valid facts");
    println!("FACTS>>{facts}");
    assert_eq!(v["has_entry_block"], json!(true), "schema block must set has_entry_block: {facts}");
    assert!(v["roots"].as_array().map(|r| !r.is_empty()).unwrap_or(false), "roots captured: {facts}");
}
