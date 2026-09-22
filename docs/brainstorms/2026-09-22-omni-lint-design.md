---
date: 2026-09-22
topic: omni-lint
status: proposed-design
---

# Omni-lint: a Rust framework for building linters

## Product thesis

Give authors the reusable parts of RuboCop, ESLint, Credo, and kube-linter—CLI, configuration, rule execution, diagnostics, suppression/TODO baselines, and reporting—without reimplementing those tools or promising compatibility with their rules. A developer supplies a domain plugin (parser and optional semantic model) and rule packs; users run one consistent CLI. GraphQL schemas are the first suggested proving ground, not a hard-coded special case.

**Not a universal AST:** syntax and semantics differ too much between languages and domains. The engine is domain-neutral; plugins own their syntax tree and expose a small common navigation/diagnostic boundary. Rules can depend on a particular plugin's typed nodes or capabilities. A generic tree-query facility may help simple rules later, but must not erase domain-specific semantics.

## Architecture

```text
CLI/config ──> discovery ──> parse plugins ──> optional workspace analysis
                  │                │                       │
                  └──────────── rule scheduler <───────────┘
                                      │
                           diagnostics + proposed edits
                                      │
                      suppression / TODO baseline
                                      │
                         formatter + exit policy
```

- **Host (Rust):** path discovery, ignores, config layering, plugin/rule registry, scheduling, cancellation, diagnostics, baselines, stable output formats, and exit codes. No language-specific parsing or AST node kinds in the host.
- **Domain plugin:** declares extensions/input types, parses input into a syntax tree with source ranges, optionally builds a cross-file semantic model, and defines native comment directives if it supports inline suppression. A plugin may also discover virtual or generated inputs. Parsing failures are reported as diagnostics, not crashes.
- **Rule pack:** declares rules and metadata (namespaced ID, description, severity, options schema, required capabilities, scope). File rules inspect parsed input; workspace rules inspect a semantic model spanning files. A rule emits diagnostics, not terminal output.
- **Shared contracts:** `SourceId`, byte-offset `Span`, `Diagnostic` (rule ID, severity, message, primary span, optional related spans/help), and optionally a proposed edit. Span-to-line/column conversion belongs to the host. Capability negotiation lets rules require e.g. `graphql.schema` without asking the host to understand GraphQL.
- **Plugin boundary:** host-owned source buffers and opaque parse handles; the domain exposes traversal/queries and lifetime rules. Never require every parser to serialize its entire tree into a universal interchange format. Exact ABI/query details are a planning/prototype question.

## Plugin deployment strategy

Begin with **compile-time Rust crates** for parsers and rule packs: the fastest, simplest path to prove the contracts and avoid premature ABI commitments. Define an extension-facing capability contract so the host can add **sandboxed WASM components** for third-party distribution once the contract is tested. WASM trades some performance and API flexibility for isolation and portable installation; it should be measured, not assumed free. Do not load arbitrary native dynamic libraries from untrusted projects by default. External-process plugins are a later escape hatch for non-Rust authors if demand warrants them.

A v1 compiled distribution can include its first domain plugin and accept additional domain/rule crates at build time. The aspiration of installing a third-party plugin at runtime is **not** a v1 claim. Version the eventual host/plugin contract independently from config and rule IDs; incompatible plugins fail early with an actionable error. Do not market compatibility with existing linter plugin ecosystems.

## User flows and example

1. **Author:** implement a parser/capability provider, register file patterns and optional workspace model; implement rules against its typed API; test against fixture inputs and expected diagnostics.
2. **Run:** `omni-lint check [paths]` loads config, discovers inputs, parses each input once, builds only requested workspace capabilities, runs enabled rules, applies suppressions/baseline, renders findings, and exits nonzero for unsuppressed findings above the configured threshold.
3. **Adopt incrementally:** `omni-lint todo generate` records existing findings in a baseline; `omni-lint check` reports new findings while tracking matched existing ones; `omni-lint todo prune` removes stale entries. Baselines are machine-readable, reviewable, and can be scoped or expired. Inline suppression is optional and defined by the domain plugin.

For example, a GraphQL plugin parses `.graphql` files, builds a linked schema across files, and provides `graphql.schema`. A `graphql/no-deprecated-required-field` rule inspects that model and emits a diagnostic on the field declaration. The host knows nothing about GraphQL field nodes, yet its CLI, config, baseline, formatter, and exit policy still work.

