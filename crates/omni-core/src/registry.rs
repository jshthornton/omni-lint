//! Plugin / rule registry.

use crate::plugin::{Plugin, Rule, RuleMeta};
use crate::Severity;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Registry of engine-loaded plugins and their rules.
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

    pub fn register(&mut self, plugin: Arc<dyn Plugin>, rules: Vec<Arc<dyn Rule>>) {
        self.plugins.push(plugin);
        self.rules.extend(rules);
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

    /// Rules grouped by the plugin that owns their namespace prefix.
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
