//! Rule modules: exactly one rule per `.wasm`, registered into the ruleset à
//! la carte — the same authoring model as an eslint/credo/rubocop rule.
//!
//! A rule module declares its own id/description/severity and the facts
//! capability it reads, so any dev can write one in a few dozen lines, drop
//! it in `<config dir>/rules/`, and tune it in `omni-lint.toml` alongside
//! every other rule.

use crate::session::{
    clamp_span, engine, load_module, normalize_limits, span_of, GuestFinding, Session, WasmLimits,
    MAX_REPORTED_FINDINGS,
};
use omni_core::plugin::{Rule, RuleContext, RuleMeta};
use omni_core::{Diagnostic, Severity, SourceId, Span};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Rule module metadata (`omni_rule_meta`):
/// `{ "id": "acme/no-empty-type", "description": "...", "severity": "error",
///    "requires": "graphql.facts" }`
///
/// `requires` names the facts capability the rule reads:
///   - `<language>.facts` — per-file facts documents (`graphql.facts`, ...)
///   - `<language>.workspace` — cross-file facts model
/// Either way the run envelope carries every file's facts plus the workspace
/// view; the suffix is the capability negotiation hook (a rule needing the
/// cross-file model is only run when some language module provides one).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleModuleMeta {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub severity: Option<String>,
    pub requires: String,
}

/// One rule finding, returned as a JSON array from `omni_rule_run` and/or
/// streamed through `env.omni_report`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFinding {
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    pub message: String,
    #[serde(default)]
    pub span: [u32; 2],
    #[serde(default)]
    pub subject: Option<String>,
    /// Workspace-relative path of the offending file (one of the envelope's
    /// `files[].path` values).
    #[serde(default)]
    pub path: Option<String>,
}

/// A validated, loaded one-rule module.
pub struct WasmRule {
    engine: wasmtime::Engine,
    module: wasmtime::Module,
    /// Engine-facing metadata (strings leaked to `'static`, built once).
    rule_meta: RuleMeta,
    /// Facts capability of the target language (e.g. `graphql.facts`).
    facts_cap: String,
    /// Cross-file capability of the target language (e.g. `graphql.workspace`).
    workspace_cap: String,
    limits: WasmLimits,
    pub module_path: PathBuf,
    raw: RuleModuleMeta,
}

fn valid_rule_id(id: &str) -> bool {
    match id.split_once('/') {
        Some((ns, name)) => {
            !ns.is_empty()
                && !name.is_empty()
                && [ns, name].iter().all(|s| {
                    s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                        && s.chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                })
        }
        None => false,
    }
}

impl WasmRule {
    pub fn load(path: &Path, raw_limits: WasmLimits) -> Result<WasmRule, String> {
        let limits = normalize_limits(raw_limits);
        let engine = engine()?;
        let module = load_module(
            &engine,
            path,
            &["memory", "omni_alloc", "omni_rule_meta", "omni_rule_run"],
        )?;

        // Probe metadata through a real instantiation so load-time validation
        // runs guest code once.
        let raw: RuleModuleMeta = {
            let mut session = Session::open(&engine, &module, &limits)
                .map_err(|e| format!("instantiate {}: {e}", path.display()))?;
            let json = session
                .call_ret_json("omni_rule_meta", &[])
                .map_err(|e| format!("probe {}: {e}", path.display()))?;
            serde_json::from_slice(&json)
                .map_err(|e| format!("rule module `{}` meta malformed: {e}", path.display()))?
        };
        if !valid_rule_id(&raw.id) {
            return Err(format!(
                "rule module `{}` declares an invalid id {:?} (use `namespace/rule-name`, lowercase kebab)",
                path.display(),
                raw.id
            ));
        }
        let lang = raw
            .requires
            .split('.')
            .next()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let lang = match lang {
            Some(l) => l,
            None => {
                return Err(format!(
                    "rule module `{}` has invalid `requires` {:?} (use `<language>.facts` or `<language>.workspace`)",
                    path.display(),
                    raw.requires
                ))
            }
        };
        if raw.requires != format!("{lang}.facts") && raw.requires != format!("{lang}.workspace") {
            return Err(format!(
                "rule module `{}` has invalid `requires` {:?} (use `<language>.facts` or `<language>.workspace`)",
                path.display(),
                raw.requires
            ));
        }

        let rule_meta = RuleMeta {
            id: crate::session::leak_str(&raw.id),
            description: crate::session::leak_str(&raw.description),
            default_severity: raw
                .severity
                .as_deref()
                .and_then(Severity::parse)
                .unwrap_or(Severity::Warning),
            requires: crate::session::leak_str(&raw.requires),
        };

        Ok(WasmRule {
            engine,
            module,
            rule_meta,
            facts_cap: format!("{lang}.facts"),
            workspace_cap: format!("{lang}.workspace"),
            limits,
            module_path: path.to_path_buf(),
            raw,
        })
    }

    /// Discover every `.wasm` rule module under `dir`. Returns loaded rules
    /// plus (path, error) for modules that failed validation (reported and
    /// skipped by the caller, never fatal).
    pub fn discover(dir: &Path, limits: WasmLimits) -> (Vec<WasmRule>, Vec<(PathBuf, String)>) {
        let mut rules = Vec::new();
        let mut errors = Vec::new();
        let mut paths: Vec<PathBuf> = super::wasm_paths(dir);
        paths.sort();
        for path in paths {
            match WasmRule::load(&path, limits.clone()) {
                Ok(r) => rules.push(r),
                Err(e) => errors.push((path, e)),
            }
        }
        (rules, errors)
    }

