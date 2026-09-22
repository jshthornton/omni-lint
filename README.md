# omni-lint

A fast, language-agnostic **framework for building linters**, in Rust. It owns the reusable machinery of tools like RuboCop, ESLint, Credo, and kube-linter — CLI, config, rule scheduling, diagnostics, TODO baselines, and reporting — so a new linter is just a *domain plugin + rules*.

Design background: `docs/brainstorms/2026-09-22-omni-lint-design.md`.

```text
CLI/config ──> discovery ──> parse plugins ──> workspace models
                              │
                   rule scheduler ──> diagnostics
                              │
                     TODO baseline ──> formatter + exit codes
```

## The built-in GraphQL plugin proves the seams

`omni-graphql` parses `.graphql`/`.gql` SDL, links definitions across files into a schema model, and contributes 7 rules. Playing a second role: writing this plugin took no changes to `omni-core` and none to the CLI.

| Rule | Scope | Default |
|---|---|---|
| `graphql/no-empty-type` | file | error |
| `graphql/type-name-pascal` | file | warning |
| `graphql/field-name-camel` | file | warning |
| `graphql/deprecated-required-input` | workspace | error |
| `graphql/no-undefined-type` | workspace | error |
| `graphql/duplicate-type` | workspace | error |
| `graphql/no-unused-type` | workspace | info (suppressed without a `schema` block) |

## Usage

```sh
cargo install --path crates/omni-lint     # or use target/release/omni-lint

omni-lint check [ROOT] [--format text|json]
omni-lint todo generate [ROOT]   # record current findings in the baseline
omni-lint todo prune [ROOT]      # drop baseline entries for fixed findings
omni-lint list-rules
```

Exit codes: `0` clean (or all findings baselined), `1` findings, `2` config/UI error, `3` internal failure. Parse failures surface as `graphql/parse-error` findings, never a crash.

### Config (`omni-lint.toml`, discovered upward from the lint root)

```toml
[rules."graphql/no-empty-type"]
enabled = true            # false disables; a severity string also enables
severity = "error"        # optional override

[targets]
extensions = ["graphql"]  # narrow discovery to these extensions

ignore = ["generated/"]   # glob patterns relative to the config dir
baseline = "/abs/or/toml-relative-path.json"  # defaults to omni-lint-baseline.json

[plugins.graphql]         # free-form per-plugin options, passed verbatim
strict_mode = true
```

### TODO baselines

Findings match baseline entries by **stable subject identity** when the plugin provides one (`type:User.field:name`), else by a bounded **context fingerprint** (span text), not line numbers — so files shift without leaking "new" findings. A committed baseline makes an existing codebase pass while new violations still fail.

## Writing a plugin (v1: compile-time crate)

1. Implement `omni_core::plugin::Plugin` — id, extensions, capabilities
   (`graphql.ast` file-scoped, `graphql.schema` workspace-scoped), `parse_file`
   producing typed artifacts, optional `build_workspace`.
2. Implement `omni_core::plugin::Rule` per rule: metadata plus a `run` that
   downcasts artifacts and reports `Diagnostic`s with byte-offset `Span`s and
   (optionally) stable subject strings.
3. Register both in a `Registry` from `crates/omni-lint/src/main.rs::registry()`.
4. Recover, don't crash: parse errors are diagnostics.

The engine guarantees: each file is parsed **once** (parallel), workspace models are built **only** when an enabled rule needs them, diagnostics are re-located to line/column by the host, and output is deterministic.

## Sandboxed WASM plugins (third-party)

Plugins load **at runtime** from `<config dir>/plugins/*.wasm` — third-party authors ship one `.wasm` file plus nothing else; they never contribute to this repo.

```sh
# Author a plugin in any language that targets wasm32-wasip1 (Rust, TinyGo,
# Swift/AssemblyScript by hand, etc):
cargo build --release --target wasm32-wasip1   # in plugins-src/demo-graphql-plugin
cp target/wasm32-wasip1/release/demo_graphql_plugin.wasm <project>/plugins/
```

Route files to it with config when a first-party plugin claims the same extension:

```toml
prefer = "demo-gql"   # plugin id from its metadata
```

**Contract** (a guest is a classic wasm32-wasip1 module):

| Export | Meaning |
|---|---|
| `memory` | linear memory |
| `omni_alloc(len) -> ptr` | give the host a buffer to copy bytes into |
| `omni_plugin_meta() -> ptr` | `[len: u32 LE][json]` — `{id, name, extensions, capabilities, rules}` |
| `omni_plugin_parse(path_ptr, path_len, src_ptr, src_len) -> ptr` | `[len][json]` facts for one file; file rules report via `omni_report` |
| `omni_plugin_run_workspace(facts_ptr, facts_len) -> ptr` | `[len][json array of findings]` cross-file pass |

Host imports the guest can call: `env.omni_report(ptr, len)` (finding: `{rule_id, severity, message, span: [start, end], subject?}`) and `env.omni_parse_error(ptr, len)`.

**Sandbox guarantees:** fuel metered (5B ticks/call default, tunable via `plugins/limits.toml`), no filesystem/network (WASI linked with nothing granted), 100k-findings and 64 MB-payload caps, guest spans clamped. A plugin that traps or lies never crashes the engine — it surfaces as a config diagnostic.

**Costs, measured** (40 SDL files × 50 types, release CLI): native plugin ≈ 12 ms; wasm plugin ≈ 207 ms for the same tree. The sandbox overhead is dominated by per-call instantiation + JSON serialization — the order-of-magnitude number is real, and per-file overhead in a lint-scale run is milliseconds. See the design doc for the optimization levers.

A complete, working example lives in `plugins-src/demo-graphql-plugin/` (Rust, no_std-ish, hand-rolled bump allocator over `memory.grow`): facts about types it scans, rules for empty/undefined/duplicate/unused types, JSON report — the same fixture lints identically whether the plugin is native or WASM. Do read that contract test as the third-party story's proof: `tests/fixtures/wasm-plugin/`.
