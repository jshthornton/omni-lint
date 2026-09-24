//! Demo language module: GraphQL SDL as an omni-lint **AST plugin**.
//!
//! Build: `cargo build --release --target wasm32-wasip1` in `plugins-src/`,
//! then drop `demo_graphql_ast.wasm` into `<config dir>/plugins/`. The
//! engine loads it at run time and publishes its facts to any rule that
//! `requires = "graphql.facts"` — including separately authored `.wasm`
//! rules in `<config dir>/rules/`. **No rules live in this module.**
//!
//! Contract implementation:
//!   omni_plugin_meta   -> id, extensions, capabilities (graphql.facts/workspace)
//!   omni_plugin_parse  -> one document -> facts + parse errors
//!
//! No `omni_plugin_workspace` here on purpose: the host then synthesizes the
//! cross-file facts (`graphql.workspace`) from the per-file facts, which is
//! enough for most workspace rules. A plugin with a real semantic model can
//! export `omni_plugin_workspace` to override the merge.
//!
//! The scanner is deliberately simple (a real plugin embeds its own lexer /
//! parser / AST — the contract never constrains guest internals). Memory:
//! see `omni-guest`'s bump allocator.

use omni_guest::{read_input, write_json, DefFact, Facts, FieldFact, GuestError, UseFact};
use serde_json::json;

const BUILTIN: [&str; 5] = ["String", "Int", "Boolean", "Float", "ID"];

// ---------------------------------------------------------------------------
// meta
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn omni_plugin_meta() -> i32 {
    write_json(
        r#"{
  "id": "demo-gql",
  "name": "Demo GraphQL AST module (sandboxed WASM)",
  "extensions": ["graphql", "gql"],
  "capabilities": [
    { "name": "graphql.facts", "scope": "file" },
    { "name": "graphql.workspace", "scope": "workspace" }
  ]
}"#,
    )
}

// ---------------------------------------------------------------------------
// parse: one document -> facts (+ parse errors)
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
        Some((&self.src[start..self.i], [start as u32, self.i as u32]))
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.bytes.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, w: &str) -> bool {
        self.skip_trivia();
        if self.src[self.i..].starts_with(w) {
            let after = self.i + w.len();
            let boundary = self
                .bytes
                .get(after)
                .map(|c| !c.is_ascii_alphanumeric() && *c != b'_')
                .unwrap_or(true);
            if boundary {
                self.i = after;
                return true;
            }
        }
        false
    }
}

/// Parse one type reference (`[Post!]!` -> ("Post", span, non_null)).
fn type_ref(scan: &mut Scan) -> (String, [u32; 2], bool) {
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
    (named, [start as u32, scan.i as u32], raw.ends_with('!'))
}

/// Collect `@directive` names until `stop` (or an unclaimed token).
fn directives_until(scan: &mut Scan, stop: u8) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        let save = scan.i;
        scan.skip_trivia();
        if scan.i >= scan.bytes.len() || scan.bytes[scan.i] == stop {
            return out;
        }
        if scan.eat(b'@') {
            if let Some((n, _)) = scan.name() {
                out.push(n.to_string());
                skip_arg_list(scan);
                continue;
            }
        }
        scan.i = save;
        return out;
    }
}

