//! Language AST modules: parse a language, publish `*.facts` (and optional
//! `*.workspace`) JSON capabilities. No rules live here — rules are separate
//! units in `crate::rule`.

use crate::session::{
    engine, leak_str, load_module, normalize_limits, GuestError, Session, WasmLimits,
};
use omni_core::plugin::{Capability, CapabilityScope, ParsedFile, Plugin, WorkspaceConfig};
use omni_core::{Diagnostic, Severity, SourceFile, Span};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::any::Any;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Language module metadata (`omni_plugin_meta`):
/// `{ "id": "demo-gql", "name": "...", "extensions": ["graphql"],
///    "capabilities": [{"name": "graphql.facts", "scope": "file"},
///                     {"name": "graphql.workspace", "scope": "workspace"}] }`
#[derive(Debug, Serialize, Deserialize)]
pub struct PluginMeta {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub extensions: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<MetaCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaCapability {
    pub name: String,
    pub scope: String, // "file" | "workspace"
}

/// Parse output: a facts document (the vocabulary in `omni_core`'s docs and
/// `omni-graphql::facts`) plus an `errors` array the host peels off into
/// parse diagnostics.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Facts {
    /// Parse errors, surfaced as `<plugin id>/parse-error` diagnostics.
    #[serde(default)]
    pub errors: Vec<GuestError>,
    /// Everything else is opaque facts (`defs`, `uses`, `roots`, ...).
    #[serde(flatten)]
    pub view: Map<String, Value>,
}

/// A validated, loaded language AST module.
pub struct WasmModule {
    engine: wasmtime::Engine,
    module: wasmtime::Module,
    pub meta: PluginMeta,
    limits: WasmLimits,
    pub module_path: PathBuf,
    has_workspace_export: bool,
}

impl WasmModule {
    pub fn load(path: &Path, raw_limits: WasmLimits) -> Result<WasmModule, String> {
        let limits = normalize_limits(raw_limits);
        let engine = engine()?;
        let module = load_module(
            &engine,
            path,
            &["memory", "omni_alloc", "omni_plugin_meta", "omni_plugin_parse"],
        )?;
        let has_workspace_export = module
            .exports()
            .any(|e| e.name() == "omni_plugin_workspace");

        // Probe metadata through a real instantiation so load-time validation
        // actually runs guest code once.
        let meta = {
            let mut session = Session::open(&engine, &module, &limits)
                .map_err(|e| format!("instantiate {}: {e}", path.display()))?;
            let json = session
                .call_ret_json("omni_plugin_meta", &[])
                .map_err(|e| format!("probe {}: {e}", path.display()))?;
            let meta: PluginMeta = serde_json::from_slice(&json)
                .map_err(|e| format!("language module `{}` meta malformed: {e}", path.display()))?;
            if meta.id.is_empty() || !meta.id.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                return Err(format!(
                    "language module `{}` declares an invalid id {:?} (use lowercase kebab)",
                    path.display(),
                    meta.id
                ));
            }
            if !meta
                .capabilities
                .iter()
                .any(|c| c.scope == "file" && c.name.ends_with(".facts"))
            {
                return Err(format!(
                    "language module `{}` must declare a file capability named `<language>.facts`",
                    path.display()
                ));
            }
            meta
        };

