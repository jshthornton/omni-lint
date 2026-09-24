//! Span, Severity and Diagnostic — the shared contracts every plugin and rule uses.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Severity of a diagnostic. Order matters: it ranks severities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
    /// Rule-meta only: the rule is registered but disabled by default
    /// (eslint "off" / rubocop opt-in). Never a finding severity; users opt
    /// in per rule in `omni-lint.toml`.
    Off,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
            Severity::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Severity> {
        match s {
            "info" => Some(Severity::Info),
            "warning" | "warn" => Some(Severity::Warning),
            "error" => Some(Severity::Error),
            "off" => Some(Severity::Off),
            _ => None,
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A byte-offset half-open range `[start, end)` within a [`SourceFile`].
/// Plugins always produce byte spans in their own parsed input; the host
/// converts to line/column for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }

    pub fn merge(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

/// A resolved location for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: String, // path as the user knows it (usually relative to cwd)
    pub line: u32,    // 1-based
    pub column: u32,  // 1-based, byte-based UTF-8 column
}

/// An additional related location attached to a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedSpan {
    pub message: String,
    pub source_id: SourceId,
    pub span: Span,
}

/// Opaque per-file id handed out by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceId(pub u32);

/// A related span attached to [`PartialDiagnostic`], expressed by relative
/// path (resolved by the runner once it knows all sources).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedPathSpan {
    pub message: String,
    pub path: String,
    pub span: Span,
}

/// A finding emitted by a rule. Spans are file-relative byte offsets.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    pub source_id: SourceId,
    pub span: Span,
    pub related: Vec<RelatedSpan>,
    /// Stable domain-defined subject identity, used for TODO baselines
    /// (e.g. `type:User.field:name`). Optional.
    pub subject: Option<String>,
}

/// A finding carrying a relative path instead of a resolved `SourceId` —
/// used when the engine maps findings reported for paths it owns.
#[derive(Debug, Clone)]
pub struct PartialDiagnostic {
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    /// Byte span within `path`.
    pub span: Span,
    pub subject: Option<String>,
    pub related: Vec<RelatedPathSpan>,
}

/// A [`PartialDiagnostic`] keyed by relative path.
#[derive(Debug, Clone)]
pub struct PathDiagnostic {
    pub path: String,
    pub diag: PartialDiagnostic,
}

impl Diagnostic {
    /// Represent this diagnostic as a partial one keyed by relative path.
    pub fn to_partial(self, path: impl Into<String>) -> PathDiagnostic {
        PathDiagnostic {
            path: path.into(),
            diag: PartialDiagnostic {
                rule_id: self.rule_id,
                severity: self.severity,
                message: self.message,
                span: self.span,
                subject: self.subject,
                related: self
                    .related
                    .into_iter()
                    .map(|r| RelatedPathSpan {
                        message: r.message,
                        path: String::new(),
                        span: r.span,
                    })
                    .collect(),
            },
        }
    }
}

impl PartialDiagnostic {
    /// Resolve to a full diagnostic against a source id.
    pub fn into_diagnostic(self, source_id: SourceId) -> Diagnostic {
        Diagnostic {
            rule_id: self.rule_id,
            severity: self.severity,
            message: self.message,
            source_id,
            span: self.span,
            subject: self.subject,
            related: self
                .related
                .into_iter()
                .map(|r| RelatedSpan {
                    message: r.message,
                    source_id,
                    span: r.span,
                })
                .collect(),
        }
    }
}

/// A parse or engine-level error, reported under a rule id of `parse`/plugin id.
#[derive(Debug, Clone)]
pub struct EngineError {
    pub kind: EngineErrorKind,
    pub message: String,
    pub source_id: Option<SourceId>,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineErrorKind {
    /// Input failed to parse; treated like findings (exit 1).
    Parse,
    /// Configuration is wrong; exit 2.
    Config,
    /// Plugin/infrastructure failure; exit 3.
    Plugin,
}

/// Sink rules emit into. Engine-provided.
pub trait Sink: Send + Sync {
    fn emit(&self, diag: Diagnostic);
}
