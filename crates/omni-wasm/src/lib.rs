//! Sandboxed WASM plugin loading for the engine.
//!
//! Contract (classic `wasm32-wasip1` modules):
//!
//! Guest exports:
//!   - `memory`
//!   - `omni_alloc(len: i32) -> i32` — bump-allocate a buffer in guest
//!     memory, returning its pointer (the host then copies bytes in)
//!   - `omni_plugin_meta() -> ptr` — UTF-8 JSON `PluginMeta`
//!   - `omni_plugin_parse(path_ptr, path_len, src_ptr, src_len) -> ptr`
//!       UTF-8 JSON `Facts` for one document
//!   - `omni_plugin_run_workspace(facts_ptr, facts_len) -> ptr`
//!       UTF-8 JSON array of `GuestFinding`
//!
//! Host imports the guest may call while running:
//!   - `env.omni_report(ptr, len)` — one `GuestFinding` JSON per call
//!   - `env.omni_parse_error(ptr, len)` — one `GuestError` JSON per call
//!
//! Every string-returning export returns a pointer to a little-endian
//! `u32` length followed by that many bytes inside guest memory.
//!
//! Safety: guest code runs with per-call **fuel metering** (bounded
//! execution), gets no filesystem or network access (the linker exports no
//! OS WASI functions at all), and every host-observed span is clamped into
//! sane maxima so a misbehaving module cannot corrupt host indices.

use omni_core::plugin::{Capability, CapabilityScope, ParsedFile, Plugin};
use omni_core::{Diagnostic, PartialDiagnostic, PathDiagnostic, Severity, SourceFile, Span};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::{anyhow, Error};
use wasmtime::{Config, Engine, Extern, Instance, Linker, Module, Store, Val};

/// Fuel ticks granted per plugin call.
pub const DEFAULT_FUEL: u64 = 5_000_000_000;
/// Hard ceiling on findings a plugin may report per call.
pub const MAX_REPORTED_FINDINGS: usize = 100_000;
/// Maximum bytes a plugin result may claim.
pub const MAX_RESULT_BYTES: usize = 64 * 1024 * 1024;
/// Span addresses are clamped to this; keeps host arithmetic within bounds.
const MAX_SPAN_OFFSET: u32 = 0x4000_0000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WasmLimits {
    /// Fuel ticks per call. `None` disables metering (not recommended for
    /// third-party plugins; the default policy is `Some(DEFAULT_FUEL)`).
    pub fuel: Option<u64>,
}

