//! Direct guest tests over the built modules.

use omni_wasm::{WasmLimits, WasmRulePack};
use serde_json::json;

#[test]
fn echo_shows_what_guest_receives() {
    let pack = WasmRulePack::load(
        "/home/josh/code/jshthornton/omni-lint/plugins-src/target/wasm32-wasip1/release/rule_option_echo.wasm".as_ref(),
        WasmLimits::default(),
    )
    .expect("module loads");
    let envelope = json!({
        "language": "graphql",
        "files": [{ "path": "schema/a.graphql", "source": "x", "facts": { "defs": [] } }],
        "workspace": serde_json::Value::Null,
        "rules": [{ "id": "demo-gql/option-echo", "options": { "allow": ["EmptyThing"] } }],
    });
    let out = pack.call_run_dbg(&envelope.to_string()).expect("runs");
    println!("ECHO>>{out}");
    assert!(out.contains("EmptyThing"), "echo must contain options: {out}");
}
