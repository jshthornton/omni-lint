# omni-lint

A fast, language-agnostic **framework for building linters**, in Rust. It owns the reusable machinery of tools like RuboCop, ESLint, Credo, and kube-linter — CLI, config, rule scheduling, diagnostics, TODO baselines, and reporting — so a new linter is just a *language plugin + rules*:

- A **language plugin** is a pure **AST provider**: it parses a language and
  publishes an AST plus a JSON *facts* view. It contains **no rules**.
- A **rule** is one independently authored unit. Rules join a ruleset **à la
  carte** — exactly like adding an entry to ESLint's `rules` map: write one
  rule, register it (or drop its `.wasm` in `rules/`), tune it in config.
- Nothing in the core is language-specific. Rules target a facts vocabulary
  (`graphql.facts`, ...), not a parser, so any AST provider for a language
  runs with any rule set for it.

Design background: `docs/brainstorms/2026-09-22-omni-lint-design.md`.

```text
CLI/config ──> discovery ──> language plugins (AST + facts)
                              │
              ruleset (native rules + rules/*.wasm) ──> diagnostics
                              │
                     TODO baseline ──> formatter + exit codes
```

## The built-in GraphQL example proves the seams

`omni-graphql` is a language plugin: it parses `.graphql`/`.gql` SDL, links
definitions across files into a schema model, and publishes `graphql.facts`
(JSON) alongside its typed AST. Its 7 example rules live in
`crates/omni-graphql/src/rules/` — **one file per rule**, each a standalone
unit you can register alone.

| Rule | Reads | Default |
|---|---|---|
| `graphql/no-empty-type` | `graphql.ast` | error |
| `graphql/type-name-pascal` | `graphql.ast` | warning |
| `graphql/field-name-camel` | `graphql.ast` | warning |
| `graphql/deprecated-required-input` | `graphql.schema` | error |
| `graphql/no-undefined-type` | `graphql.schema` | error |
| `graphql/duplicate-type` | `graphql.schema` | error |
| `graphql/no-unused-type` | `graphql.schema` | info (suppressed without a `schema` block) |

## Usage

```sh
cargo install --path crates/omni-lint     # or use target/release/omni-lint

omni-lint check [ROOT] [--format text|json]
omni-lint todo generate [ROOT]   # record current findings in the baseline
omni-lint todo prune [ROOT]      # drop baseline entries for fixed findings
omni-lint list-rules
```

Exit codes: `0` clean (or all findings baselined), `1` findings, `2` config/UI error, `3` internal failure. Parse failures surface as `<plugin>/parse-error` findings, never a crash.

### Config (`omni-lint.toml`, discovered upward from the lint root)

```toml
[rules."graphql/no-empty-type"]
enabled = true            # false disables; a severity string also enables
severity = "error"        # optional override

[rules."acme/no-foo".options]      # rule options, handed to the rule
allow_names = ["id", "guid"]

[targets]
extensions = ["graphql"]  # narrow discovery to these extensions

ignore = ["generated/"]   # glob patterns relative to the config dir
baseline = "/abs/or/toml-relative-path.json"  # defaults to omni-lint-baseline.json

[plugins.graphql]         # free-form per-plugin options, passed verbatim
strict_mode = true
```

### TODO baselines

Findings match baseline entries by **stable subject identity** when the rule provides one (`type:User.field:name`), else by a bounded **context fingerprint** (span text), not line numbers — so files shift without leaking "new" findings. A committed baseline makes an existing codebase pass while new violations still fail. Inline `# omni-lint-disable-next-line rule-id` markers suppress per line.

## Writing a rule (the ESLint story)

A rule is one file. Two flavours:

**1. One-rule WASM module** — author anywhere, ship one `.wasm`, no engine
changes, no registry edits: drop it in `<config dir>/rules/` and it joins the
ruleset. Copy `plugins-src/rules/no-empty-type/` and change two functions:

```rust
use omni_guest::{write_findings, Finding, RuleEnvelope};

#[no_mangle]
pub extern "C" fn omni_rule_meta() -> i32 {
    omni_guest::write_json(r#"{
  "id": "acme/no-empty-type",
  "description": "Types must not be empty.",
  "severity": "error",
  "requires": "graphql.facts"
}"#)
}

#[no_mangle]
pub extern "C" fn omni_rule_run(ptr: i32, len: i32) -> i32 {
    let envelope = RuleEnvelope::read(ptr, len);   // files[{path, source, facts}], workspace, options
    let findings = envelope.files.iter().flat_map(|f| {
        f.facts.defs.iter().filter(|d| d.fields.is_empty())
            .map(|d| Finding::at(f.path.clone(), format!("`{}` is empty", d.name), d.span)
                .subject(d.subject.clone()))
    }).collect::<Vec<_>>();
    write_findings(&findings)                       // or stream with omni_guest::report()
}
```