        Ok(WasmModule {
            engine,
            module,
            meta,
            limits,
            module_path: path.to_path_buf(),
            has_workspace_export,
        })
    }

    /// Raw parse call: facts JSON text + guest-reported findings/errors.
    /// Exposed for debugging.
    pub fn call_parse(
        &self,
        path: &str,
        src: &str,
    ) -> Result<(String, Vec<GuestError>), String> {
        let mut session = Session::open(&self.engine, &self.module, &self.limits)?;
        let (path_ptr, path_len) = session.write_bytes(path.as_bytes())?;
        let (src_ptr, src_len) = session.write_bytes(src.as_bytes())?;
        let ret = session.call_ret_json(
            "omni_plugin_parse",
            &[(path_ptr, path_len), (src_ptr, src_len)],
        )?;
        let json = String::from_utf8(ret).map_err(|_| String::from("malformed UTF-8 facts"))?;
        Ok((json, session.reported_errors().to_vec()))
    }

    /// Parse one source into a facts `Value` + parse errors.
    fn parse_source(&self, source: &SourceFile) -> Result<(Value, Vec<GuestError>), String> {
        let path_arg = source.path.display().to_string();
        let (raw, streamed) = self.call_parse(&path_arg, source.text())?;
        let facts: Facts =
            serde_json::from_str(&raw).map_err(|e| format!("malformed facts: {e}"))?;
        let mut errors = facts.errors;
        errors.extend(streamed);
        errors.truncate(50);
        Ok((Value::Object(facts.view), errors))
    }

    /// Raw workspace call over the per-file facts envelope. Debug aid.
    pub fn call_workspace_dbg(&self, envelope: &str) -> Result<String, String> {
        let mut session = Session::open(&self.engine, &self.module, &self.limits)?;
        let (ptr, len) = session.write_bytes(envelope.as_bytes())?;
        let ret = session.call_ret_json("omni_plugin_workspace", &[(ptr, len)])?;
        String::from_utf8(ret).map_err(|_| String::from("malformed UTF-8 workspace facts"))
    }

    fn has_workspace_export(&self) -> bool {
        self.has_workspace_export
    }

    /// Workspace facts for the run: guest-produced when it exports
    /// `omni_plugin_workspace`, else the host-synthesized merge of the
    /// per-file facts (same vocabulary, `path` on every `def`/`use`).
    fn workspace_facts(&self, per_file: &[(String, Value)]) -> Result<Value, String> {
        if self.has_workspace_export() {
            let envelope = json!({
                "files": per_file
                    .iter()
                    .map(|(path, facts)| json!({ "path": path, "facts": facts }))
                    .collect::<Vec<_>>(),
            })
            .to_string();
            let mut session = Session::open(&self.engine, &self.module, &self.limits)?;
            let (ptr, len) = session.write_bytes(envelope.as_bytes())?;
            let ret = session.call_ret_json("omni_plugin_workspace", &[(ptr, len)])?;
            let value: Value = serde_json::from_slice(&ret)
                .map_err(|e| format!("malformed workspace facts: {e}"))?;
            Ok(value)
        } else {
            Ok(merge_facts(per_file))
        }
    }
}

/// Host-side merge of per-file facts into the cross-file view: the same
/// vocabulary with a `path` on every `def`/`use`. This is what workspace
/// rules see when the language module computes no model of its own.
pub fn merge_facts(per_file: &[(String, Value)]) -> Value {
    let mut defs = Vec::new();
    let mut uses = Vec::new();
    let mut directives_defined = Vec::new();
    let mut directives_used = Vec::new();
    let mut has_entry_block = false;
    let mut roots = Vec::new();

    for (path, facts) in per_file {
        for key in ["defs", "uses"] {
            let Some(items) = facts.get(key).and_then(|v| v.as_array()) else {
                continue;
            };
            for item in items {
                let mut obj = item.clone();
                if let Some(o) = obj.as_object_mut() {
                    o.insert("path".into(), json!(path));
                }
                if key == "defs" {
                    defs.push(obj);
                } else {
                    uses.push(obj);
                }
            }
        }
        for key in ["directives_defined", "directives_used"] {
            let Some(items) = facts.get(key).and_then(|v| v.as_array()) else {
                continue;
            };
            let sink = if key == "directives_defined" {
                &mut directives_defined
            } else {
                &mut directives_used
            };
            for d in items {
                if !sink.contains(d) {
                    sink.push(d.clone());
                }
            }
        }
        has_entry_block |= facts
            .get("has_entry_block")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(r) = facts.get("roots").and_then(|v| v.as_array()) {
            roots.extend(r.iter().cloned());
        }
    }

    json!({
        "defs": defs,
        "uses": uses,
        "directives_defined": directives_defined,
        "directives_used": directives_used,
        "has_entry_block": has_entry_block,
        "roots": roots,
    })
}

// ---------------------------------------------------------------------------
// Engine Plugin adapter
// ---------------------------------------------------------------------------

/// A WASM language module exposed through the engine's `Plugin` trait.
/// Validation happens at load; a broken module surfaces as a load error and
/// is skipped — it never crashes the engine.
pub struct WasmPlugin {
    module: WasmModule,
    path: PathBuf,
}

impl WasmPlugin {
    /// Load and validate up front.
    pub fn load(path: &Path, limits: WasmLimits) -> Result<WasmPlugin, String> {
        WasmModule::load(path, limits).map(|module| WasmPlugin {
            module,
            path: path.to_path_buf(),
        })
    }

    pub fn module_path(&self) -> &Path {
        &self.path
    }

    pub fn meta(&self) -> &PluginMeta {
        &self.module.meta
    }

    /// Debug access to the underlying module.
    pub fn module(&self) -> &WasmModule {
        &self.module
    }

