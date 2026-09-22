//! Run orchestration: discover -> parse (parallel, once per file) ->
//! negotiate capabilities -> run rules -> classify against baseline -> report.

use crate::baseline::{Baseline, BaselineEntry, BaselineMatch, Reportable};
use crate::config::{Config, RuleDirective};
use crate::diagnostic::{Diagnostic, SourceId, Span};
use crate::plugin::{CapabilityScope, ParsedFile, Plugin};
use crate::registry::Registry;
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Clean = 0,
    Findings = 1,
    Config = 2,
    Internal = 3,
}

#[derive(Debug)]
pub struct RunResult {
    /// Findings not covered by the baseline.
    pub findings: Vec<Reportable>,
    /// Parse errors (always reported as findings, never baselined).
    pub parse_errors: Vec<Reportable>,
    pub files: usize,
    /// Findings covered by the baseline.
    pub baselined: usize,
    pub totals: BTreeMap<crate::Severity, usize>,
}

/// Bounded, stable fingerprint for baseline identity: subject identity when
/// present, else a hash of the diagnostic's span text. Line shifts survive;
/// text edits that change the fingerprint correctly invalidate the entry.
pub fn fingerprint(source: &crate::SourceFile, span: Span, subject: Option<&str>) -> String {
    let mut input = String::new();
    input.push_str(subject.unwrap_or(""));
    input.push('|');
    let text: String = source.span_text(span).chars().take(96).collect();
    input.push_str(&text);
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in input.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

pub fn registry_allowed_extensions(registry: &Registry) -> Option<Vec<String>> {
    Some(
        registry
            .plugins
            .iter()
            .flat_map(|p| p.extensions().iter().map(|e| e.to_string()))
            .collect(),
    )
}

/// Full check run. Parse failures are surfaced as `parse/<plugin>` findings.
pub fn run_check(root: &Path, config: &Config, registry: &Registry) -> Result<RunResult, String> {
    // Restrict discovery to extensions a loaded plugin claims, unless the
    // config already narrows filters explicitly.
    let allowed: Option<Vec<String>> = if config.extensions.is_some() {
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
    let sources = crate::discovery::discover(root, config, allowed.as_deref())?;

    let files = sources.len();
    if files == 0 {
        return Ok(empty_result());
    }

    // Select plugins actually needed.
    let mut chosen: Vec<Arc<dyn Plugin>> = Vec::new();
    for src in &sources {
        if let Some(ext) = src.extension() {
            if let Some(p) = registry.plugin_by_extension(ext.as_str(), config.prefer.as_deref()) {
                if !chosen.iter().any(|c| c.id() == p.id()) {
                    chosen.push(Arc::clone(p));
                }
            }
        }
    }

    let sources_by_id: BTreeMap<SourceId, Arc<crate::SourceFile>> =
        sources.iter().map(|s| (s.id, Arc::clone(s))).collect();

    // Parse once per file, in parallel; deterministic order below.
    let plugin_by_ext: BTreeMap<String, Arc<dyn Plugin>> = chosen
        .iter()
        .flat_map(|p| p.extensions().iter().map(|e| (e.to_string(), Arc::clone(p))))
        .collect();
    let mut parsed: Vec<ParsedFile> = sources
        .par_iter()
        .map(|src| {
            let ext = src.extension().unwrap_or_default();
            let plugin = plugin_by_ext
                .get(ext.as_str())
                .expect("discovery guaranteed a plugin for this extension")
                .clone();
            plugin.parse_file(Arc::clone(src))
        })
        .collect();
    parsed.sort_by_key(|p| p.source.path.clone());

    // Baseline must be loaded before classification, but affects nothing upstream.
    let baseline: Option<Baseline> = match &config.baseline {
        Some(path) => {
            if path.is_file() {
                Some(Baseline::load(path)?)
            } else {
                None
            }
        }
        None => None,
    };

    // Build workspace capabilities only when some enabled rule needs them.
    let needed_caps: HashSet<String> = registry
        .rules()
        .iter()
        .filter(|r| config.rules.get(r.meta().id) != Some(&RuleDirective::Off))
        .filter(|r| !r.meta().requires.is_empty())
        .map(|r| r.meta().requires.to_string())
        .collect();

    let mut workspace: BTreeMap<String, Arc<dyn std::any::Any + Send + Sync>> = BTreeMap::new();
    for plugin in &chosen {
        let caps = plugin.capabilities();
        let wants_workspace = caps
            .iter()
            .any(|c| c.scope == CapabilityScope::Workspace && needed_caps.contains(&c.name.to_string()));
        if !wants_workspace {
            continue;
        }
        let plugin_config: BTreeMap<String, toml::Value> = config.plugins
            .get(plugin.id())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let built = plugin.build_workspace(&parsed, &plugin_config)?;

        workspace.extend(built);
    }
    // A rule may only need file-scoped capabilities; those live per ParsedFile,
    // so the presence check is over what plugins declare in total.
    let mut available_caps: HashSet<String> = workspace.keys().cloned().collect();
    for p in &chosen {
        for c in p.capabilities() {
            if c.scope == CapabilityScope::File {
                available_caps.insert(c.name.to_string());
            }
        }
    }

    // Effective severities.
    let severity_by_rule: BTreeMap<String, crate::Severity> = registry
        .rules()
        .iter()
        .map(|r| {
            let meta = r.meta();
            let sev = match config.rules.get(meta.id) {
                Some(RuleDirective::On(Some(sev))) => *sev,
                _ => meta.default_severity,
            };
            (meta.id.to_string(), sev)
        })
        .collect();

    // Run rules (sequentially: rules are cheap and diagnostics are the cost).
    let collected: Mutex<Vec<Diagnostic>> = Mutex::new(Vec::new());
    for rule in registry.rules() {
        let meta = rule.meta();
        if config.rules.get(meta.id) == Some(&RuleDirective::Off) {
            continue;
        }
        if !meta.requires.is_empty() && !available_caps.contains(meta.requires) {
            continue;
        }
        let emit = |diag: Diagnostic| {
            if diag.rule_id != meta.id {
                return; // guard against a rule emitting a foreign rule id
            }
            collected.lock().unwrap().push(diag);
        };
        let ctx = crate::plugin::RuleContext::new(&parsed, &workspace, &emit);
        rule.run(&ctx);
    }
    let mut diagnostics: Vec<Diagnostic> = collected.into_inner().unwrap();

    // Plugin-reported file-scope findings (WASM plugins run their file rules
    // inside parse). Config severity overrides apply when the rule is known.
    for p in &parsed {
        for mut f in p.findings.clone() {
            if config.rules.get(&f.rule_id) == Some(&RuleDirective::Off) {
                continue;
            }
            if let Some(sev) = severity_by_rule.get(&f.rule_id) {
                f.severity = *sev;
            }
            diagnostics.push(f);
        }
    }

    // Plugin-specific workspace rules (WASM). Findings carry relative paths;
    // resolve them against discovered sources.
    let path_to_id: BTreeMap<String, SourceId> = sources
        .iter()
        .map(|s| (s.path.display().to_string(), s.id))
        .collect();
    // Plugins that declare workspace capabilities get their workspace pass
    // invoked; guests consult the enabled-rules envelope to prune themselves.
    let workspace_wanted: HashSet<String> = chosen
        .iter()
        .filter(|p| p.capabilities().iter().any(|c| c.scope == CapabilityScope::Workspace))
        .map(|p| p.id().to_string())
        .collect();
    for plugin in &chosen {
        if !workspace_wanted.contains(plugin.id()) {
            continue;
        }
        for pd in plugin.run_workspace_rules(&parsed) {
            if config.rules.get(&pd.diag.rule_id) == Some(&RuleDirective::Off) {
                continue;
            }
            let source_id = path_to_id.get(&pd.path).copied().unwrap_or_else(|| {
                // Unknown path: use the first source so the location still
                // renders; the path stays visible in the message.
                sources.first().map(|s| s.id).unwrap_or(SourceId(0))
            });
            let mut d = pd.diag.into_diagnostic(source_id);
            if pd.path.is_empty() {
                d.severity = crate::Severity::Error;
            }
            if let Some(sev) = severity_by_rule.get(&d.rule_id) {
                d.severity = *sev;
            }
            diagnostics.push(d);
        }
    }

    // Parse errors: report under their own ids, always error severity,
    // and skip severity overrides (they are infrastructure faults).
    for p in &parsed {
        for mut e in p.errors.clone() {
            e.severity = crate::Severity::Error;
            diagnostics.push(e);
        }
    }

    // Map to Reportable and apply severity overrides.
    let mut reportables: Vec<Reportable> = diagnostics
        .into_iter()
        .map(|mut d| {
            d.severity = severity_by_rule.get(&d.rule_id).copied().unwrap_or(d.severity);
            let source = sources_by_id.get(&d.source_id).expect("source for diag");
            Reportable {
                located: source.locate(d.span),
                path: source.path.display().to_string(),
                diag: d,
            }
        })
        .collect();
    reportables.sort_by(|a, b| {
        (&a.path, a.located.line, a.located.column, &a.diag.rule_id)
            .cmp(&(&b.path, b.located.line, b.located.column, &b.diag.rule_id))
    });

    // Baseline classification: subject/fingerprint identity match.
    let mut new_findings = Vec::new();
    let mut baselined = 0usize;
    let cloned_baseline = baseline.clone();
    for r in reportables {
        if let Some(b) = &cloned_baseline {
            let fp = fingerprint(
                sources_by_id.get(&r.diag.source_id).unwrap(),
                r.diag.span,
                r.diag.subject.as_deref(),
            );
            if let BaselineMatch::Baselined(_) = b.classify(&r.diag, &r.path, &fp) {
                baselined += 1;
                continue;
            }
        }
        new_findings.push(r);
    }

    let mut totals: BTreeMap<crate::Severity, usize> = BTreeMap::new();
    for r in &new_findings {
        *totals.entry(r.diag.severity).or_insert(0) += 1;
    }

    Ok(RunResult {
        findings: new_findings,
        parse_errors: vec![], // already folded into findings under parse/*
        files,
        baselined,
        totals,
    })
}

fn empty_result() -> RunResult {
    RunResult {
        findings: vec![],
        parse_errors: vec![],
        files: 0,
        baselined: 0,
        totals: BTreeMap::new(),
    }
}

/// All findings (rule findings + parse errors) with fingerprint inputs, used
/// by `todo generate`. Includes baselined findings so a regenerate is complete.
pub fn collect_all_for_baseline(
    root: &Path,
    config: &Config,
    registry: &Registry,
) -> Result<Vec<BaselineEntry>, String> {
    let generated = run_check(root, &Config { baseline: None, ..config.clone() }, registry)?;

    let sources = crate::discovery::discover(
        root,
        config,
        None,
    )?;

    let sources_by_id: BTreeMap<SourceId, Arc<crate::SourceFile>> =
        sources.iter().map(|s| (s.id, Arc::clone(s))).collect();
    let mut out = Vec::new();
    for r in &generated.findings {
        let source = sources_by_id.get(&r.diag.source_id).expect("source");
        let fp = fingerprint(source, r.diag.span, r.diag.subject.as_deref());
        out.push(BaselineEntry {
            rule_id: r.diag.rule_id.clone(),
            path: r.path.clone(),
            subject: r.diag.subject.clone(),
            line: r.located.line,
            fingerprint: fp,
        });
    }
    out.sort_by(|a, b| {
        (&a.path, a.line, &a.rule_id).cmp(&(&b.path, b.line, &b.rule_id))
    });
    Ok(out)
}

/// Prune: drop baseline entries no longer present in the tree. `live` holds
/// (rule_id, path, subject, fingerprint) of every current finding
/// (from `collect_all_for_baseline`, which ignores the baseline).
pub fn prune(baseline: Baseline, live: &BTreeSet<(String, String, Option<String>, String)>) -> (Baseline, usize) {
    let total = baseline.entries.len();
    let mut kept = Baseline {
        generated_at: baseline.generated_at.clone(),
        entries: Vec::new(),
    };
    for e in baseline.entries.iter().cloned() {
        let key = (
            e.rule_id.clone(),
            e.path.clone(),
            e.subject.clone(),
            e.fingerprint.clone(),
        );
        if live.contains(&key) {
            kept.entries.push(e);
        }
    }
    let removed = total - kept.entries.len();
    (kept, removed)
}
