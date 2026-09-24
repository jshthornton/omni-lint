//! Language plugin and rule contracts + capability negotiation.
//!
//! The engine owns config, discovery, scheduling, diagnostics, baselines and
//! reporting. A *language plugin* owns parsing and (optionally) cross-file
//! semantic models for its inputs — it is a pure AST provider and contains no
//! rules. A *rule* is an independently authored unit (a native `Rule` impl or
//! a one-rule WASM module registered at load time) that reads capabilities
//! and emits findings. A *ruleset* is assembled from rules à la carte, like
//! eslint/credo/rubocop: write one rule, register it, tune it in config.
//!
//! Rules declare which capability they require (e.g. `graphql.facts` for
//! JSON-facts rules, `graphql.ast`/`graphql.schema` for typed native rules,
//! `graphql.workspace` for cross-file JSON models), and the engine only runs
//! them once that capability is available.

use crate::diagnostic::{Diagnostic, SourceId};
use crate::source::SourceFile;
use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Whether a capability is produced per file or once per workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityScope {
    File,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub name: &'static str,
    pub scope: CapabilityScope,
}

/// The unit a plugin produces per input file: the source plus named artifacts.
///
/// Facts-capability artifacts (e.g. `graphql.facts`) are
/// `Arc<serde_json::Value>` so independently authored WASM rules can consume
/// any language plugin's output; typed artifacts (e.g. `graphql.ast`) are for
/// native rules over that plugin's own AST.
pub struct ParsedFile {
    pub source: Arc<SourceFile>,
    /// capability name -> typed artifact (downcast by rules)
    pub artifacts: BTreeMap<String, Arc<dyn Any + Send + Sync>>,
    /// Findings the plugin itself reported during parse (parse-time checks,
    /// not rules). Counted like rule findings.
    pub findings: Vec<Diagnostic>,
    /// Parse errors, if any, reported under the plugin's diagnostic id.
    pub errors: Vec<Diagnostic>,
}

impl ParsedFile {
    pub fn artifact<T: Send + Sync + 'static>(&self, cap: &str) -> Option<&T> {
        self.artifacts.get(cap).and_then(|a| a.downcast_ref::<T>())
    }

    pub fn empty(source: Arc<SourceFile>) -> Self {
        ParsedFile {
            source,
            artifacts: BTreeMap::new(),
            findings: Vec::new(),
            errors: Vec::new(),
        }
    }
}

/// A language plugin: parses inputs and produces capabilities (an AST
/// provider). Rules are registered separately — a plugin never bundles rules.
pub trait Plugin: Send + Sync {
    /// Plugin id, used as namespace for its rules and warnings
    /// (e.g. `graphql`).
    fn id(&self) -> &'static str;
    /// Human-readable description.
    fn describe(&self) -> &'static str;
    /// File extensions (without dot, lowercase) this plugin parses.
    fn extensions(&self) -> &'static [&'static str];
    /// Capabilities this plugin can produce.
    fn capabilities(&self) -> Vec<Capability>;
    /// Parse one file into artifacts. Parse failures become `errors` —
    /// never host panics.
    fn parse_file(&self, source: Arc<SourceFile>) -> ParsedFile;
    /// Build workspace-scoped capabilities after all files are parsed.
    /// Only called if some enabled rule requires a workspace capability.
    fn build_workspace(
        &self,
        files: &[ParsedFile],
        config: &WorkspaceConfig,
    ) -> Result<BTreeMap<String, Arc<dyn Any + Send + Sync>>, String> {
        let _ = (files, config);
        Ok(BTreeMap::new())
    }

}

/// Extra per-plugin config passed to workspace construction.
pub type WorkspaceConfig = BTreeMap<String, toml::Value>;

/// Metadata for one rule.
#[derive(Debug, Clone)]
pub struct RuleMeta {
    /// Full id, namespaced by the rule author, e.g. `graphql/no-empty-type`.
    /// Rule ids are author-chosen (like eslint rule names): any namespace,
    /// registered à la carte into the ruleset.
    pub id: &'static str,
    pub description: &'static str,
    pub default_severity: crate::diagnostic::Severity,
    /// Capability this rule reads, e.g. `graphql.facts` (JSON facts view,
    /// consumable by WASM rules) or `graphql.ast` / `graphql.schema` (typed
    /// native artifacts). `""` = none.
    pub requires: &'static str,
}

/// A lint rule. Rules concrete over their domain, downcast the capability
/// from `ctx` and report findings via `ctx.emit`. Native rules implement this
/// trait directly; one-rule WASM modules are adapted into it by `omni-wasm`.
pub trait Rule: Send + Sync {
    fn meta(&self) -> RuleMeta;
    fn run(&self, ctx: &RuleContext);
}

/// What a rule sees at runtime.
pub struct RuleContext<'a> {
    /// All parsed files in the workspace (parsed once, shared across rules).
    pub parsed: &'a [ParsedFile],
    /// Workspace-level artifacts (empty when the rule only needs file scope).
    pub workspace: &'a BTreeMap<String, Arc<dyn Any + Send + Sync>>,
    /// Per-rule options from `omni-lint.toml` (`[rules."<id>"]`), when set.
    pub options: Option<serde_json::Value>,
    /// Sink for the enabled severity-adjusted flow.
    emit: &'a dyn Fn(Diagnostic),
}

impl<'a> RuleContext<'a> {
    pub fn new(
        parsed: &'a [ParsedFile],
        workspace: &'a BTreeMap<String, Arc<dyn Any + Send + Sync>>,
        emit: &'a dyn Fn(Diagnostic),
    ) -> Self {
        RuleContext {
            parsed,
            workspace,
            options: None,
            emit,
        }
    }

    /// Attach per-rule options from config.
    pub fn with_options(mut self, options: Option<serde_json::Value>) -> Self {
        self.options = options;
        self
    }

    pub fn emit(&self, diag: Diagnostic) {
        (self.emit)(diag)
    }

    /// File-scoped artifact for one parsed file.
    pub fn file_artifact<'p, T: Send + Sync + 'static>(
        &self,
        file: &'p ParsedFile,
        cap: &str,
    ) -> Option<&'p T> {
        file.artifact::<T>(cap)
    }

    /// Workspace-scoped artifact.
    pub fn workspace_artifact<T: Send + Sync + 'static>(&self, cap: &str) -> Option<&T> {
        self.workspace.get(cap).and_then(|a| a.downcast_ref::<T>())
    }

    /// JSON facts view of one file (facts capabilities hold
    /// `Arc<serde_json::Value>`, so every provider speaks one vocabulary).
    pub fn file_facts<'p>(
        &self,
        file: &'p ParsedFile,
        cap: &str,
    ) -> Option<&'p serde_json::Value> {
        file.artifact::<serde_json::Value>(cap)
    }

    /// JSON cross-file model, when the language plugin provides one.
    pub fn workspace_facts(&self, cap: &str) -> Option<&serde_json::Value> {
        self.workspace
            .get(cap)
            .and_then(|a| a.downcast_ref::<serde_json::Value>())
    }

    /// Source id for a workspace-relative path (findings from JSON rules are
    /// addressed by path; the engine resolves them here).
    pub fn source_id_for_path(&self, path: &str) -> Option<SourceId> {
        self.parsed
            .iter()
            .find(|f| f.source.path.display().to_string() == path)
            .map(|f| f.source.id)
    }

    pub fn source_id_of(&self, file: &ParsedFile) -> SourceId {
        file.source.id
    }
}