    /// Discover all `.wasm` language modules under `dir`. Returns valid
    /// plugins plus (path, error) for modules that failed validation
    /// (reported and skipped by the caller, never fatal).
    pub fn discover(
        dir: &Path,
        limits: WasmLimits,
    ) -> (Vec<WasmPlugin>, Vec<(PathBuf, String)>) {
        let mut plugins = Vec::new();
        let mut errors = Vec::new();
        let mut paths: Vec<PathBuf> = super::wasm_paths(dir);
        paths.sort();
        for path in paths {
            match WasmPlugin::load(&path, limits.clone()) {
                Ok(p) => plugins.push(p),
                Err(e) => errors.push((path, e)),
            }
        }
        (plugins, errors)
    }
}

impl Plugin for WasmPlugin {
    fn id(&self) -> &'static str {
        leak_str(&self.module.meta.id)
    }

    fn describe(&self) -> &'static str {
        leak_str(&format!(
            "{} (sandboxed WASM language module, fuel-metered)",
            self.module
                .meta
                .name
                .clone()
                .unwrap_or_else(|| self.module.meta.id.clone())
        ))
    }

    fn extensions(&self) -> &'static [&'static str] {
        let exts: Vec<&'static str> = self
            .module
            .meta
            .extensions
            .iter()
            .map(|e| leak_str(e.trim_start_matches('.').to_ascii_lowercase().as_str()))
            .collect();
        Box::leak(exts.into_boxed_slice())
    }

    fn capabilities(&self) -> Vec<Capability> {
        self.module
            .meta
            .capabilities
            .iter()
            .map(|c| Capability {
                name: leak_str(&c.name),
                scope: match c.scope.as_str() {
                    "workspace" => CapabilityScope::Workspace,
                    _ => CapabilityScope::File,
                },
            })
            .collect()
    }

    fn parse_file(&self, source: Arc<SourceFile>) -> ParsedFile {
        let mut artifacts: BTreeMap<String, Arc<dyn Any + Send + Sync>> = BTreeMap::new();
        let mut errors = Vec::new();
        let module = &self.module;
        let meta = &module.meta;
        match module.parse_source(&source) {
                    Ok((facts, guest_errors)) => {
                        // Publish the facts JSON under every declared
                        // file-scope `*.facts` capability.
                        for cap in &meta.capabilities {
                            if cap.scope == "file" && cap.name.ends_with(".facts") {
                                artifacts.insert(
                                    cap.name.clone(),
                                    Arc::new(facts.clone()) as Arc<dyn Any + Send + Sync>,
                                );
                            }
                        }
                        for e in &guest_errors {
                            errors.push(Diagnostic {
                                rule_id: format!("{}/parse-error", module.meta.id),
                                severity: Severity::Warning,
                                message: if e.message.is_empty() {
                                    "parse error".into()
                                } else {
                                    e.message.clone()
                                },
                                source_id: source.id,
                                span: crate::session::span_of(e.span),
                                related: vec![],
                                subject: None,
                            });
                        }
                    }
                    Err(e) => {
                        errors.push(Diagnostic {
                            rule_id: format!("{}/error", module.meta.id),
                            severity: Severity::Error,
                            message: format!(
                                "wasm language module failed on {}: {e}",
                                source.path.display()
                            ),
                            source_id: source.id,
                            span: Span::new(0, 0),
                            related: vec![],
                            subject: None,
                        });
                    }
                }
        ParsedFile {
            source,
            artifacts,
            findings: Vec::new(),
            errors,
        }
    }

    fn build_workspace(
        &self,
        files: &[ParsedFile],
        _config: &WorkspaceConfig,
    ) -> Result<BTreeMap<String, Arc<dyn Any + Send + Sync>>, String> {
        let module = &self.module;
        // Which key holds per-file facts (first declared `*.facts` cap).
        let Some(facts_cap) = module
            .meta
            .capabilities
            .iter()
            .find(|c| c.scope == "file" && c.name.ends_with(".facts"))
            .map(|c| c.name.clone())
        else {
            return Ok(BTreeMap::new());
        };
        let ws_caps: Vec<String> = module
            .meta
            .capabilities
            .iter()
            .filter(|c| c.scope == "workspace" && c.name.ends_with(".workspace"))
            .map(|c| c.name.clone())
            .collect();
        if ws_caps.is_empty() {
            return Ok(BTreeMap::new());
        }

        let per_file: Vec<(String, Value)> = files
            .iter()
            .filter_map(|f| {
                f.artifact::<Value>(&facts_cap)
                    .map(|facts| (f.source.path.display().to_string(), facts.clone()))
            })
            .collect();
        let workspace = module.workspace_facts(&per_file)?;
        let mut out: BTreeMap<String, Arc<dyn Any + Send + Sync>> = BTreeMap::new();
        for cap in ws_caps {
            out.insert(cap, Arc::new(workspace.clone()) as Arc<dyn Any + Send + Sync>);
        }
        Ok(out)
    }
}