## Baseline identity and reporting

Rule ID + normalized path + domain-provided stable subject identity (when available) + a bounded context fingerprint should identify existing findings. Line number alone is too fragile. Ambiguous matches must not silently suppress new findings; stale entries should be surfaced/pruned, and moves/renames need explicit tests. A plugin with no stable subject falls back to a documented context fingerprint. Config supports rule-level severity/options, path overrides, ignore patterns, and a baseline path; plugin configuration is namespaced. Human-readable and JSON output are v1; SARIF is a later integration.

## Performance and reliability constraints

Parse each file once per domain, cache source/line maps, parallelize independent files, and run workspace rules only after their required model is ready. Avoid global locks in hot paths. Incremental on-disk caching is deferred until benchmarks show value and invalidation semantics are known. Deterministic ordering at output is mandatory even with parallel execution. A bad rule/plugin yields a bounded error and nonzero tooling exit rather than silently claiming a clean lint run. WASM plugins, when introduced, need explicit memory/time/fuel and filesystem-access limits. Separate exit statuses for findings, configuration/plugin errors, and internal failures.

## Scope and release slices

**First usable slice:** Rust host and CLI, compiled GraphQL parser/semantic plugin, a few representative file and workspace rules, config, text/JSON diagnostics, baseline generate/check/prune, fixtures and performance benchmarks. This proves extension seams with a second small parser fixture or domain adapter before declaring the API stable.

**Later:** runtime-installable sandboxed plugins, rule distribution/registry, fix application, richer query DSL, watch mode, editor protocol, SARIF, external-process plugins, persistent cache. None should be necessary to write a new compiled domain plugin in v1.

**Outside identity:** replacing or faithfully running RuboCop/ESLint/Credo/kube-linter; translating every foreign rule/config; imposing a universal AST; accepting arbitrary native code from a project config as a safe plugin format.

## Success criteria and open design tests

- A second domain can add parsing and rules without changing the host's core types or CLI behavior.
- GraphQL file and cross-file rules produce precise, deterministic locations and correctly distinguish new versus baselined findings.
- Benchmarks capture cold-start, parse, rule, and end-to-end time and peak memory versus a simple single-threaded baseline; speed claims are based on measurements rather than the choice of Rust alone.
- Prototype the ownership/query boundary before stabilizing the plugin API: typed Rust rules over a compiled plugin, then a potential serialized/capability-oriented WASM boundary.
- Confirm whether the first external adopters need runtime plugin installation; if so, move sandboxed plugin support earlier and accept a narrower v1 API.

No code is implemented by this document.

## Addendum: sandboxed WASM plugin loading (2026-09-22)

Implemented as predicted in the release-slice section: `crates/omni-wasm` loads classic `wasm32-wasip1` modules from `<config dir>/plugins/*.wasm`, exposes a JSON + length-prefixed buffer contract (`omni_plugin_meta`, `omni_plugin_parse`, `omni_plugin_run_workspace`), and adapts them to the engine `Plugin` trait with deferred load errors. Third-party plugins ship a single `.wasm` file — no changes to this repo.

**Sandbox posture:** per-call fuel metering (default 5B ticks), no filesystem/network WASI grants (WASI linked but with no preopens, argv, or env), findings capped at 100k per call, result payloads capped at 64 MB, guest-reported spans clamped into engine-safe bounds.

**Measured performance** (40 generated SDL files, 50 types each, release build, same machine):

```text
    native plugin (GraphQL crate, 7 rules):   ~12 ms total
    wasm plugin (demo, 4 rules):             ~207 ms total
```

So the sandbox costs roughly one order of magnitude in this micro-benchmark — dominated by per-call Store/instance setup and JSON re-serialization, not execution (the guest does less work than the native plugin in this run; the number must be read as an *ordering* claim, not a rule-for-rule count). Optimization levers for later: reuse a pooled instance for a whole workspace, pool a more compact facts wire format, compile once and reuse the module cache.

**Contract surface the guest must implement:** `memory`, `omni_alloc(len) -> ptr`, the three `omni_plugin_*` exports, and the two host imports (`env.omni_report`, `env.omni_parse_error`). Missing/malformed metadata is a load failure, not a crash; guests are told the enabled rules and know how the host prunes.
