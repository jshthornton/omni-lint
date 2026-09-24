//! Guest-side SDK for omni-lint WASM modules (both kinds).
//!
//! A module links this crate and gets the boring half of the contract for
//! free:
//!
//!  - the `omni_alloc` export + a bump allocator over guest linear memory
//!  - `[u32 LE len][bytes]` result framing (`write_json`, `write_findings`)
//!  - the shared **facts** vocabulary (`Facts`, `DefFact`, `FieldFact`,
//!    `UseFact`) that every language module publishes and every rule reads
//!  - the rule run envelope (`RuleEnvelope`, `RuleFile`) and `Finding`
//!  - streaming reports through `env.omni_report` / `env.omni_parse_error`
//!
//! A complete rule is ~40 lines: implement `omni_rule_meta` + `omni_rule_run`
//! and copy `rules/no-empty-type` to start.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Memory: bump allocator + `omni_alloc`
// ---------------------------------------------------------------------------

struct BumpAlloc;

unsafe impl core::alloc::GlobalAlloc for BumpAlloc {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        bump_alloc(layout.size())
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {
        // bump: never frees (fine for a linting pass)
    }
}

#[global_allocator]
static A: BumpAlloc = BumpAlloc;

extern "C" {
    static __heap_base: u8;
}

static BUMP: AtomicU32 = AtomicU32::new(0);

/// Allocate `size` bytes past `__heap_base`, growing linear memory as needed.
/// Returns null on failure (page-grow exhaustion).
fn bump_alloc(size: usize) -> *mut u8 {
    let base = unsafe { &__heap_base as *const u8 as usize };
    let offset = BUMP.fetch_add(size as u32, Ordering::Relaxed);
    let ptr = base + offset as usize;
    let new_end = (ptr + size) as u64;
    let current_pages = core::arch::wasm32::memory_size(0);
    let current = (current_pages * 65536usize) as u64;
    if new_end > current {
        let want_pages = new_end.div_ceil(65536) as usize;
        if want_pages > current_pages
            && core::arch::wasm32::memory_grow(0, want_pages - current_pages) == usize::MAX
        {
            return core::ptr::null_mut();
        }
    }
    ptr as *mut u8
}

/// Contract allocation: the host needs a buffer in guest memory to copy the
/// envelope/source into.
#[no_mangle]
pub unsafe extern "C" fn omni_alloc(len: i32) -> i32 {
    let ptr = bump_alloc(len.max(0) as usize) as usize;
    if ptr == 0 {
        return -1;
    }
    ptr as i32
}

/// Write a length-prefixed buffer `[u32 LE len][bytes]`, return its pointer.
pub fn write_result(bytes: &[u8]) -> i32 {
    let total = 4 + bytes.len();
    let ptr = bump_alloc(total) as usize;
    if ptr == 0 {
        return -1;
    }
    let mut off = ptr;
    for b in (bytes.len() as u32).to_le_bytes() {
        unsafe { *(off as *mut u8) = b };
        off += 1;
    }
    for b in bytes {
        unsafe { *(off as *mut u8) = *b };
        off += 1;
    }
    ptr as i32
}

/// Write framed JSON text; return its pointer (this is what exports return).
pub fn write_json(s: &str) -> i32 {
    write_result(s.as_bytes())
}

/// Read host-written bytes at (ptr, len).
pub unsafe fn guest_bytes<'b>(ptr: i32, len: i32) -> &'b [u8] {
    core::slice::from_raw_parts(ptr as *const u8, len.max(0) as usize)
}

/// Read a host-written UTF-8 string (the parse envelope/source).
pub fn read_input(ptr: i32, len: i32) -> String {
    String::from_utf8_lossy(unsafe { guest_bytes(ptr, len) }).into_owned()
}

// ---------------------------------------------------------------------------
// Host imports (findings / parse errors)
// ---------------------------------------------------------------------------

extern "C" {
    fn omni_report(ptr: i32, len: i32);
    fn omni_parse_error(ptr: i32, len: i32);
}

/// Stream one finding to the host right now (`env.omni_report`).
pub fn report(f: &Finding) {
    let json = serde_json::to_string(f).unwrap_or_else(|_| "{\"message\":\"finding\"}".into());
    let bytes = json.as_bytes();
    let ptr = bump_alloc(bytes.len()) as usize;
    if ptr == 0 {
        return;
    }
    for (i, b) in bytes.iter().enumerate() {
        unsafe { ((ptr + i) as *mut u8).write(*b) };
    }
    unsafe { omni_report(ptr as i32, bytes.len() as i32) };
}

/// Stream one parse error to the host (`env.omni_parse_error`). Language
/// modules usually put errors in their facts `errors` array instead.
pub fn parse_error(message: impl Into<String>, span: [u32; 2]) {
    let e = GuestError {
        message: message.into(),
        span,
    };
    let json = serde_json::to_string(&e).unwrap_or_else(|_| "{\"message\":\"parse error\"}".into());
    let bytes = json.as_bytes();
    let ptr = bump_alloc(bytes.len()) as usize;
    if ptr == 0 {
        return;
    }
    for (i, b) in bytes.iter().enumerate() {
        unsafe { ((ptr + i) as *mut u8).write(*b) };
    }
    unsafe { omni_parse_error(ptr as i32, bytes.len() as i32) };
}