fn skip_arg_list(scan: &mut Scan) {
    scan.skip_trivia();
    if scan.eat(b'(') {
        let mut depth = 1;
        while scan.i < scan.bytes.len() && depth > 0 {
            match scan.bytes[scan.i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            scan.i += 1;
        }
    }
}

/// Scan one SDL document and return the framed facts JSON for the host.
fn scan_document(src: &str) -> i32 {
    let mut facts = Facts::default();
    let mut errors: Vec<GuestError> = Vec::new();
    let mut scan = Scan {
        src,
        bytes: src.as_bytes(),
        i: 0,
    };
    let mut current_def: Option<usize> = None;
    loop {
        scan.skip_trivia();
        if scan.i >= scan.bytes.len() {
            break;
        }
        if scan.eat(b'{') {
            continue; // body opens (def body / enum values)
        }
        if scan.eat(b'}') {
            current_def = None;
            continue;
        }
        let start_pos = scan.i;
        let word_match = scan.name();
        let Some((word, _)) = word_match else {
            errors.push(GuestError {
                message: format!(
                    "cannot identify token near `{}`",
                    &scan.src[start_pos..(start_pos + 12).min(scan.src.len())]
                ),
                span: [start_pos as u32, (start_pos + 1) as u32],
            });
            scan.i += 1;
            continue;
        };
        match word {
            "schema" => {
                // `schema { query: QueryRoot mutation: MutationRoot }`
                scan.skip_trivia();
                if scan.eat(b'{') {
                    loop {
                        scan.skip_trivia();
                        if scan.i >= scan.bytes.len() || scan.eat(b'}') {
                            break;
                        }
                        let Some((op, _)) = scan.name() else {
                            scan.i += 1;
                            continue;
                        };
                        scan.skip_trivia();
                        if scan.eat(b':') {
                            scan.skip_trivia();
                            let (ty, _, _) = type_ref(&mut scan);
                            facts.roots.push((op.to_string(), ty));
                        }
                    }
                }
                facts.has_entry_block = true;
            }
            "type" | "interface" | "input" | "enum" | "union" | "scalar" => {
                let kind = match word {
                    "type" => "object",
                    other => other,
                };
                scan.skip_trivia();
                let name_match = scan.name();
                let Some((name, name_span)) = name_match else {
                    errors.push(GuestError {
                        message: format!("`{word}` without a name"),
                        span: [start_pos as u32, scan.i as u32],
                    });
                    continue;
                };
                let name = name.to_string();
                let directives = directives_until(&mut scan, b'{');
                facts.directives_used.extend(directives.iter().cloned());
                facts.defs.push(DefFact {
                    kind: kind.to_string(),
                    name: name.clone(),
                    span: name_span,
                    subject: format!("type:{name}"),
                    fields: Vec::new(),
                    implements: Vec::new(),
                    union_members: Vec::new(),
                    enum_values: Vec::new(),
                    directives,
                    path: None,
                });
                current_def = Some(facts.defs.len() - 1);

                match kind {
                    "object" | "interface" => {
                        // `implements A & B` before the body
                        let save = scan.i;
                        if scan.eat_word("implements") {
                            loop {
                                scan.skip_trivia();
                                let iface_match = scan.name();
                                let Some((iface, _)) = iface_match else { break };
                                let idx = facts.defs.len() - 1;
                                facts.defs[idx].implements.push(iface.to_string());
                                facts.uses.push(UseFact {
                                    name: iface.to_string(),
                                    span: name_span,
                                    path: None,
                                });
                                scan.skip_trivia();
                                scan.eat(b'&');
                            }
                        } else {
                            scan.i = save;
                        }
                    }
                    "union" => {
                        // `union U = A | B` (no body block)
                        scan.skip_trivia();
                        scan.eat(b'=');
                        loop {
                            scan.skip_trivia();
                            let member_match = scan.name();
                            let Some((member, member_span)) = member_match else { break };
                            let idx = facts.defs.len() - 1;
                            facts.defs[idx].union_members.push(member.to_string());
                            facts.uses.push(UseFact {
                                name: member.to_string(),
                                span: member_span,
                                path: None,
                            });
                            scan.skip_trivia();
                            if !scan.eat(b'|') {
                                break;
                            }
                        }
                        current_def = None;
                    }
                    "scalar" => current_def = None,
                    _ => {}
                }
            }
            "directive" => {
                // `directive @name on ...` — record the name (without @)
                scan.skip_trivia();
                scan.eat(b'@');
                let dname = scan.name().map(|(n, _)| n.to_string());
                if let Some(n) = dname {
                    facts.directives_defined.push(n);
                }
                current_def = None;
            }
            _ => {
                // Inside a def body:
                //   field-name: TypeRef @directive   (object/interface/input)
                //   ENUM_VALUE                        (enum)
                let word_span = [start_pos as u32, scan.i as u32];
                let Some(def_idx) = current_def else { continue };
                match facts.defs[def_idx].kind.as_str() {
                    "enum" => {
                        facts.defs[def_idx].enum_values.push(word.to_string());
                    }
                    "object" | "interface" | "input" => {
                        scan.skip_trivia();
                        if !scan.eat(b':') {
                            continue;
                        }
                        scan.skip_trivia();
                        let (named, ty_span, required) = type_ref(&mut scan);
                        let directives = directives_until(&mut scan, b'\n');
                        facts.directives_used.extend(directives.iter().cloned());
                        let subject = format!("type:{}.field:{}", facts.defs[def_idx].name, word);
                        facts.defs[def_idx].fields.push(FieldFact {
                            name: word.to_string(),
                            span: word_span,
                            type_name: Some(named.clone()),
                            required,
                            directives,
                            subject,
                        });
                        if !BUILTIN.contains(&named.as_str()) {
                            facts.uses.push(UseFact {
                                name: named,
                                span: ty_span,
                                path: None,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let out = json!({
        "errors": errors,
        "defs": facts.defs,
        "uses": facts.uses,
        "directives_defined": facts.directives_defined,
        "directives_used": facts.directives_used,
        "has_entry_block": facts.has_entry_block,
        "roots": facts.roots.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
    });
    write_json(&out.to_string())
}

/// `omni_plugin_parse`: one (path, source) -> framed facts JSON.
#[no_mangle]
pub extern "C" fn omni_plugin_parse(path_ptr: i32, path_len: i32, src_ptr: i32, src_len: i32) -> i32 {
    let _path = read_input(path_ptr, path_len);
    let src = read_input(src_ptr, src_len);
    scan_document(&src)
}