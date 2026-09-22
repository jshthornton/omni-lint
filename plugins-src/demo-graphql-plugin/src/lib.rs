//! Demo third-party plugin: GraphQL SDL via the omni-lint WASM contract.
//!
//! Build: `cargo build --release --target wasm32-wasip1` from this dir.
//! The resulting `target/wasm32-wasip1/release/demo_graphql_plugin.wasm`
//! is a complete third-party plugin: the engine discovers it at runtime and
//! the plugin author ships only the .wasm — no changes to the engine repo.
//!
//! Contract implementation:
//!   omni_plugin_meta       -> plugin metadata (id, extensions, rules)
//!   omni_plugin_parse      -> per-file facts + file rules (no-empty-type)
//!   omni_plugin_run_workspace -> unused/undefined/duplicate type rules
//!
//! The scanner is deliberately simple (a real plugin embeds its own lexer /
//! parser / AST — the contract never constrains guest internals).
//!
//! Memory model: bump allocator over guest linear memory, growing pages via
//! `memory.grow`. Buffers are never freed (fine for a linting pass; should a
//! real plugin want reclaiming memory it needs its own strategy).

#![allow(static_mut_refs)]

use serde::{Deserialize, Serialize};

static mut BUMP: u32 = 0;

/// A bump `GlobalAllocator` over the wasm heap: never frees, grows pages on
/// demand. Contract buffers and std allocations share this.
struct BumpAlloc;

unsafe impl core::alloc::GlobalAlloc for BumpAlloc {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        bump_alloc(layout.size())
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {
        // bump: no free
    }
}

#[global_allocator]
static A: BumpAlloc = BumpAlloc;

extern "C" {
    static __heap_base: u8;
}

/// Allocate `size` bytes from the linear-memory region past `__heap_base`,
/// growing the memory if needed. Returns null on failure (page grow exhaustion).
fn bump_alloc(size: usize) -> *mut u8 {
    let base = unsafe { &__heap_base as *const u8 as usize };
    unsafe {
        let ptr = base + BUMP as usize;
        let new_end = (ptr + size) as u64;
        let current = (core::arch::wasm32::memory_size(0) * 65536usize) as u64;
        if new_end > current {
            let pages = new_end.div_ceil(65536) as u32;
            if core::arch::wasm32::memory_grow(0, pages as usize) == usize::MAX {
                return core::ptr::null_mut();
            }
        }
        BUMP += size as u32;
        ptr as *mut u8
    }
}

extern "C" {
    fn omni_report(ptr: i32, len: i32);
    fn omni_parse_error(ptr: i32, len: i32);
}

/// Contract allocation: the host needs a buffer in guest memory to copy the
/// source/manifest into. Uses the same bump region as the global allocator.
#[no_mangle]
pub unsafe extern "C" fn omni_alloc(len: i32) -> i32 {
    let ptr = bump_alloc(len as usize) as usize;
    if ptr == 0 {
        return -1;
    }
    ptr as i32
}