// ---------------------------------------------------------------------------
// JSON halves of the contract
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginMeta {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub extensions: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<MetaCapability>,
    /// Rules the plugin implements, used for `--list-rules` and config
    /// severity overrides. Ids are namespaced by the plugin author, e.g.
    /// `sql/no-drop-without-where`.
    #[serde(default)]
    pub rules: Vec<MetaRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaCapability {
    pub name: String,
    pub scope: String, // "file" | "workspace"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaRule {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub severity: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GuestError {
    pub message: String,
    #[serde(default)]
    pub span: [u32; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestFinding {
    pub rule_id: String,
    #[serde(default = "default_severity")]
    pub severity: String,
    pub message: String,
    #[serde(default)]
    pub span: [u32; 2],
    #[serde(default)]
    pub subject: Option<String>,
    /// Relative path the finding belongs to. Workspace findings must set it.
    #[serde(default)]
    pub path: Option<String>,
}

fn default_severity() -> String {
    "warning".into()
}

/// Facts a WASM plugin emits per document, delivered host-side so the plugin
/// can run workspace rules later with the full cross-file view.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Facts {
    #[serde(default)]
    pub errors: Vec<GuestError>,
    /// Top-level declarations with plugin-defined kinds and subjects.
    #[serde(default)]
    pub defs: Vec<DefFact>,
    /// Type-like references: name + span.
    #[serde(default)]
    pub uses: Vec<UseFact>,
    #[serde(default)]
    pub directives_defined: Vec<String>,
    #[serde(default)]
    pub directives_used: Vec<String>,
    /// True when the doc declares an entry-point block (like GraphQL's `schema`).
    #[serde(default)]
    pub has_entry_block: bool,
    /// Entry-point map (name -> target), e.g. ("query", "QueryRoot").
    #[serde(default)]
    pub roots: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefFact {
    pub kind: String,
    pub name: String,
    pub span: [u32; 2],
    pub subject: String,
    #[serde(default)]
    pub fields: Vec<FieldFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldFact {
    pub name: String,
    pub span: [u32; 2],
    #[serde(default)]
    pub type_name: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub directives: Vec<String>,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UseFact {
    pub name: String,
    pub span: [u32; 2],
}

// ---------------------------------------------------------------------------
// Host functions exposed to the guest
// ---------------------------------------------------------------------------

struct GuestState {
    wasi: wasmtime_wasi::preview1::WasiP1Ctx,
    findings: Vec<GuestFinding>,
    errors: Vec<GuestError>,
}

fn get_memory<T>(caller: &mut wasmtime::Caller<'_, T>) -> Result<wasmtime::Memory, Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| anyhow!("plugin has no exported `memory`"))
}

fn read_slice<T: 'static>(
    caller: &mut wasmtime::Caller<'_, T>,
    ptr: i32,
    len: i32,
) -> Result<Vec<u8>, Error> {
    if ptr < 0 || len < 0 {
        return Err(anyhow!("negative buffer"));
    }
    let memory = get_memory(caller)?;
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| anyhow!("buffer overflow"))?;
    let size = memory.data_size(&mut *caller);
    if end > size {
        return Err(anyhow!(format!(
            "buffer {start}..{end} exceeds guest memory {size}"
        )));
    }
    Ok(memory.data(&mut *caller)[start..end].to_vec())
}

fn read_json<T: 'static, J: for<'de> Deserialize<'de>>(
    caller: &mut wasmtime::Caller<'_, T>,
    ptr: i32,
    len: i32,
    what: &str,
) -> Result<J, Error> {
    let bytes = read_slice(caller, ptr, len)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| anyhow!(format!("{what}: invalid UTF-8")))?;
    serde_json::from_str(&text)
        .map_err(|e| anyhow!(format!("{what}: malformed JSON: {e}")))
}

/// Clamp guest-reported spans into engine-visible maxima.
fn clamp_span(sp: [u32; 2]) -> [u32; 2] {
    let s = sp[0].min(MAX_SPAN_OFFSET);
    let e = sp[1].min(MAX_SPAN_OFFSET).max(s);
    [s, e]
}

fn define_host_fns(linker: &mut Linker<GuestState>) -> wasmtime::Result<()> {
    linker.func_wrap(
        "env",
        "omni_report",
        |mut caller: wasmtime::Caller<'_, GuestState>, ptr: i32, len: i32| -> Result<(), Error> {
            let mut finding: GuestFinding = read_json(&mut caller, ptr, len, "omni_report")?;
            finding.span = clamp_span(finding.span);
            let state = caller.data_mut();
            if state.findings.len() >= MAX_REPORTED_FINDINGS {
                return Err(anyhow!("guest reported findings over limit"));
            }
            state.findings.push(finding);
            Ok(())
        },
    )?;
    linker.func_wrap(
        "env",
        "omni_parse_error",
        |mut caller: wasmtime::Caller<'_, GuestState>, ptr: i32, len: i32| -> Result<(), Error> {
            let mut err: GuestError = read_json(&mut caller, ptr, len, "omni_parse_error")?;
            err.span = clamp_span(err.span);
            caller.data_mut().errors.push(err);
            Ok(())
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Session: one instantiated module instance per plugin call
// ---------------------------------------------------------------------------

struct Session {
    store: Store<GuestState>,
    instance: Instance,
    memory: wasmtime::Memory,
}

impl Session {
    fn open(
        engine: &Engine,
        module: &Module,
        limits: &WasmLimits,
    ) -> Result<Session, String> {
        let mut linker: Linker<GuestState> = Linker::new(engine);
        define_host_fns(&mut linker).map_err(|e| format!("define host fns: {e}"))?;
        // Sandbox: WASI is linked but with no filesystem preopens, no argv/env,
        // no sockets — file/network calls simply fail closed inside the guest.
        wasmtime_wasi::preview1::add_to_linker_sync(&mut linker, |state: &mut GuestState| &mut state.wasi)
            .map_err(|e| format!("link wasi: {e}"))?;
        let mut store: Store<GuestState> = Store::new(
            engine,
            GuestState {
                wasi: wasmtime_wasi::p2::WasiCtxBuilder::new().build_p1(),
                findings: Vec::new(),
                errors: Vec::new(),
            },
        );
        if let Some(fuel) = limits.fuel {
            store
                .set_fuel(fuel)
                .map_err(|e| format!("set fuel: {e}"))?;
        }
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| format!("instantiate plugin module: {e}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or("plugin must export memory")?;
        Ok(Session {
            store,
            instance,
            memory,
        })
    }

    /// Call the guest allocator and copy bytes into guest memory.
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(i32, i32), String> {
        if bytes.len() > MAX_RESULT_BYTES {
            return Err("input too large for plugin contract".into());
        }
        let alloc = self
            .instance
            .get_typed_func::<i32, i32>(&mut self.store, "omni_alloc")
            .map_err(|e| format!("missing omni_alloc: {e}"))?;
        let ptr = alloc
            .call(&mut self.store, bytes.len() as i32)
            .map_err(|e| format!("omni_alloc: {e}"))?;
        if ptr < 0 {
            return Err("omni_alloc returned a negative pointer".into());
        }
        self.memory
            .write(&mut self.store, ptr as usize, bytes)
            .map_err(|e| format!("copy into guest memory: {e}"))?;
        Ok((ptr, bytes.len() as i32))
    }

    /// Call `name` with alternating (ptr, len) i32 pairs; decode the
    /// length-prefixed buffer the guest returns as `[u32 LE len][bytes]`.
    fn call_ret_json(&mut self, name: &str, pairs: &[(i32, i32)]) -> Result<Vec<u8>, String> {
        let f = self
            .instance
            .get_func(&mut self.store, name)
            .ok_or_else(|| format!("plugin is missing export `{name}`"))?;
        let mut args: Vec<Val> = Vec::with_capacity(pairs.len() * 2);
        for (p, l) in pairs {
            args.push(Val::I32(*p));
            args.push(Val::I32(*l));
        }
        let mut results = vec![Val::I32(0)];
        f.call(&mut self.store, &args, &mut results)
            .map_err(|e| format!("call {name}: {e}"))?;
        let ret_ptr = results
            .first()
            .and_then(|v| v.i32())
            .ok_or_else(|| format!("{name} did not return a pointer"))?;
        if ret_ptr < 0 {
            return Err(format!("{name} returned a negative pointer"));
        }
        let mut head = [0u8; 4];
        self.memory
            .read(&self.store, ret_ptr as usize, &mut head)
            .map_err(|e| format!("read result header: {e}"))?;
        let len = u32::from_le_bytes(head) as usize;
        if len > MAX_RESULT_BYTES {
            return Err(format!(
                "{name} returned an oversized buffer ({len} bytes)"
            ));
        }
        let mut buf = vec![0u8; len];
        if len > 0 {
            self.memory
                .read(&self.store, ret_ptr as usize + 4, &mut buf)
                .map_err(|e| format!("read result body: {e}"))?;
        }
        Ok(buf)
    }
}

// ---------------------------------------------------------------------------
// Loaded module
// ---------------------------------------------------------------------------

fn span_of(sp: [u32; 2]) -> Span {
    Span::new(sp[0], sp[1].max(sp[0]))
}

/// A validated, loaded WASM plugin module.
pub struct WasmModule {
    engine: Engine,
    module: Module,
    pub meta: PluginMeta,
    limits: WasmLimits,
    pub module_path: PathBuf,
}

impl WasmModule {
    pub fn load(path: &Path, raw_limits: WasmLimits) -> Result<WasmModule, String> {
        // Default policy: meter fuel unless the embedder opted out explicitly.
        let limits = WasmLimits {
            fuel: Some(raw_limits.fuel.unwrap_or(DEFAULT_FUEL)),
        };
        let engine = {
            let mut cfg = Config::new();
            cfg.consume_fuel(true);
            Engine::new(&cfg).map_err(|e| format!("wasmtime engine: {e}"))?
        };
        let bytes = std::fs::read(path)
            .map_err(|e| format!("cannot read plugin module {}: {e}", path.display()))?;
        let module = Module::from_binary(&engine, &bytes)
            .map_err(|e| format!("invalid wasm module {}: {e}", path.display()))?;

        let mut exports = std::collections::BTreeSet::new();
        for exp in module.exports() {
            exports.insert(exp.name().to_string());
        }
        for required in [
            "memory",
            "omni_alloc",
            "omni_plugin_meta",
            "omni_plugin_parse",
            "omni_plugin_run_workspace",
        ] {
            if !exports.contains(required) {
                return Err(format!(
                    "plugin `{}` is missing required export `{required}`",
                    path.display()
                ));
            }
        }

        // Probe metadata through a real instantiation so load-time validation
        // actually runs guest code once.
        let meta = {
            let mut session = Session::open(&engine, &module, &limits)
                .map_err(|e| format!("instantiate {}: {e}", path.display()))?;
            let json = session
                .call_ret_json("omni_plugin_meta", &[])
                .map_err(|e| format!("probe {}: {e}", path.display()))?;
            let meta: PluginMeta = serde_json::from_slice(&json)
                .map_err(|e| format!("plugin `{}` meta malformed: {e}", path.display()))?;
            if meta.id.is_empty() || !meta.id.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                return Err(format!(
                    "plugin `{}` declares an invalid id {:?} (use lowercase kebab)",
                    path.display(),
                    meta.id
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
        })
    }

    /// Run the plugin's file-scoped pass over a (path, source) pair; returns
    /// the raw JSON result the guest produced plus guest-reported findings.
    /// Exposed for debugging.
    pub fn call_parse(&self, path: &str, src: &str) -> Result<(String, Vec<GuestFinding>, Vec<GuestError>), String> {
        let mut session = Session::open(&self.engine, &self.module, &self.limits)?;
        let (path_ptr, path_len) = session.write_bytes(path.as_bytes())?;
        let (src_ptr, src_len) = session.write_bytes(src.as_bytes())?;
        let ret = session.call_ret_json(
            "omni_plugin_parse",
            &[(path_ptr, path_len), (src_ptr, src_len)],
        )?;
        let guest = session.store.data();
        let json = String::from_utf8(ret)
            .map_err(|_| String::from("malformed UTF-8 facts"))?;
        Ok((json, guest.findings.iter().cloned().collect(), guest.errors.iter().cloned().collect()))
    }

    /// Run the plugin's file-scoped pass over one source document.
    fn parse_source(
        &self,
        source: &SourceFile,
    ) -> Result<(Facts, Vec<Diagnostic>, Vec<Diagnostic>), String> {
        let path_arg = source.path.display().to_string();
        let (raw, guest_findings, guest_errors) = self.call_parse(&path_arg, source.text())?;
        let facts: Facts =
            serde_json::from_str(&raw).map_err(|e| format!("malformed facts: {e}"))?;

        let findings: Vec<Diagnostic> = guest_findings
            .iter()
            .map(|f| Diagnostic {
                rule_id: f.rule_id.clone(),
                severity: Severity::parse(&f.severity).unwrap_or(Severity::Warning),
                message: f.message.clone(),
                source_id: source.id,
                span: span_of(f.span),
                related: vec![],
                subject: f.subject.clone(),
            })
            .collect();
        let mut errors: Vec<Diagnostic> = facts
            .errors
            .iter()
            .chain(guest_errors.iter())
            .map(|e| Diagnostic {
                rule_id: format!("{}/parse-error", self.meta.id),
                severity: Severity::Warning,
                message: if e.message.is_empty() {
                    "parse error".into()
                } else {
                    e.message.clone()
                },
                source_id: source.id,
                span: span_of(e.span),
                related: vec![],
                subject: None,
            })
            .collect();
        errors.truncate(50);
        Ok((facts, findings, errors))
    }

    /// Workspace pass with raw JSON I/O, exposed for debugging.
    pub fn call_workspace_dbg(&self, envelope: &str) -> Result<String, String> {
        let mut session = Session::open(&self.engine, &self.module, &self.limits)?;
        let (ptr, len) = session.write_bytes(envelope.as_bytes())?;
        let ret = session.call_ret_json("omni_plugin_run_workspace", &[(ptr, len)])?;
        String::from_utf8(ret).map_err(|_| String::from("malformed UTF-8 workspace findings"))
    }

    /// Run the plugin's workspace pass; returns diagnostics whose file is a
    /// relative path the runner resolves. Findings come both from the returned
    /// array and from `omni_report` calls made inside the guest.
    fn run_workspace(&self, facts_json: &str) -> Result<Vec<PathDiagnostic>, String> {
        let mut session = Session::open(&self.engine, &self.module, &self.limits)?;
        let (ptr, len) = session.write_bytes(facts_json.as_bytes())?;
        let _ret = session.call_ret_json("omni_plugin_run_workspace", &[(ptr, len)])?;
        let mut findings: Vec<GuestFinding> = session.store.data().findings.clone();
        // Findings the guest returned inline (in addition to omni_report).
        if let Ok(inline) = serde_json::from_slice::<Vec<GuestFinding>>(&_ret) {
            findings.extend(inline);
        }
        Ok(findings
            .into_iter()
            .map(|f| PathDiagnostic {
                path: f.path.clone().unwrap_or_default(),
                diag: PartialDiagnostic {
                    rule_id: f.rule_id.clone(),
                    severity: Severity::parse(&f.severity).unwrap_or(Severity::Warning),
                    message: f.message.clone(),
                    span: span_of(f.span),
                    subject: f.subject.clone(),
                    related: Vec::new(),
                },
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Engine Plugin adapter
// ---------------------------------------------------------------------------

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// A WASM plugin exposed through the engine's `Plugin` trait. Handles
/// deferred loading so one broken third-party module never crashes the
/// engine: it surfaces as a config diagnostic instead.
pub struct WasmPlugin {
    module: Option<WasmModule>,
    path: PathBuf,
    load_error: Option<String>,
}

impl WasmPlugin {
    /// Load and validate up front.
    pub fn load(path: &Path, limits: WasmLimits) -> Result<WasmPlugin, String> {
        WasmModule::load(path, limits).map(|module| WasmPlugin {
            module: Some(module),
            path: path.to_path_buf(),
            load_error: None,
        })
    }

    /// Deferred variant: a bad module becomes a config error at run time
    /// instead of failing plugin-dir load.
    pub fn load_deferred(path: &Path, limits: WasmLimits) -> WasmPlugin {
        match WasmModule::load(path, limits) {
            Ok(module) => WasmPlugin {
                module: Some(module),
                path: path.to_path_buf(),
                load_error: None,
            },
            Err(e) => WasmPlugin {
                module: None,
                path: path.to_path_buf(),
                load_error: Some(e),
            },
        }
    }

    pub fn module_path(&self) -> &Path {
        &self.path
    }

    pub fn meta(&self) -> Option<&PluginMeta> {
        self.module.as_ref().map(|m| &m.meta)
    }

    /// Debug access to the underlying module.
    pub fn module(&self) -> Option<&WasmModule> {
        self.module.as_ref()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    /// Discover all `.wasm` plugins under `dir` (deferred loading: bad modules
    /// carry their error). Returns (plugins, deferred_errors).
    pub fn discover(
        dir: &Path,
        limits: WasmLimits,
    ) -> (Vec<WasmPlugin>, Vec<(PathBuf, String)>) {
        let mut plugins = Vec::new();
        let mut errors = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return (plugins, errors);
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("wasm"))
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();
        for path in paths {
            match WasmPlugin::load_deferred(&path, limits.clone()) {
                p if p.load_error.is_none() => plugins.push(p),
                p if p.load_error.is_some() => {
                    let err = p.load_error.clone().unwrap_or_default();
                    errors.push((path, err));
                    plugins.push(p);
                }
                _ => unreachable!(),
            }
        }
        (plugins, errors)
    }
}

impl Plugin for WasmPlugin {
    fn id(&self) -> &'static str {
        match self.module.as_ref() {
            Some(m) => leak_str(&m.meta.id),
            None => leak_str("broken-wasm-plugin"),
        }
    }

    fn describe(&self) -> &'static str {
        match self.module.as_ref() {
            Some(m) => leak_str(&format!(
                "{} (sandboxed WASM module, fuel-metered)",
                m.meta.name.clone().unwrap_or_else(|| m.meta.id.clone())
            )),
            None => leak_str(
                self.load_error
                    .as_deref()
                    .unwrap_or("(broken wasm plugin)"),
            ),
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        match self.module.as_ref() {
            Some(m) => {
                let exts: Vec<&'static str> = m
                    .meta
                    .extensions
                    .iter()
                    .map(|e| leak_str(e.trim_start_matches('.').to_ascii_lowercase().as_str()))
                    .collect();
                Box::leak(exts.into_boxed_slice())
            }
            None => Box::leak(Vec::new().into_boxed_slice()),
        }
    }

    fn capabilities(&self) -> Vec<Capability> {
        match self.module.as_ref() {
            Some(m) => m
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
                .collect(),
            None => Vec::new(),
        }
    }

    fn parse_file(&self, source: Arc<SourceFile>) -> ParsedFile {
        let mut artifacts = std::collections::BTreeMap::new();
        let mut findings = Vec::new();
        let mut errors = Vec::new();
        match self.module.as_ref() {
            Some(module) => match module.parse_source(&source) {
                Ok((facts, f, e)) => {
                    artifacts.insert(
                        format!("{}/facts", module.meta.id),
                        Arc::new((source.path.display().to_string(), facts))
                            as Arc<dyn std::any::Any + Send + Sync>,
                    );
                    findings = f;
                    errors = e;
                }
                Err(e) => {
                    errors.push(Diagnostic {
                        rule_id: format!("{}/error", module.meta.id),
                        severity: Severity::Error,
                        message: format!(
                            "wasm plugin failed on {}: {e}",
                            source.path.display()
                        ),
                        source_id: source.id,
                        span: Span::new(0, 0),
                        related: vec![],
                        subject: None,
                    });
                }
            },
            None => {}
        }
        ParsedFile {
            source,
            artifacts,
            findings,
            errors,
        }
    }

    fn run_workspace_rules(&self, files: &[ParsedFile]) -> Vec<PathDiagnostic> {
        let Some(module) = self.module.as_ref() else {
            return vec![];
        };
        let key = format!("{}/facts", module.meta.id);
        let mut per_file = Vec::new();
        for f in files {
            if let Some(a) = f.artifacts.get(&key) {
                if let Some((path, facts)) = a.downcast_ref::<(String, Facts)>() {
                    per_file.push(serde_json::json!({
                        "path": path,
                        "facts": facts,
                    }));
                }
            }
        }
        let envelope = serde_json::json!({
            "files": per_file,
            "rules": module.meta.rules.iter()
                .map(|r| serde_json::json!({ "id": r.id }))
                .collect::<Vec<_>>(),
        });
        let json = serde_json::json!(envelope).to_string();
        match module.run_workspace(&json) {
            Ok(diags) => diags,
            Err(e) => vec![PathDiagnostic {
                path: String::new(),
                diag: PartialDiagnostic {
                    rule_id: format!("{}/error", module.meta.id),
                    severity: Severity::Error,
                    message: format!(
                        "wasm plugin workspace pass failed: {e} (module: {})",
                        module.module_path.display()
                    ),
                    span: Span::new(0, 0),
                    subject: None,
                    related: Vec::new(),
                },
            }],
        }
    }
}

// ---------------------------------------------------------------------------
// Registry integration: pseudo-rules carrying wasm plugin rule metadata
// ---------------------------------------------------------------------------

/// Placeholder `Rule` for a WASM plugin's declared rule, so config overrides
/// (severity/enabled) and `--list-rules` behave uniformly. Actual findings
/// arrive through `ParsedFile::findings` / plugin workspace reports.
pub struct MetaOnlyRule {
    meta: omni_core::plugin::RuleMeta,
}

impl MetaOnlyRule {
    pub fn new(meta: omni_core::plugin::RuleMeta) -> Self {
        MetaOnlyRule { meta }
    }
}

impl omni_core::Rule for MetaOnlyRule {
    fn meta(&self) -> omni_core::plugin::RuleMeta {
        self.meta.clone()
    }
    fn run(&self, _ctx: &omni_core::plugin::RuleContext) {
        // findings for wasm rules arrive via the plugin, not this stub
        let _ = self;
    }
}
