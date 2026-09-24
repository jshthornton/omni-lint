//! Rule pack modules: one `.wasm` ships a bundle of rules — the eslint /
//! rubocop / credo packaging model.
//!
//! An app author drops a pack module into `<config dir>/rules/`, which
//! *extends* the app's ruleset with all of the pack's rules; each rule inside
//! is then enabled/disabled/re-graded/configured individually in
//! `omni-lint.toml`, exactly like an eslint rule. Rules graded `off` in the
//! pack are registered but only run once the user opts in (bundles layer:
//! pack `recommended` vs full set is just how the pack grades its rules).
//!
//! A module is either:
//!
//!  - a **pack**: exports `omni_pack_meta` +
//!      `{ "namespace": "acme", "name": "...", "requires": "graphql.facts",
//!         "rules": [{"id": "acme/no-empty-type", "description": "...",
//!                    "severity": "error"}, ...] }`
//!  - a **single-rule module** (back-compat, also a fine "first rule" shape):
//!      exports `omni_rule_meta` + `{ "id": "acme/no-empty-type", ... }`
//!
//! `omni_rule_run(ptr, len)` is the single entry point for both: the host
//! passes the run envelope
//! `{"language", "files": [{path, source, facts}], "workspace", "options",
//!   "rules": [{"id", "options"?}]}` —
//! where `rules` is the subset this invocation concerns itself with, with
//! each rule's own options from `[rules."<id>".options]`. Findings carry a
//! `rule_id` (default: the first envelope rule) and are filtered to that
//! subset host-side.

