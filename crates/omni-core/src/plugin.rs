//! Plugin and rule traits + capability negotiation.
//!
//! The engine owns config, discovery, scheduling, diagnostics, baselines and
//! reporting. A *domain plugin* owns parsing and (optionally) cross-file
//! semantic models for its inputs; a *rule pack* is any collection of `Rule`
//! implementations registered by a plugin. Rules declare which capability
//! they require (e.g. `graphql.ast` or `graphql.schema`), and the engine only
//! runs them once that capability is available.

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
pub struct ParsedFile {
    pub source: Arc<SourceFile>,
    /// capability name -> typed artifact (downcast by rules)
    pub artifacts: BTreeMap<String, Arc<dyn Any + Send + Sync>>,
    /// Findings the plugin itself reported during parse (e.g. WASM plugins
    /// run their file rules inside the parse call). Counted like rule findings.
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

/// A domain plugin: parses inputs, produces capabilities, and registers rules.
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

    /// Run plugin-specific workspace rules over aggregated artifacts.
    /// Default: none (native workspace rules use the `Rule` trait instead).
    /// Findings are addressed by relative path; the runner resolves them.
    fn run_workspace_rules(&self, files: &[ParsedFile]) -> Vec<crate::PathDiagnostic> {
        let _ = files;
        Vec::new()
    }
}

/// Extra per-plugin config passed to workspace construction.
pub type WorkspaceConfig = BTreeMap<String, toml::Value>;

/// Metadata for one rule.
#[derive(Debug, Clone)]
pub struct RuleMeta {
    /// Full id, namespaced by plugin, e.g. `graphql/no-empty-type`.
    pub id: &'static str,
    pub description: &'static str,
    pub default_severity: crate::diagnostic::Severity,
    /// Capability this rule reads. `""` = none.
    pub requires: &'static str,
}

/// A lint rule. Rules concrete over their domain, downcast the capability
/// from `ctx` and report findings via `ctx.emit`.
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
            emit,
        }
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

    pub fn source_id_of(&self, file: &ParsedFile) -> SourceId {
        file.source.id
    }
}