/// Serialize findings as the framed JSON array `omni_rule_run` returns.
/// (The host also honours findings streamed with [`report`] — pick either.)
pub fn write_findings(findings: &[Finding]) -> i32 {
    let json = serde_json::to_string(findings).unwrap_or_else(|_| "[]".into());
    write_json(&json)
}

// ---------------------------------------------------------------------------
// The facts vocabulary (shared by every language and every rule)
// ---------------------------------------------------------------------------

/// Facts for one file (the workspace view adds a `path` on `defs`/`uses`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Facts {
    #[serde(default)]
    pub defs: Vec<DefFact>,
    #[serde(default)]
    pub uses: Vec<UseFact>,
    #[serde(default)]
    pub directives_defined: Vec<String>,
    #[serde(default)]
    pub directives_used: Vec<String>,
    #[serde(default)]
    pub has_entry_block: bool,
    #[serde(default)]
    pub roots: Vec<(String, String)>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DefFact {
    pub kind: String,
    pub name: String,
    pub span: [u32; 2],
    pub subject: String,
    #[serde(default)]
    pub fields: Vec<FieldFact>,
    #[serde(default)]
    pub implements: Vec<String>,
    #[serde(default)]
    pub union_members: Vec<String>,
    #[serde(default)]
    pub enum_values: Vec<String>,
    #[serde(default)]
    pub directives: Vec<String>,
    /// Present only in the workspace (merged) view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FieldFact {
    pub name: String,
    pub span: [u32; 2],
    #[serde(default)]
    pub type_name: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub directives: Vec<String>,
    #[serde(default)]
    pub subject: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct UseFact {
    pub name: String,
    pub span: [u32; 2],
    /// Present only in the workspace (merged) view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GuestError {
    pub message: String,
    #[serde(default)]
    pub span: [u32; 2],
}

// ---------------------------------------------------------------------------
// Rule envelope + findings
// ---------------------------------------------------------------------------

/// What `omni_rule_run` receives: every file's source and facts for the
/// target language, the optional cross-file model, this rule's options from
/// `[rules."<id>"]` in `omni-lint.toml`, and the rule subset this invocation
/// is for (one entry for a per-rule adapter invocation; a pack's run function
/// dispatches on `rules[].id`).
#[derive(Debug, Clone, Deserialize)]
pub struct RuleEnvelope {
    pub language: String,
    pub files: Vec<RuleFile>,
    #[serde(default)]
    pub workspace: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Option<serde_json::Value>,
    #[serde(default)]
    pub rules: Vec<RuleRef>,
}

/// One rule of the invocation subset.
#[derive(Debug, Clone, Deserialize)]
pub struct RuleRef {
    /// Full id, `namespace/rule-name` — dispatch on this in a pack.
    pub id: String,
    /// The rule's `[rules."<id>".options]` table, when configured.
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleFile {
    /// Workspace-relative path — echo it back in `Finding::path`.
    pub path: String,
    pub source: String,
    pub facts: Facts,
}

impl RuleEnvelope {
    /// Parse the host's (ptr, len) envelope.
    pub fn read(ptr: i32, len: i32) -> RuleEnvelope {
        let text = read_input(ptr, len);
        serde_json::from_str(&text).unwrap_or_else(|_| RuleEnvelope {
            language: String::new(),
            files: Vec::new(),
            workspace: None,
            options: None,
            rules: Vec::new(),
        })
    }

    /// The one rule ref of this invocation (per-rule adapter calls pass
    /// exactly one).
    pub fn rule(&self) -> Option<&RuleRef> {
        self.rules.first()
    }

    /// The `workspace` facts, when the host has one and it parses as the
    /// merged vocabulary (`defs`/`uses` with `path`s).
    pub fn workspace_facts(&self) -> Option<Facts> {
        self.workspace
            .as_ref()
            .and_then(|v| serde_json::from_value::<Facts>(v.clone()).ok())
    }
}

/// One finding (the JSON array `omni_rule_run` returns; `rule_id` defaults to
/// the rule's own id host-side, same for `severity`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Finding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    pub message: String,
    #[serde(default)]
    pub span: [u32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl Finding {
    /// A finding on `path` with `message` at `span` = `[start, end]` bytes.
    pub fn at(path: impl Into<String>, message: impl Into<String>, span: [u32; 2]) -> Finding {
        Finding {
            path: Some(path.into()),
            message: message.into(),
            span,
            ..Default::default()
        }
    }

    /// Stable subject id for TODO baselines (`type:User.field:name`).
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Per-finding severity override ("error" | "warning" | "info").
    pub fn severity(mut self, severity: impl Into<String>) -> Self {
        self.severity = Some(severity.into());
        self
    }
}