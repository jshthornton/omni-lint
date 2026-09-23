//! omni-lint CLI: check / todo generate / todo prune / list-rules.

use omni_core::baseline::{self, Baseline};
use omni_core::config::Config;
use omni_core::report::{format_text, write_json, OutputFormat};
use omni_core::runner::{run_check, ExitCode};
use omni_core::{Plugin, Registry};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(feature = "graphql")]
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err((message, code)) => {
            eprintln!("error: {message}");
            code
        }
    };
    std::process::exit(code as i32)
}

struct Usage {
    kind: UsageKind,
    positional: Vec<String>,
    format: OutputFormat,
    /// `--max-warnings N`: exit 0 when only warnings remain and their count
    /// is <= N. None: any finding fails the run.
    max_warnings: Option<usize>,
    /// `--pairs` (todo generate only): record per-(rule, path) suppression
    /// budgets instead of per-finding identities.
    pairs: bool,
}

enum UsageKind {
    Check,
    TodoGenerate,
    TodoPrune,
    ListRules,
    Help,
    Version,
}

fn parse_args(args: &[String]) -> Result<Usage, (String, ExitCode)> {
    let mut kind: Option<UsageKind> = None;
    let mut positional = Vec::new();
    let mut format: Option<OutputFormat> = None;
    let mut max_warnings: Option<usize> = None;
    let mut pair_note = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--help" | "-h" => {
                return Ok(Usage {
                    kind: UsageKind::Help,
                    positional: vec![],
                    format: OutputFormat::Text,
                    max_warnings: None,
                    pairs: false,
                })
            }
            "--version" | "-V" => {
                return Ok(Usage {
                    kind: UsageKind::Version,
                    positional: vec![],
                    format: OutputFormat::Text,
                    max_warnings: None,
                    pairs: false,
                })
            }
            "--format" | "-f" => {
                i += 1;
                let v = args.get(i).ok_or_else(|| ("--format needs a value".to_string(), ExitCode::Config))?;
                format = Some(
                    OutputFormat::parse(v)
                        .ok_or((format!("unknown output format \"{v}\" (text, json)"), ExitCode::Config))?,
                );
            }
            "--format=json" | "--format=text" => {
                let v = a.trim_start_matches("--format=");
                format = Some(
                    OutputFormat::parse(v)
                        .ok_or((format!("unknown output format \"{v}\" (text, json)"), ExitCode::Config))?,
                );
            }
            "--" => {} // end of flags; positional-only args follow
            "--pairs" => {
                pair_note = true;
            }

            "--max-warnings" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| ("--max-warnings needs a count".to_string(), ExitCode::Config))?;
                let n: usize = v
                    .parse()
                    .map_err(|_| (format!("--max-warnings must be a count, got {v:?}"), ExitCode::Config))?;
                max_warnings = Some(n);
            }
            s if s.strip_prefix("--max-warnings=").is_some() => {
                let v = s.strip_prefix("--max-warnings=").unwrap();
                let n: usize = v
                    .parse()
                    .map_err(|_| (format!("--max-warnings must be a count, got {v:?}"), ExitCode::Config))?;
                max_warnings = Some(n);
            }
            // Unknown flags used to silently get treated as ROOT; reject them
            // as configuration mistakes instead.
            s if s.starts_with('-') && s != "-" => {
                return Err((
                    format!("unknown argument {s:?}; use `-h` for usage"),
                    ExitCode::Config,
                ));
            }

            "check" | "todo" | "list-rules" if kind.is_none() => {
                if a == "todo" {
                    i += 1;
                    kind = Some(match args.get(i).map(|s| s.as_str()) {
                        Some("generate") => UsageKind::TodoGenerate,
                        Some("prune") => UsageKind::TodoPrune,
                        _ => return Err(("todo requires `generate` or `prune`".into(), ExitCode::Config)),
                    });
                } else {
                    kind = Some(if a == "check" {
                        UsageKind::Check
                    } else {
                        UsageKind::ListRules
                    });
                }
            }
            _ => positional.push(a.clone()),
        }
        i += 1;
    }
    Ok(Usage {
        kind: kind.unwrap_or(UsageKind::Check),
        positional,
        format: format.unwrap_or(OutputFormat::Text),
        max_warnings,
        pairs: pair_note,
    })
}

