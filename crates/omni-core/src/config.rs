//! Configuration loading.
//!
//! `omni-lint.toml` at the lint root (config dir) configures the run:
//!
//! ```toml
//! [rules."graphql/no-empty-type"]
//! enabled = true          # or false, or a severity string like "warn"
//! severity = "error"      # optional severity override
//!
//! [targets]
//! extensions = ["graphql", "gql"]   # optional: narrow which extensions are linted
//!
//! ignore = ["generated/**"]         # glob patterns relative to the config dir
//! baseline = "omni-lint-baseline.json"  # optional TODO baseline file
//!
//! [plugins.graphql]     # per-plugin extra options, passed verbatim
//! strict_mode = true
//! ```

use serde::de::IgnoredAny;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "omni-lint.toml";

/// A per-rule directive parsed from config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleDirective {
    /// Rule disabled.
    Off,
    /// Rule enabled, optionally with a severity override.
    On(Option<crate::Severity>),
}

#[derive(Debug, Default, Clone)]
pub struct Config {
    /// Path of the config file (if any).
    pub path: Option<PathBuf>,
    /// Directory containing the config file; relative paths resolve here.
    pub dir: PathBuf,
    pub rules: BTreeMap<String, RuleDirective>,
    /// Restrict linting to these extensions (lowercase, no dot).
    pub extensions: Option<Vec<String>>,
    /// Glob ignore patterns relative to config dir.
    pub ignore: Vec<String>,
    /// Baseline JSON path relative to config dir.
    pub baseline: Option<PathBuf>,
    /// Plugin id preferred for shared extensions (e.g. `"demo-gql"` claims
    /// `.graphql` too — prefering it routes those files to the WASM plugin).
    pub prefer: Option<String>,
    /// Per-plugin options, namespaced by plugin id.
    pub plugins: BTreeMap<String, toml::Table>,
}

impl Config {
    /// Find the nearest config file walking up from `start`, bounded at the
    /// git repository root (or filesystem root) so a config from an unrelated
    /// parent checkout cannot leak in.
    pub fn discover(start: &Path) -> Option<PathBuf> {
        let mut dir: Option<PathBuf> = Some(start.to_path_buf());
        while let Some(d) = dir {
            let candidate = d.join(CONFIG_FILE_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
            // Bounded walk: stop at the git root.
            if d.join(".git").is_dir() || d.join(".git").is_file() {
                return None;
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        None
    }

    /// Load from an explicit path or discovery; empty config if none found.
    pub fn load(start: &Path) -> Result<Config, String> {
        match Self::discover(start) {
            Some(path) => {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                let mut cfg = Self::from_str(&text)?;
                cfg.path = Some(path.clone());
                cfg.dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                cfg.resolve_paths(start);
                // Default baseline: record + check share `omni-lint-baseline.json`
                // next to the config unless overridden. Nonexistent files are
                // tolerated everywhere (load checks `is_file`).
                if cfg.baseline.is_none() {
                    cfg.baseline = Some(cfg.dir.join("omni-lint-baseline.json"));
                }
                Ok(cfg)
            }
            None => Ok(Config {
                dir: start.to_path_buf(),
                ..Default::default()
            }),
        }
    }

    pub fn from_str(text: &str) -> Result<Config, String> {
        let raw: toml::Table =
            toml::from_str(text).map_err(|e| format!("parse {}: {e}", CONFIG_FILE_NAME))?;
        let mut cfg = Config::default();

        if let Some(rules) = raw.get("rules").and_then(|r| r.as_table()) {
            for (id, val) in rules.iter() {
                let Some(t) = val.as_table() else {
                    return Err(format!("[rules.\"{id}\"] must be a table"));
                };
                let mut bool_flag: Option<bool> = None;
                let mut severity: Option<crate::Severity> = None;
                if let Some(v) = t.get("enabled") {
                    if let Some(b) = v.as_bool() {
                        bool_flag = Some(b);
                    } else if let Some(s) = v.as_str() {
                        severity = Some(parse_severity(s, id)?);
                    }
                }
                if let Some(v) = t.get("severity") {
                    let Some(s) = v.as_str() else {
                        return Err(format!("severity for rule \"{id}\" must be a string"));
                    };
                    severity = Some(parse_severity(s, id)?);
                }
                let directive = match (bool_flag, severity) {
                    (Some(false), _) => RuleDirective::Off,
                    (Some(true), sev) | (None, sev) => RuleDirective::On(sev),
                };
                cfg.rules.insert(id.clone(), directive);
            }
        }

        if let Some(targets) = raw.get("targets").and_then(|r| r.as_table()) {
            if let Some(serde::de::IgnoredAny) = targets.get("extensions").map(|_| IgnoredAny) {
                // unused; handled below through generic access
            }
            if let Some(exts) = targets.get("extensions").and_then(|v| v.as_array()) {
                let mut list = Vec::new();
                for e in exts {
                    let Some(s) = e.as_str() else {
                        return Err("[targets] extensions must be strings".into());
                    };
                    list.push(s.trim_start_matches('.').to_ascii_lowercase());
                }
                cfg.extensions = Some(list);
            }
        }

        // `ignore` may be a string or array of strings.
        if let Some(ig) = raw.get("ignore") {
            match ig {
                toml::Value::String(s) => cfg.ignore.push(s.clone()),
                toml::Value::Array(items) => {
                    for item in items {
                        let Some(s) = item.as_str() else {
                            return Err("ignore entries must be strings".into());
                        };
                        cfg.ignore.push(s.to_string());
                    }
                }
                _ => return Err("ignore must be a string or array of strings".into()),
            }
        }

        if let Some(b) = raw.get("baseline").and_then(|v| v.as_str()) {
            cfg.baseline = Some(PathBuf::from(b));
        }

        if let Some(p) = raw.get("prefer").and_then(|v| v.as_str()) {
            cfg.prefer = Some(p.to_string());
        }

        if let Some(plugins) = raw.get("plugins").and_then(|v| v.as_table()) {
            for (name, val) in plugins.iter() {
                let Some(t) = val.as_table() else {
                    return Err(format!("[plugins.{name}] must be a table"));
                };
                cfg.plugins.insert(name.clone(), t.clone());
            }
        }

        Ok(cfg)
    }

    /// `baseline` is relative to the config dir; keep it absolute-friendly.
    fn resolve_paths(&mut self, _start: &Path) {
        if let Some(b) = &self.baseline {
            if b.is_relative() {
                self.baseline = Some(self.dir.join(b));
            }
        }
    }

    /// True when glob patterns in `ignore` (relative to config dir) match `path`.
    pub fn is_ignored(&self, path: &Path) -> bool {
        if self.ignore.is_empty() {
            return false;
        }
        let mut builder = globset::GlobSetBuilder::new();
        for pat in &self.ignore {
            let mut p = pat.clone();
            // A pattern like "generated" usually means the directory tree.
            if !p.contains('*') && !p.ends_with("**") {
                if p.ends_with('/') {
                    p.push_str("**");
                } else {
                    p.push_str("/**");
                }
            }
            let anchored = format!("**/{p}");
            if let Ok(g) = globset::GlobBuilder::new(&anchored)
                .literal_separator(true)
                .build()
            {
                builder.add(g);
                let _ = p;
            }
        }
        match builder.build() {
            Ok(set) => set.is_match(path),
            Err(_) => false,
        }
    }
}

fn parse_severity(s: &str, rule_id: &str) -> Result<crate::Severity, String> {
    crate::Severity::parse(s)
        .ok_or_else(|| format!("unknown severity \"{s}\" for rule \"{rule_id}\""))
}
