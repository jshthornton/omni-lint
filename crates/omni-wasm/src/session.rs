//! Shared wasmtime plumbing for language modules and rule modules.
//!
//! Classic `wasm32-wasip1` guest conventions used by both module kinds:
//!
//!   - `memory`
//!   - `omni_alloc(len: i32) -> i32` — bump-allocate a buffer in guest
//!     memory, returning its pointer (the host then copies bytes in)
//!
//! Every string-returning export returns a pointer to a little-endian
//! `u32` length followed by that many bytes inside guest memory.
//!
//! Host imports the guest may call while running:
//!   - `env.omni_report(ptr, len)` — one `GuestFinding` JSON per call
//!   - `env.omni_parse_error(ptr, len)` — one `GuestError` JSON per call
//!
//! Safety: guest code runs with per-call **fuel metering** (bounded
//! execution), gets no filesystem or network access (the linker exports no
//! OS WASI functions at all), and every host-observed span is clamped into
//! sane maxima so a misbehaving module cannot corrupt host indices.

use anyhow::{anyhow, Error};
use serde::{Deserialize, Serialize};
use wasmtime::{Config, Engine, Extern, Instance, Linker, Module, Store, Val};

/// Fuel ticks granted per plugin call.
pub const DEFAULT_FUEL: u64 = 5_000_000_000;
/// Hard ceiling on findings a guest may report per call.
pub const MAX_REPORTED_FINDINGS: usize = 100_000;
/// Maximum bytes a guest result may claim.
pub const MAX_RESULT_BYTES: usize = 64 * 1024 * 1024;
/// Span addresses are clamped to this; keeps host arithmetic within bounds.
pub(crate) const MAX_SPAN_OFFSET: u32 = 0x4000_0000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WasmLimits {
    /// Fuel ticks per call. `None` disables metering (not recommended for
    /// third-party modules; the default policy is `Some(DEFAULT_FUEL)`).
    pub fuel: Option<u64>,
}

/// One finding as reported by any guest (rule modules; kept tolerant: every
/// field but `message` is optional and defaulted host-side).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestFinding {
    /// Defaults to the rule's own id; a foreign id is discarded host-side.
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    pub message: String,
    #[serde(default)]
    pub span: [u32; 2],
    #[serde(default)]
    pub subject: Option<String>,
    /// Workspace-relative path the finding belongs to. Required once a run
    /// touches more than one file.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GuestError {
    pub message: String,
    #[serde(default)]
    pub span: [u32; 2],
}

// ---------------------------------------------------------------------------
// Host functions exposed to the guest
// ---------------------------------------------------------------------------

pub(crate) struct GuestState {
    wasi: wasmtime_wasi::preview1::WasiP1Ctx,
    pub findings: Vec<GuestFinding>,
    pub errors: Vec<GuestError>,
}

fn get_memory<T>(caller: &mut wasmtime::Caller<'_, T>) -> Result<wasmtime::Memory, Error> {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| anyhow!("guest has no exported `memory`"))
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
pub(crate) fn clamp_span(sp: [u32; 2]) -> [u32; 2] {
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
// Session: one instantiated module instance per guest call
// ---------------------------------------------------------------------------

pub(crate) struct Session {
    store: Store<GuestState>,
    instance: Instance,
    memory: wasmtime::Memory,
}

impl Session {
    pub(crate) fn open(
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
            .map_err(|e| format!("instantiate guest module: {e}"))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or("guest must export memory")?;
        Ok(Session {
            store,
            instance,
            memory,
        })
    }

    /// Call the guest allocator and copy bytes into guest memory.
    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) -> Result<(i32, i32), String> {
        if bytes.len() > MAX_RESULT_BYTES {
            return Err("input too large for the guest contract".into());
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
    pub(crate) fn call_ret_json(&mut self, name: &str, pairs: &[(i32, i32)]) -> Result<Vec<u8>, String> {
        let f = self
            .instance
            .get_func(&mut self.store, name)
            .ok_or_else(|| format!("guest is missing export `{name}`"))?;
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

    /// Findings the guest streamed via `omni_report` during the last call.
    pub(crate) fn reported(&self) -> &[GuestFinding] {
        &self.store.data().findings
    }

    /// Parse errors the guest streamed via `omni_parse_error`.
    pub(crate) fn reported_errors(&self) -> &[GuestError] {
        &self.store.data().errors
    }
}

// ---------------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------------

pub(crate) fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

pub(crate) fn span_of(sp: [u32; 2]) -> omni_core::Span {
    omni_core::Span::new(sp[0], sp[1].max(sp[0]))
}

/// Load a module and check the exports it must provide.
pub(crate) fn load_module(
    engine: &Engine,
    path: &std::path::Path,
    required_exports: &[&str],
) -> Result<Module, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("cannot read module {}: {e}", path.display()))?;
    let module = Module::from_binary(engine, &bytes)
        .map_err(|e| format!("invalid wasm module {}: {e}", path.display()))?;
    let mut exports = std::collections::BTreeSet::new();
    for exp in module.exports() {
        exports.insert(exp.name().to_string());
    }
    for required in required_exports {
        if !exports.contains(*required) {
            return Err(format!(
                "module `{}` is missing required export `{required}`",
                path.display()
            ));
        }
    }
    Ok(module)
}

/// A fresh wasmtime engine with fuel metering enabled.
pub(crate) fn engine() -> Result<Engine, String> {
    let mut cfg = Config::new();
    cfg.consume_fuel(true);
    Engine::new(&cfg).map_err(|e| format!("wasmtime engine: {e}"))
}

/// Default fueling policy for a caller-supplied limits value.
pub(crate) fn normalize_limits(raw: WasmLimits) -> WasmLimits {
    WasmLimits {
        fuel: Some(raw.fuel.unwrap_or(DEFAULT_FUEL)),
    }
}