    pub fn module_path(&self) -> &Path {
        &self.module_path
    }

    pub fn meta_raw(&self) -> &RuleModuleMeta {
        &self.raw
    }

    /// Raw rule call for debugging: run an envelope through the guest and get
    /// its findings JSON text back.
    pub fn call_run_dbg(&self, envelope: &str) -> Result<String, String> {
        let mut session = Session::open(&self.engine, &self.module, &self.limits)?;
        let (ptr, len) = session.write_bytes(envelope.as_bytes())?;
        let ret = session.call_ret_json("omni_rule_run", &[(ptr, len)])?;
        String::from_utf8(ret).map_err(|_| String::from("malformed UTF-8 rule findings"))
    }

    /// Build the run envelope: every parsed file carrying the language's
    /// per-file facts, plus the cross-file facts and this rule's options.
    fn envelope(&self, ctx: &RuleContext) -> Value {
        let files: Vec<Value> = ctx
            .parsed
            .iter()
            .filter_map(|f| {
                ctx.file_facts(f, &self.facts_cap).map(|facts| {
                    json!({
                        "path": f.source.path.display().to_string(),
                        "source": f.source.text(),
                        "facts": facts,
                    })
                })
            })
            .collect();
        let workspace = ctx
            .workspace_facts(&self.workspace_cap)
            .cloned()
            .unwrap_or(Value::Null);
        json!({
            "language": self.facts_cap.split('.').next().unwrap_or(""),
            "files": files,
            "workspace": workspace,
            "options": ctx.options.clone().unwrap_or(Value::Null),
        })
    }

    /// Call `omni_rule_run` once; returns (inline findings, omni_report
    /// findings).
    fn run_raw(&self, envelope: &str) -> Result<(Vec<RuleFinding>, Vec<RuleFinding>), String> {
        let mut session = Session::open(&self.engine, &self.module, &self.limits)?;
        let (ptr, len) = session.write_bytes(envelope.as_bytes())?;
        let ret = session.call_ret_json("omni_rule_run", &[(ptr, len)])?;
        let streamed: Vec<RuleFinding> = session
            .reported()
            .iter()
            .map(guest_to_rule_finding)
            .collect();
        let inline: Vec<RuleFinding> = if ret.is_empty() {
            Vec::new()
        } else {
            serde_json::from_slice::<Vec<RuleFinding>>(&ret)
                .map_err(|e| format!("malformed rule findings: {e}"))?
        };
        Ok((inline, streamed))
    }
}

impl Rule for WasmRule {
    fn meta(&self) -> RuleMeta {
        self.rule_meta.clone()
    }

    fn run(&self, ctx: &RuleContext) {
        let envelope = self.envelope(ctx);
        let file_count = envelope
            .get("files")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if file_count == 0 {
            return;
        }
        let only_path = if file_count == 1 {
            envelope["files"][0]["path"].as_str().map(|s| s.to_string())
        } else {
            None
        };
        let fallback_id = ctx.parsed.first().map(|f| f.source.id).unwrap_or(SourceId(0));

        let findings = match self.run_raw(&envelope.to_string()) {
            Ok((inline, streamed)) => {
                let mut all: Vec<RuleFinding> = streamed;
                all.extend(inline);
                all
            }
            Err(e) => {
                ctx.emit(Diagnostic {
                    rule_id: self.rule_meta.id.into(),
                    severity: Severity::Error,
                    message: format!(
                        "rule module failed: {e} (module: {})",
                        self.module_path.display()
                    ),
                    source_id: fallback_id,
                    span: Span::new(0, 0),
                    related: vec![],
                    subject: None,
                });
                return;
            }
        };

        let default_severity = self.rule_meta.default_severity;
        for f in findings.into_iter().take(MAX_REPORTED_FINDINGS) {
            // A rule may only report under its own id.
            let id_ok = f
                .rule_id
                .as_deref()
                .map(|id| id == self.rule_meta.id)
                .unwrap_or(true);
            if !id_ok {
                continue;
            }
            let severity = f
                .severity
                .as_deref()
                .and_then(Severity::parse)
                .unwrap_or(default_severity);
            let span = span_of(clamp_span(f.span));
            let path = f.path.clone().unwrap_or_default();
            let source_id = if path.is_empty() {
                only_path
                    .as_deref()
                    .and_then(|p| ctx.source_id_for_path(p))
                    .or(Some(fallback_id))
            } else {
                match ctx.source_id_for_path(&path) {
                    Some(id) => Some(id),
                    None => {
                        // Unknown path: a rule authoring bug — surface it,
                        // never silently mislocate the finding.
                        ctx.emit(Diagnostic {
                            rule_id: self.rule_meta.id.into(),
                            severity: Severity::Error,
                            message: format!(
                                "rule reported a finding for unknown path `{path}` (module: {})",
                                self.module_path.display()
                            ),
                            source_id: fallback_id,
                            span: Span::new(0, 0),
                            related: vec![],
                            subject: None,
                        });
                        continue;
                    }
                }
            };
            let Some(source_id) = source_id else {
                continue;
            };
            ctx.emit(Diagnostic {
                rule_id: self.rule_meta.id.into(),
                severity,
                message: f.message.clone(),
                source_id,
                span,
                related: vec![],
                subject: f.subject.clone(),
            });
        }
    }
}

fn guest_to_rule_finding(f: &GuestFinding) -> RuleFinding {
    RuleFinding {
        rule_id: f.rule_id.clone(),
        severity: f.severity.clone(),
        message: f.message.clone(),
        span: f.span,
        subject: f.subject.clone(),
        path: f.path.clone(),
    }
}