//! Sandboxed WASM loading for the engine — two independent module kinds.
//!
//! **Language AST modules** (`<config dir>/plugins/*.wasm`) parse a language
//! and publish an AST/facts view. They contain no rules. Contract:
//!
//!   exports: `memory`, `omni_alloc`, `omni_plugin_meta`, `omni_plugin_parse`
//!            (optional `omni_plugin_workspace`)
//!   - `omni_plugin_meta() -> ptr` — UTF-8 JSON `PluginMeta`
//!   - `omni_plugin_parse(path_ptr, path_len, src_ptr, src_len) -> ptr`
//!       UTF-8 JSON facts document for one file (see `crate::plugin::Facts`);
//!       parse errors ride along under `errors`
//!   - `omni_plugin_workspace(ptr, len) -> ptr` — UTF-8 JSON cross-file
//!       facts; the input is `{"files": [{"path", "facts"}]}`. When a module
//!       omits this export the host synthesizes the merged view from the
//!       per-file facts automatically.
//!
//! **Rule modules** (`<config dir>/rules/*.wasm`) implement exactly one lint
//! rule each — this is what makes third-party rules as easy to add as an
//! eslint rule. Contract:
//!
//!   exports: `memory`, `omni_alloc`, `omni_rule_meta`, `omni_rule_run`
//!   - `omni_rule_meta() -> ptr` — UTF-8 JSON rule metadata
//!   - `omni_rule_run(ptr, len) -> ptr` — UTF-8 JSON array of findings for
//!       the run envelope `{"language", "files": [{path, source, facts}],
//!       "workspace", "options"}` (see `crate::rule::RuleModuleMeta`)
//!
//! Both kinds report findings through `env.omni_report(ptr, len)` and/or a
//! returned JSON array of `GuestFinding`. Every string-returning export uses
//! the `[u32 LE len][bytes]` framing documented in `session`.
//!
//! Safety: see `session` — fuel metering, no OS access, clamped spans.

mod plugin;
mod rule;
mod session;

pub use plugin::{MetaCapability, PluginMeta, WasmModule, WasmPlugin};
pub use rule::{RuleFinding, RuleModuleMeta, WasmRule};
pub use session::{GuestError, GuestFinding, WasmLimits, DEFAULT_FUEL, MAX_REPORTED_FINDINGS};

/// `.wasm` files directly under `dir` (unsorted; loaders sort).
fn wasm_paths(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("wasm"))
                .unwrap_or(false)
        })
        .collect()
}