fn run(args: &[String]) -> Result<ExitCode, (String, ExitCode)> {
    let usage = parse_args(args)?;
    match usage.kind {
        UsageKind::Help => {
            print_help();
            Ok(ExitCode::Clean)
        }
        UsageKind::Version => {
            println!("omni-lint {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::Clean)
        }
        UsageKind::ListRules => {
            let root = usage
                .positional
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let cfg = config_for(&root)?;
            let registry = registry_for(Some(&cfg));
            for m in registry.rule_meta() {
                println!(
                    "{}: {} (default severity: {})",
                    m.id, m.description, m.default_severity
                );
            }
            Ok(ExitCode::Clean)
        }
        UsageKind::Check => {
            let root = usage
                .positional
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            check(&root, usage.format, usage.max_warnings)
        }
        UsageKind::TodoGenerate => {
            let root = usage
                .positional
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            todo_generate(&root, usage.pairs)
        }
        UsageKind::TodoPrune => {
            let root = usage
                .positional
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            todo_prune(&root)
        }
    }
}

fn build_registry(repo: &mut Registry) {
    // Example plugin packs are opt-in at build time, NOT part of the default
    // package: the framework ships rule-free (see README); third-party packs
    // load at runtime from plugins/*.wasm.
    #[cfg(feature = "graphql")]
    {
        repo.register(Arc::new(omni_graphql::GraphqlPlugin), omni_graphql::rules::all_rules());
    }
    #[cfg(not(feature = "graphql"))]
    {
        // Rule-free framework build: nothing registered here. Third-party
        // packs load at runtime from plugins/*.wasm (see registry_for).
        let _ = repo;
    }
}



/// Native plugins + any `.wasm` plugins found under `<config dir>/plugins/`.
/// A third-party module that fails validation degrades to a load error on
/// that plugin (surfaced by the runner as a config-severity warning) instead
/// of bricking the whole run.
fn registry_for(config: Option<&omni_core::config::Config>) -> Registry {
    let mut r = Registry::new();
    build_registry(&mut r);
    if let Some(cfg) = config {
        let plugins_dir = cfg.dir.join("plugins");
        if plugins_dir.is_dir() {
            let limits = load_wasm_limits(&plugins_dir);
            let (wasm_plugins, errors) = omni_wasm::WasmPlugin::discover(&plugins_dir, limits);
            for (path, err) in errors {
                eprintln!("warning: plugin {} failed to load: {err}", path.display());
            }
            for plugin in wasm_plugins {
                if let Some(err) = plugin.load_error() {
                    eprintln!(
                        "warning: plugin {} unusable: {err}",
                        plugin.module_path().display()
                    );
                    continue;
                }
                let mut rules: Vec<std::sync::Arc<dyn omni_core::Rule>> = Vec::new();
                if let Some(meta) = plugin.meta() {
                    for rule in &meta.rules {
                        rules.push(std::sync::Arc::new(omni_wasm::MetaOnlyRule::new(
                            omni_core::plugin::RuleMeta {
                                id: leak_static(&rule.id),
                                description: leak_static(&rule.description),
                                default_severity: rule
                                    .severity
                                    .as_deref()
                                    .and_then(omni_core::Severity::parse)
                                    .unwrap_or(omni_core::Severity::Warning),
                                // findings arrive via the plugin itself
                                requires: "",
                            },
                        )));
                    }
                }
                let plugin_arc: std::sync::Arc<dyn Plugin> = std::sync::Arc::new(plugin);
                r.register(plugin_arc, rules);
            }
        }
    }
    r
}

fn leak_static(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// Optional `plugins/limits.toml`: per-module fuel tweaks.
/// ```toml
/// [limits."my-plugin.wasm"]
/// fuel = 10_000_000_000
/// ```
fn load_wasm_limits(plugins_dir: &Path) -> omni_wasm::WasmLimits {
    let path = plugins_dir.join("limits.toml");
    let _ = &path;
    let mut default = omni_wasm::WasmLimits { fuel: Some(omni_wasm::DEFAULT_FUEL) };
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(raw) = toml::from_str::<toml::Table>(&text) {
            // A global default without named section is fine too.
            if let Some(fuel) = raw.get("fuel").and_then(|v| v.as_integer()) {
                default.fuel = Some(fuel.max(0) as u64);
            }
            if let Some(section) = raw.get("limits") {
                let _ = section; // per-module overrides land with a config rewrite
            }
        }
    }
    default
}

fn config_for(root: &Path) -> Result<Config, (String, ExitCode)> {
    let start = if root.is_dir() { root } else { root.parent().unwrap_or(Path::new(".")) };
    let cfg = Config::load(start).map_err(|e| (e, ExitCode::Config))?;
    Ok(cfg)
}

fn check(root: &Path, format: OutputFormat, max_warnings: Option<usize>) -> Result<ExitCode, (String, ExitCode)> {
    let cfg = config_for(root)?;
    let registry = registry_for(Some(&cfg));
    if registry.plugins.is_empty() {
        eprintln!(
            "warning: no lint plugins loaded. Install example packs (cargo build -p omni-lint --features graphql) or drop .wasm modules in <config dir>/plugins/."
        );
    }

    // Config validation: unknown rule ids and sections for unloaded plugins
    // are configuration errors (exit 2), matching --list-rules output.
    for id in cfg.rules.keys() {
        let known = registry.rule_meta().iter().any(|m| m.id == id);
        if !known {
            return Err((
                format!(
                    "unknown rule \"{id}\" in config; run `omni-lint list-rules` for the registered set"
                ),
                ExitCode::Config,
            ));
        }
    }
    for (plugin, _) in &cfg.plugins {
        let loaded = registry
            .plugins
            .iter()
            .any(|p| p.id() == plugin.as_str());
        if !loaded {
            eprintln!(
                "warning: config references plugin \"{plugin}\" but no such plugin is loaded (not a builtin; missing from plugins/?)"
            );
        }
    }

    let result = run_check(root, &cfg, &registry).map_err(|e| (e, ExitCode::Config))?;

    if format == OutputFormat::Json {
        let sources = sources_map(root, &cfg, &registry)?;
        let stdout = std::io::stdout();
        let mut w = stdout.lock();
        write_json(&mut w, result.files, &result.findings, &sources, &result.totals)
            .map_err(|e| (e, ExitCode::Internal))?;
    } else {
        let sources = sources_map(root, &cfg, &registry)?;
        let stdout = std::io::stdout();
        let mut w = stdout.lock();
        for r in &result.findings {
            let source = sources
                .get(&r.diag.source_id)
                .ok_or(("internal: unknown source id".to_string(), ExitCode::Internal))?;
            write!(w, "{}", format_text(r, source)).ok();
        }
        let files_word = if result.files == 1 { "file" } else { "files" };
        let findings_word = if result.findings.len() == 1 && result.baselined == 0 {
            "finding"
        } else {
            "findings"
        };
        writeln!(
            w,
            "{}: {} {}, {} baselined.",
            result.files,
            result.findings.len(),
            findings_word,
            result.baselined
        )
        .ok();
        let _ = files_word;
    }

    if result.findings.is_empty() {
        Ok(ExitCode::Clean)
    } else if let Some(limit) = max_warnings {
        let errors = result.totals.get(&omni_core::Severity::Error).copied().unwrap_or(0);
        let warnings = result.totals.get(&omni_core::Severity::Warning).copied().unwrap_or(0);
        if errors == 0 && warnings <= limit {
            Ok(ExitCode::Clean)
        } else {
            Ok(ExitCode::Findings)
        }
    } else {
        Ok(ExitCode::Findings)
    }
}

fn sources_map(
    root: &Path,
    cfg: &Config,
    registry: &Registry,
) -> Result<omni_core::SourceMap, (String, ExitCode)> {
    let allowed: Option<Vec<String>> = if cfg.extensions.is_some() {
        None
    } else {
        Some(
            registry
                .plugins
                .iter()
                .flat_map(|p| p.extensions().iter().map(|e| e.to_string()))
                .collect(),
        )
    };
    let mut map = omni_core::SourceMap::new();
    let sources = omni_core::discovery::discover(root, cfg, allowed.as_deref())
        .map_err(|e| (e, ExitCode::Config))?;
    for s in sources {
        map.insert(s.id, s);
    }
    Ok(map)
}

/// `omni-lint todo generate`: record every current finding in the baseline;
/// regenerating replaces the file wholesale (regenerate semantics).
fn todo_generate(root: &Path, pair_mode: bool) -> Result<ExitCode, (String, ExitCode)> {
    let cfg = config_for(root)?;
    let baseline_path = cfg
        .baseline
        .clone()
        .unwrap_or_else(|| cfg.dir.join("omni-lint-baseline.json"));
    let registry = registry_for(Some(&cfg));
    let entries =
        omni_core::runner::collect_all_for_baseline(root, &cfg, &registry)
            .map_err(|e| (e, ExitCode::Config))?;
    let baseline = if pair_mode {
        // Ratchet granularity: (rule, path) -> count. Suppress exactly the
        // recorded backlog; over-baseline growth re-flags.
        let mut counts: std::collections::BTreeMap<(String, String), u64> = Default::default();
        for e in &entries {
            *counts
                .entry((e.rule_id.clone(), e.path.clone()))
                .or_insert(0) += 1;
        }
        Baseline {
            generated_at: Some(baseline::now_iso()),
            entries: Vec::new(),
            pairs: counts
                .into_iter()
                .map(|((rule_id, path), count)| baseline::BaselinePair {
                    rule_id,
                    path,
                    count,
                })
                .collect(),
        }
    } else {
        Baseline {
            generated_at: Some(baseline::now_iso()),
            entries,
            pairs: Vec::new(),
        }
    };
    let findings_count = baseline.entries.len()
        + baseline.pairs.iter().map(|p| p.count as usize).sum::<usize>();
    baseline
        .save(&baseline_path)
        .map_err(|e| (e, ExitCode::Internal))?;
    println!(
        "recorded {} findings {} in {}",
        findings_count,
        if pair_mode { "as (rule, path) pairs" } else { "with per-finding identity" },
        baseline_path.display()
    );
    Ok(ExitCode::Clean)
}

/// `omni-lint todo prune`: drop baseline entries whose finding no longer
/// exists in the current tree.
fn todo_prune(root: &Path) -> Result<ExitCode, (String, ExitCode)> {
    let cfg = config_for(root)?;
    let baseline_path = cfg
        .baseline
        .clone()
        .unwrap_or_else(|| cfg.dir.join("omni-lint-baseline.json"));
    if !baseline_path.is_file() {
        return Err((
            format!("no baseline at {}", baseline_path.display()),
            ExitCode::Config,
        ));
    }
    let baseline = Baseline::load(&baseline_path).map_err(|e| (e, ExitCode::Internal))?;
    let registry = registry_for(Some(&cfg));
    // Generate fresh entries without applying the old baseline, so every
    // live finding is visible for the prune comparison.
    let fresh = omni_core::runner::collect_all_for_baseline(
        root,
        &Config {
            baseline: None,
            ..cfg.clone()
        },
        &registry,
    )
    .map_err(|e| (e, ExitCode::Config))?;
    let mut live = std::collections::BTreeSet::new();
    for e in fresh {
        live.insert((e.rule_id, e.path, e.subject, e.fingerprint));
    }
    let (pruned, removed) = omni_core::runner::prune(baseline, &live);
    pruned
        .save(&baseline_path)
        .map_err(|e| (e, ExitCode::Internal))?;
    println!("pruned {removed} stale baseline entr{}", if removed == 1 { "y" } else { "ies" });
    Ok(ExitCode::Clean)
}

fn print_help() {
    println!(
        "omni-lint {} — a framework for building fast, language-agnostic linters
plugins: examples opt-in via --features graphql; loaded lazily from plugins/*.wasm

USAGE:
    omni-lint [COMMAND] [ROOT] [--format text|json] [--max-warnings N]

COMMANDS:
    check          lint ROOT (default: .); exits 1 when fixes/findings are new
    todo generate  record current findings in the baseline
                   (--pairs: one (rule, path, count) budget per pair)
    todo prune     drop baseline entries for findings that no longer exist
    list-rules     list all registered rules

CONFIG:
    omni-lint.toml at the lint root; see README for its schema",
        env!("CARGO_PKG_VERSION")
    );
}