/// Write a length-prefixed buffer `[u32 LE len][bytes]`, return its pointer.
fn write_result(bytes: &[u8]) -> i32 {
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

/// Write JSON `fyi` bytes part of the buffer.
fn write_json(s: &str) -> i32 {
    write_result(s.as_bytes())
}

// ---------------------------------------------------------------------------
// Facts model
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct DefFact {
    kind: String,
    name: String,
    span: [u32; 2],
    subject: String,
    #[serde(default)]
    fields: Vec<FieldFact>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct FieldFact {
    name: String,
    span: [u32; 2],
    #[serde(rename = "type_name")]
    type_name: Option<String>,
    required: bool,
    directives: Vec<String>,
    subject: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct UseFact {
    name: String,
    span: [u32; 2],
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Facts {
    #[serde(default)]
    errors: Vec<GuestError>,
    #[serde(default)]
    defs: Vec<DefFact>,
    #[serde(default)]
    uses: Vec<UseFact>,
    #[serde(default)]
    directives_defined: Vec<String>,
    #[serde(default)]
    directives_used: Vec<String>,
    #[serde(default)]
    has_entry_block: bool,
    #[serde(default)]
    roots: Vec<(String, String)>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct GuestError {
    message: String,
    span: [u32; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GuestFinding {
    rule_id: String,
    severity: String,
    message: String,
    span: [u32; 2],
    subject: Option<String>,
    path: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Envelope {
    files: Vec<FileEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileEntry {
    path: String,
    facts: Facts,
}

const BUILTIN: [&str; 5] = ["String", "Int", "Boolean", "Float", "ID"];

// ---------------------------------------------------------------------------
// meta
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn omni_plugin_meta() -> i32 {
    write_json(
        r#"{
  "id": "demo-gql",
  "name": "Demo GraphQL plugin (sandboxed WASM)",
  "extensions": ["graphql", "gql"],
  "capabilities": [
    { "name": "demo-gql.facts", "scope": "file" },
    { "name": "demo-gql.workspace", "scope": "workspace" }
  ],
  "rules": [
    { "id": "demo-gql/no-empty-type", "description": "Object/interface/input definitions must have a member.", "severity": "error" },
    { "id": "demo-gql/no-undefined-type", "description": "Referenced types must be defined.", "severity": "error" },
    { "id": "demo-gql/no-unused-type", "description": "Unreferenced definitions are dead code.", "severity": "info" },
    { "id": "demo-gql/duplicate-type", "description": "Type names must be unique.", "severity": "error" }
  ]
}"#,
    )
}

// ---------------------------------------------------------------------------
// parse: one document -> facts + file rules
// ---------------------------------------------------------------------------

/// Tokenizer-level scanner; returns facts for the whole SDL document.
struct Scan<'a> {
    src: &'a str,
    bytes: &'a [u8],
    i: usize,
}

impl<'a> Scan<'a> {
    fn skip_trivia(&mut self) {
        while self.i < self.bytes.len() {
            let c = self.bytes[self.i];
            if c.is_ascii_whitespace() || c == b',' {
                self.i += 1;
            } else if c == b'#' {
                while self.i < self.bytes.len() && self.bytes[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    fn name(&mut self) -> Option<(&'a str, [u32; 2])> {
        let start = self.i;
        if self.i >= self.bytes.len()
            || !(self.bytes[self.i].is_ascii_alphabetic() || self.bytes[self.i] == b'_')
        {
            return None;
        }
        while self.i < self.bytes.len()
            && (self.bytes[self.i].is_ascii_alphanumeric() || self.bytes[self.i] == b'_')
        {
            self.i += 1;
        }
        let name = &self.src[start..self.i];
        Some((name, [start as u32, self.i as u32]))
    }

    fn peek_char(&self, c: u8) -> bool {
        self.bytes.get(self.i) == Some(&c)
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek_char(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
}

fn parse_document(src: &str) -> Facts {
    let mut facts = Facts::default();
    let mut scan = Scan { src, bytes: src.as_bytes(), i: 0 };
    let mut current_def: Option<usize> = None;
    loop {
        scan.skip_trivia();
        if scan.i >= scan.bytes.len() {
            break;
        }
        if scan.eat(b'{') {
            continue; // block open (def body or enum values)
        }
        if scan.eat(b'}') {
            current_def = None; // end def body
            continue;
        }
        let start_pos = scan.i;
        let Some((word, _)) = scan.name() else {
            // punctuation or stray token: step over
            facts.errors.push(GuestError {
                message: format!("cannot identify token near `{}`", &scan.src[start_pos..(start_pos + 12).min(scan.src.len())]),
                span: [start_pos as u32, (start_pos + 1) as u32],
            });
            scan.i += 1;
            continue;
        };
        match word {
            "schema" => {
                scan.skip_trivia();
                if scan.eat(b'{') {
                    // consume to matching }
                    while scan.i < scan.bytes.len() {
                        scan.eat(b'}');
                        break;
                    }
                }
                facts.has_entry_block = true;
            }
            "type" | "interface" | "input" => {
                let kind = match word {
                    "type" => "object".to_string(),
                    other => other.to_string(),
                };
                scan.skip_trivia();
                let def_pos = scan.i;
                if let Some((name, _)) = scan.name() {
                    let span = [def_pos as u32, scan.i as u32];
                    facts.defs.push(DefFact {
                        kind,
                        name: name.to_string(),
                        span,
                        subject: format!("type:{name}"),
                        fields: Vec::new(),
                    });
                    current_def = Some(facts.defs.len() - 1);
                }
            }
            "enum" | "union" | "scalar" | "directive" | "extend" => {
                // swallow remainder for the demo scanner; real plugins model these
                let kind = word.to_string();
                scan.skip_trivia();
                if let Some((name, _)) = scan.name() {
                    if kind == "scalar" {
                        facts.defs.push(DefFact {
                            kind: "scalar".into(),
                            name: name.to_string(),
                            span: [0, 0],
                            subject: format!("type:{name}"),
                            fields: Vec::new(),
                        });
                    }
                }
                current_def = None;
                while scan.i < scan.bytes.len() && scan.bytes[scan.i] != b'{' && scan.bytes[scan.i] != b'\n' {
                    scan.i += 1;
                }
            }
            _ => {
                // Only reachable inside def bodies: field `name: Type`.
                if let Some(def_idx) = current_def {
                    let name = word;
                    let name_span = [start_pos as u32, scan.i as u32];
                    while scan.i < scan.bytes.len() && (scan.bytes[scan.i] == b' ' || scan.bytes[scan.i] == b'\t') {
                        scan.i += 1;
                    }
                    if !scan.eat(b':') {
                        continue;
                    }
                    scan.i += 1;
                    let type_arg = parse_type_ref(&mut scan);
                    let ty = type_arg.named;
                    let required = type_arg.non_null;
                    let subject = format!("type:{}.field:{}", facts.defs[def_idx].name, name);
                    facts.defs[def_idx].fields.push(FieldFact {
                        name: name.to_string(),
                        span: name_span,
                        type_name: Some(ty.clone()),
                        required,
                        directives: vec![],
                        subject,
                    });
                    if !BUILTIN.contains(&ty.as_str()) {
                        facts.uses.push(UseFact {
                            name: ty,
                            span: type_arg.span,
                        });
                    }
                } else {
                    // parts in a document body that demo scanner does not model
                    match word {
                        "extend" | "schema" => {}
                        _ => {}
                    }
                }
            }
        }
    }

    facts
}

/// demo-gql/no-empty-type for fielded definitions with zero fields.
fn empty_type_facts(facts: &Facts) -> Vec<(String, [u32; 2], String)> {
    // (type name, span, subject)
    facts
        .defs
        .iter()
        .filter(|d| matches!(d.kind.as_str(), "object" | "interface" | "input"))
        .filter(|d| d.fields.is_empty())
        .map(|d| (d.name.clone(), d.span, d.subject.clone()))
        .collect()
}

struct TypeRef {
    named: String,
    span: [u32; 2],
    non_null: bool,
}

fn parse_type_ref(scan: &mut Scan) -> TypeRef {
    let start = scan.i;
    while scan.i < scan.bytes.len()
        && (scan.bytes[scan.i].is_ascii_alphanumeric()
            || scan.bytes[scan.i] == b'_'
            || scan.bytes[scan.i] == b'['
            || scan.bytes[scan.i] == b']'
            || scan.bytes[scan.i] == b'!')
    {
        scan.i += 1;
    }
    let raw = &scan.src[start..scan.i];
    let named = raw
        .trim_matches(|c| c == '[' || c == ']' || c == '!')
        .to_string();
    let non_null = raw.ends_with('!');
    TypeRef {
        named,
        span: [start as u32, scan.i as u32],
        non_null,
    }
}

#[no_mangle]
pub extern "C" fn omni_plugin_parse(path_ptr: i32, path_len: i32, src_ptr: i32, src_len: i32) -> i32 {
    let path = String::from_utf8_lossy(unsafe { guest_bytes(path_ptr, path_len) }).into_owned();
    let src = String::from_utf8_lossy(unsafe { guest_bytes(src_ptr, src_len) }).into_owned();
    let facts = parse_document(&src);

    // File rule: demo-gql/no-empty-type.
    for (ty, span, subject) in empty_type_facts(&facts) {
        report(
            &path,
            "demo-gql/no-empty-type",
            "error",
            format!("type `{ty}` is empty; it must define at least one field"),
            span,
            Some(subject),
        );
    }

    write_json(&serde_json::to_string(&facts).unwrap_or_else(|_| "{}".into()))
}

unsafe fn guest_bytes<'b>(ptr: i32, len: i32) -> &'b [u8] {
    core::slice::from_raw_parts(ptr as *const u8, len as usize)
}

// ---------------------------------------------------------------------------
// workspace pass
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn omni_plugin_run_workspace(facts_ptr: i32, facts_len: i32) -> i32 {
    let text = String::from_utf8_lossy(unsafe { guest_bytes(facts_ptr, facts_len) }).into_owned();
    let Ok(envelope) = serde_json::from_str::<Envelope>(&text) else {
        parse_error_json("workspace envelope malformed".into());
        return write_json("[]");
    };

    let mut all_defs: Vec<(&str, &str, [u32; 2])> = Vec::new(); // name,path,span
    let mut all_uses: Vec<(&str, &str, [u32; 2])> = Vec::new();
    for f in &envelope.files {
        for def in &f.facts.defs {
            all_defs.push((&def.name, &f.path, def.span));
        }
        for u in &f.facts.uses {
            all_uses.push((&u.name, &f.path, u.span));
        }
    }

    // demo-gql/no-undefined-type
    for (ty, path, span) in &all_uses {
        if !all_defs.iter().any(|(n, _, _)| n == ty) {
            report(
                path,
                "demo-gql/no-undefined-type",
                "error",
                format!("type `{ty}` is used but not defined in the schema"),
                *span,
                Some(format!("type-ref:{ty}")),
            );
        }
    }

    // demo-gql/duplicate-type (all but the first definition)
    let mut seen = 0usize;
    for (name, path, span) in &all_defs {
        if all_defs[..seen].iter().any(|(n, _, _)| n == name) {
            report(
                path,
                "demo-gql/duplicate-type",
                "error",
                format!("type `{name}` is defined more than once in the schema"),
                *span,
                Some(format!("type:{name}")),
            );
        } else {
            seen += 1;
        }
    }

    // demo-gql/no-unused-type (only with an entry block)
    let has_entry = envelope.files.iter().any(|f| f.facts.has_entry_block);
    if has_entry {
        let roots: Vec<String> = envelope
            .files
            .iter()
            .flat_map(|f| f.facts.roots.iter().map(|(_, t)| t.clone()))
            .collect();
        for (name, path, span) in &all_defs {
            let referenced = all_uses.iter().any(|(u, _, _)| u == name);
            let root = roots.iter().any(|r| r == name);
            if !referenced && !root {
                report(
                    path,
                    "demo-gql/no-unused-type",
                    "info",
                    format!("type `{name}` is defined but never referenced"),
                    *span,
                    Some(format!("type:{name}")),
                );
            }
        }
    }

    write_json("[]")
}

fn report(
    path: &str,
    rule_id: &str,
    severity: &str,
    message: String,
    span: [u32; 2],
    subject: Option<String>,
) {
    let f = GuestFinding {
        rule_id: rule_id.into(),
        severity: severity.into(),
        message,
        span,
        subject,
        path: Some(path.into()),
    };
    let json = serde_json::to_string(&f).unwrap_or_else(|_| "{}".into());
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

fn parse_error_json(message: String) {
    let e = GuestError { message, span: [0, 0] };
    let json = serde_json::to_string(&e).unwrap_or_else(|_| "{}".into());
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

