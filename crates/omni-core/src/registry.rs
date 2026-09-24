//! Language plugin / rule registry.
//!
//! Plugins and rules register independently: a language plugin is an AST
//! provider, and rules join the ruleset à la carte (one native impl or one
//! WASM module at a time), like dropping a rule into eslint's `rules` map.

use crate::plugin::{Plugin, Rule, RuleMeta};
use crate::Severity;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Registry of engine-loaded language plugins and the assembled ruleset.
pub struct Registry {
    pub plugins: Vec<Arc<dyn Plugin>>,
    rules: Vec<Arc<dyn Rule>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry {
            plugins: Vec::new(),
            rules: Vec::new(),
        }
    }

    /// Register a language plugin (AST provider). Plugins never bundle rules.
    pub fn register_plugin(&mut self, plugin: Arc<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    /// Register one rule into the ruleset. Ids are unique across every origin
    /// (native crates, example packs, third-party WASM modules): a clash is a
    /// load error, never silent shadowing.
    pub fn register_rule(&mut self, rule: Arc<dyn Rule>) -> Result<(), String> {
        let id = rule.meta().id;
        if id.is_empty() || !id.contains('/') {
            return Err(format!(
                "rule id {id:?} is invalid: use `namespace/rule-name` (lowercase kebab)"
            ));
        }
        if self.rules.iter().any(|r| r.meta().id == id) {
            return Err(format!(
                "rule `{id}` is already registered; two rules from different origins cannot share an id"
            ));
        }
        self.rules.push(rule);
        Ok(())
    }

    /// Register many rules (e.g. one rule pack's built-ins), returning every
    /// rejected id with its reason.
    pub fn register_rules(
        &mut self,
        rules: impl IntoIterator<Item = Arc<dyn Rule>>,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        for rule in rules {
            if let Err(e) = self.register_rule(rule) {
                errors.push(e);
            }
        }
        errors
    }

    pub fn rules(&self) -> &[Arc<dyn Rule>] {
        &self.rules
    }

    /// All rule metadata, for `--list-rules` output.
    pub fn rule_meta(&self) -> Vec<RuleMeta> {
        self.rules.iter().map(|r| r.meta()).collect()
    }

    pub fn plugin_by_extension(
        &self,
        ext: &str,
        prefer: Option<&str>,
    ) -> Option<&Arc<dyn Plugin>> {
        if let Some(pref_id) = prefer {
            if let Some(p) = self.plugins.iter().find(|p| {
                p.id() == pref_id && p.extensions().contains(&ext)
            }) {
                return Some(p);
            }
        }
        self.plugins.iter().find(|p| p.extensions().contains(&ext))
    }

    /// Rules grouped by their id namespace (e.g. `graphql/...`).
    pub fn rules_by_plugin(&self) -> BTreeMap<String, Vec<Arc<dyn Rule>>> {
        let mut out: BTreeMap<String, Vec<Arc<dyn Rule>>> = BTreeMap::new();
        for r in &self.rules {
            let id = r.meta().id;
            let ns = id.split('/').next().unwrap_or("").to_string();
            out.entry(ns).or_default().push(Arc::clone(r));
        }
        out
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Effective severity of a rule after config override.
pub fn effective_severity(meta: &RuleMeta, severity_override: Option<Severity>) -> Option<Severity> {
    Some(severity_override.unwrap_or(meta.default_severity))
}