```sh
cargo build --release --target wasm32-wasip1    # in your rule crate
cp target/wasm32-wasip1/release/acme_no_empty_type.wasm <project>/rules/
```

`requires` names the facts the rule reads: `<language>.facts` (per-file) or
`<language>.workspace` (cross-file model; negotiated at run time — a rule
needing it is skipped when no language module provides one). Findings carry
`{message, span: [start, end] bytes, path, severity?, subject?}`; the engine
re-locates byte offsets to line/column, applies suppression markers,
baselines, severity overrides and `--max-warnings` uniformly.

**2. Native Rust rule** — implement `omni_core::Rule` (see
`crates/omni-graphql/src/rules/no_empty_type.rs`) and register just that rule
in your embedded `Registry` (`register_rule` rejects duplicate ids). Native
rules can downcast typed AST artifacts (`graphql.ast`, `graphql.schema`);
WASM rules consume the JSON facts either way.

The `omni-guest` SDK (`plugins-src/omni-guest/`) gives rule authors the
allocator, contract framing, facts/finding types and options plumbing for
free. A complete rule is ~40 lines — the four in `plugins-src/rules/` cover
file scope, cross-file folds, streaming reports, the workspace model, and
options from config.

## Writing a language plugin

A language plugin parses and publishes capabilities; it never bundles rules.

**Native (Rust):** implement `omni_core::plugin::Plugin` — id, extensions,
capabilities, `parse_file` producing artifacts, optional `build_workspace`.
Publish both a typed AST artifact (for native rules) and the JSON facts under
`<language>.facts` (see `omni-graphql/src/facts.rs`) and every third-party
WASM rule for your language works out of the box.

**WASM language module** (`<config dir>/plugins/*.wasm`): converts any parser
into a plug-in language.

| Export | Meaning |
|---|---|
| `memory` | linear memory |
| `omni_alloc(len) -> ptr` | give the host a buffer to copy bytes into |
| `omni_plugin_meta() -> ptr` | `[len: u32 LE][json]` — `{id, name, extensions, capabilities}` (e.g. `graphql.facts` file, `graphql.workspace` workspace) |
| `omni_plugin_parse(path_ptr, path_len, src_ptr, src_len) -> ptr` | `[len][json]` facts document (+ `errors` array) for one file |
| `omni_plugin_workspace(ptr, len) -> ptr` *(optional)* | `[len][json]` cross-file model over `{"files": [{path, facts}]}`; omit it and the host synthesizes the merged facts automatically |

**Facts vocabulary** (`<language>.facts`, consumed by every rule):
`{defs: [{kind, name, span, subject, fields: [{name, span, type_name, required, directives, subject}], implements, union_members, enum_values, directives}], uses: [{name, span}], directives_defined, directives_used, has_entry_block, roots}` — the workspace view adds a `path` to every `def`/`use`.

Host imports a guest may call: `env.omni_report(ptr, len)` (one finding JSON)
and `env.omni_parse_error(ptr, len)`. Frame: every string-returning export is
`[len: u32 LE][bytes]`.

Both module kinds share the same **sandbox**: fuel metered (5B ticks/call
default, tunable via `plugins/limits.toml` / `rules/limits.toml`), no
filesystem/network (WASI linked with nothing granted), 100k-findings and
64 MB-payload caps, guest spans clamped. A module that traps or lies never
crashes the engine — it surfaces as a load warning and is skipped.

**Costs, measured** (40 SDL files × 50 types, release CLI): native plugin ≈ 12 ms; WASM modules ≈ 207 ms for the same tree. The sandbox overhead is dominated by per-call instantiation + JSON serialization — the order-of-magnitude number is real, and per-file overhead in a lint-scale run is milliseconds. See the design doc for the optimization levers.

Working examples live in `plugins-src/`: `demo-graphql-ast/` (a sandboxed AST
module, no rules) and `rules/*` (four one-rule modules). The same fixture
lints identically through the native example pack or the WASM modules — see
`tests/fixtures/wasm-plugin/`.

## Layout

```
crates/omni-core      engine: config, discovery, scheduling, diagnostics, baselines, report
crates/omni-graphql   example language plugin + one-file-per-rule examples
crates/omni-wasm      sandboxed WASM loading: language modules + one-rule modules
crates/omni-lint      CLI (check / todo generate / todo prune / list-rules)
plugins-src/          guest SDK + demo language module + demo rule modules
plugins/, rules/      drop-in dirs for third-party .wasm modules
```