use crate::session::{
    clamp_span, engine, leak_str, load_module, normalize_limits, span_of, GuestFinding, Session,
    WasmLimits, MAX_REPORTED_FINDINGS,
};
use omni_core::plugin::{Rule, RuleContext, RuleMeta};
use omni_core::{Diagnostic, Severity, SourceId, Span};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Rule pack metadata (`omni_pack_meta`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RulePackMeta {
    /// Namespace of every rule in the pack, e.g. `acme` / `demo-gql`.
    pub namespace: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Facts capability the whole pack reads: `<language>.facts` or
    /// `<language>.workspace`.
    pub requires: String,
    /// Every rule the pack contributes, graded by the pack author (an
    /// extension bundle's `recommended` set, essentially).
    #[serde(default)]
    pub rules: Vec<PackRuleDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackRuleDef {
    /// Full rule id, `<namespace>/<rule-name>` — this is what config keys.
    pub id: String,
    pub description: String,
    /// Pack-author default ("error" | "warning" | "info"; "off" = opt-in).
    #[serde(default)]
    pub severity: Option<String>,
}

/// Single-rule module metadata (`omni_rule_meta` back-compat).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleModuleMeta {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub severity: Option<String>,
    pub requires: String,
}

/// One finding, returned as a JSON array from `omni_rule_run` and/or
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

fn valid_pack_namespace(ns: &str) -> bool {
    !ns.is_empty()
        && ns.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && ns.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_rule_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A validated, loaded rule pack module (bundle, possibly of one rule).
pub struct WasmRulePack {
    engine: wasmtime::Engine,
    module: wasmtime::Module,
    /// `[u32][json]` metadata export name used (`omni_pack_meta` or
    /// `omni_rule_meta`).
    meta_export: &'static str,
    pub pack_meta: RulePackMeta,
    /// Facts capability of the target language (e.g. `graphql.facts`).
    pub facts_cap: String,
    /// Cross-file capability of the target language (e.g. `graphql.workspace`).
    pub workspace_cap: String,
    limits: WasmLimits,
    pub module_path: PathBuf,
}

impl WasmRulePack {
    pub fn load(path: &Path, raw_limits: WasmLimits) -> Result<WasmRulePack, String> {
        let limits = normalize_limits(raw_limits);
        let engine = engine()?;

        // Load first with the shared exports, then decide the meta shape
        // from what the module exports.
        let module = load_module(&engine, path, &["memory", "omni_alloc", "omni_rule_run"])?;
        let has_pack_meta = module.exports().any(|e| e.name() == "omni_pack_meta");
        let has_single = module.exports().any(|e| e.name() == "omni_rule_meta");
        let meta_export: &'static str = if has_pack_meta {
            "omni_pack_meta"
        } else if has_single {
            "omni_rule_meta"
        } else {
            return Err(format!(
                "rule pack `{}` exports neither `omni_pack_meta` (bundle) nor `omni_rule_meta` (single rule)",
                path.display()
            ));
        };

        // Probe metadata through a real instantiation so load-time validation
        // runs guest code once.
        // Probe metadata through a real instantiation so load-time validation
        // runs guest code once.
        let meta_json = {
            let mut session = Session::open(&engine, &module, &limits)
                .map_err(|e| format!("instantiate {}: {e}", path.display()))?;
            session
                .call_ret_json(meta_export, &[])
                .map_err(|e| format!("probe {}: {e}", path.display()))?
        };
        let single: Option<RuleModuleMeta> = if has_pack_meta {
            None
        } else {
            Some(serde_json::from_slice(&meta_json)
                .map_err(|e| format!("rule module `{}` meta malformed: {e}", path.display()))?)
        };
        let pack_meta = match &single {
            Some(s) => single_to_pack(s),
            None => serde_json::from_slice::<RulePackMeta>(&meta_json)
                .map_err(|e| format!("rule pack `{}` meta malformed: {e}", path.display()))?,
        };

        // Language / capability negotiation.
        let lang = pack_meta
            .requires
            .split('.')
            .next()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let lang = match lang {
            Some(l) => l,
            None => {
                return Err(format!(
                    "rule pack `{}` has invalid `requires` {:?} (use `<language>.facts` or `<language>.workspace`)",
                    path.display(),
                    pack_meta.requires
                ))
            }
        };
        if pack_meta.requires != format!("{lang}.facts")
            && pack_meta.requires != format!("{lang}.workspace")
        {
            return Err(format!(
                "rule pack `{}` has invalid `requires` {:?} (use `<language>.facts` or `<language>.workspace`)",
                path.display(),
                pack_meta.requires
            ));
        }

        // Bundle validation: namespace + at least one rule, ids all in it.
        if !valid_pack_namespace(&pack_meta.namespace) {
            return Err(format!(
                "rule pack `{}` declares an invalid namespace {:?} (lowercase kebab)",
                path.display(),
                pack_meta.namespace
            ));
        }
        if pack_meta.rules.is_empty() {
            return Err(format!(
                "rule pack `{}` contributes no rules",
                path.display()
            ));
        }
        for rule in &pack_meta.rules {
            let name = rule.id.split_once('/').map(|(ns, n)| (ns == pack_meta.namespace, n));
            let ok = matches!(name, Some((true, n)) if valid_rule_name(n));
            if !ok {
                return Err(format!(
                    "rule pack `{}` contributes invalid rule id {:?} (ids must be named under the pack namespace `{}`, lowercase kebab)",
                    path.display(),
                    rule.id,
                    pack_meta.namespace
                ));
            }
        }

        Ok(WasmRulePack {
            engine,
            module,
            meta_export,
            pack_meta,
            facts_cap: format!("{lang}.facts"),
            workspace_cap: format!("{lang}.workspace"),
            limits,
            module_path: path.to_path_buf(),
        })
    }

    /// Discover every `.wasm` rule pack under `dir`. Returns loaded packs
    /// plus (path, error) for modules that failed validation (reported and
    /// skipped by the caller, never fatal).
    pub fn discover(
        dir: &Path,
        limits: WasmLimits,
    ) -> (Vec<Arc<WasmRulePack>>, Vec<(PathBuf, String)>) {
        let mut packs = Vec::new();
        let mut errors = Vec::new();
        let mut paths: Vec<PathBuf> = super::wasm_paths(dir);
        paths.sort();
        for path in paths {
            match WasmRulePack::load(&path, limits.clone()) {
                Ok(p) => packs.push(Arc::new(p)),
                Err(e) => errors.push((path, e)),
            }
        }
        (packs, errors)
    }

    pub fn namespace(&self) -> &str {
        &self.pack_meta.namespace
    }

    pub fn meta_export(&self) -> &'static str {
        self.meta_export
    }

    fn open_session(&self) -> Result<Session, String> {
        Session::open(&self.engine, &self.module, &self.limits)
    }

    /// Raw rule call for debugging: run an envelope through the guest and get
    /// its findings JSON text back.
    pub fn call_run_dbg(&self, envelope: &str) -> Result<String, String> {
        let mut session = self.open_session()?;
        let (ptr, len) = session.write_bytes(envelope.as_bytes())?;
        let ret = session.call_ret_json("omni_rule_run", &[(ptr, len)])?;
        String::from_utf8(ret).map_err(|_| String::from("malformed UTF-8 rule findings"))
    }

    /// (inline findings, streamed findings) for one adapter invocation.
    fn run_raw(&self, envelope: &str) -> Result<(Vec<RuleFinding>, Vec<RuleFinding>), String> {
        let mut session = self.open_session()?;
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

fn single_to_pack(single: &RuleModuleMeta) -> RulePackMeta {
    let (ns, _) = single.id.split_once('/').unwrap_or((single.id.as_str(), ""));
    RulePackMeta {
        namespace: ns.to_string(),
        name: None,
        description: Some(single.description.clone()),
        requires: single.requires.clone(),
        rules: vec![PackRuleDef {
            id: single.id.clone(),
            description: single.description.clone(),
            severity: single.severity.clone(),
        }],
    }
}

// ---------------------------------------------------------------------------
// Per-rule adapter: every rule of the pack is registered into the ruleset
// individually, so config keys it exactly like a native rule.
// ---------------------------------------------------------------------------

/// One rule contributed by a pack module, exposed through `omni_core::Rule`.
/// The adapter's invocation concerns only its own rule; the module loops over
/// `envelope.rules` (typically one entry).
pub struct WasmRule {
    pack: Arc<WasmRulePack>,
    rule_meta: RuleMeta,
    id: String,
}

impl WasmRule {
    /// Wrap `rule` of `pack` as a ruleset member.
    pub fn new(pack: Arc<WasmRulePack>, rule: &PackRuleDef) -> WasmRule {
        WasmRule {
            pack: Arc::clone(&pack),
            rule_meta: RuleMeta {
                id: leak_str(&rule.id),
                description: leak_str(&rule.description),
                default_severity: rule
                    .severity
                    .as_deref()
                    .and_then(Severity::parse)
                    .unwrap_or(Severity::Warning),
                requires: leak_str(&pack.pack_meta.requires),
            },
            id: rule.id.clone(),
        }
    }
}

impl Rule for WasmRule {
    fn meta(&self) -> RuleMeta {
        self.rule_meta.clone()
    }

    fn run(&self, ctx: &RuleContext) {
        // The pack reads per-file facts + workspace model + this rule's
        // options (from `[rules."<id>".options]`).
        let files: Vec<Value> = ctx
            .parsed
            .iter()
            .filter_map(|f| {
                ctx.file_facts(f, &self.pack.facts_cap).map(|facts| {
                    json!({
                        "path": f.source.path.display().to_string(),
                        "source": f.source.text(),
                        "facts": facts,
                    })
                })
            })
            .collect();
        let file_count = files.len();
        if file_count == 0 {
            return;
        }
        let only_path = if file_count == 1 {
            files[0]["path"].as_str().map(|s| s.to_string())
        } else {
            None
        };
        let fallback_id = ctx.parsed.first().map(|f| f.source.id).unwrap_or(SourceId(0));

        // What this invocation should produce: only this rule.
        let rules = vec![json!({
            "id": self.id,
            "options": ctx.options.clone().unwrap_or(Value::Null),
        })];
        let workspace = ctx
            .workspace_facts(&self.pack.workspace_cap)
            .cloned()
            .unwrap_or(Value::Null);
        let envelope = json!({
            "language": self.pack.facts_cap.split('.').next().unwrap_or(""),
            "files": files,
            "workspace": workspace,
            // top-level convenience for single-rule guests; per-rule entries
            // in `rules` are authoritative when a pack batches.
            "options": rules[0]["options"].clone(),
            "rules": rules,
        });

        let findings = match self.pack.run_raw(&envelope.to_string()) {
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
                        "rule pack failed: {e} (module: {})",
                        self.pack.module_path.display()
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
            // Only this invocation's rule id may report (default: its own id).
            let reported_id = f
                .rule_id
                .clone()
                .unwrap_or_else(|| self.id.clone());
            if reported_id != self.id {
                continue;
            }
            let severity = f
                .severity
                .as_deref()
                .and_then(Severity::parse)
                .filter(|s| *s != Severity::Off)
                .unwrap_or(default_severity);
            // Default severity resolution (including off-by-default rules
            // opting in) is the engine's job in severity_by_rule — the
            // adapter only honours explicit per-finding severities.
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
                                self.pack.module_path.display()
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