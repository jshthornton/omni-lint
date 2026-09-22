pub mod baseline;
pub mod config;
pub mod diagnostic;
pub mod discovery;
pub mod plugin;
pub mod registry;
pub mod report;
pub mod runner;
pub mod source;

pub use diagnostic::{
    Diagnostic, EngineError, EngineErrorKind, Located, PartialDiagnostic, PathDiagnostic, RelatedPathSpan, RelatedSpan, Severity, Sink, SourceId, Span,
};
pub use plugin::{Capability, CapabilityScope, ParsedFile, Plugin, Rule, RuleContext, RuleMeta};
pub use source::SourceFile;
pub use registry::Registry;

pub type SourceMap = std::collections::BTreeMap<SourceId, std::sync::Arc<SourceFile>>;
pub type SeverityTotals = std::collections::BTreeMap<Severity, usize>;

#[cfg(test)]
mod tests {
    use crate::diagnostic::{Diagnostic, SourceId, Span};
    use crate::baseline::Baseline;

    fn d(rule: &str, subject: Option<&str>, sp: Span) -> Diagnostic {
        Diagnostic {
            rule_id: rule.into(),
            severity: crate::Severity::Warning,
            message: "msg".into(),
            source_id: SourceId(0),
            span: sp,
            related: vec![],
            subject: subject.map(|s| s.to_string()),
        }
    }

    #[test]
    fn baseline_subject_identity_matches() {
        let b = Baseline {
            entries: vec![crate::baseline::BaselineEntry {
                rule_id: "graphql/no-empty-type".into(),
                path: "a.graphql".into(),
                subject: Some("type:User.field:name".into()),
                line: 3,
                fingerprint: "deadbeef".into(),
            }],
            ..Default::default()
        };
        // Same subject: baselined even with a different fingerprint.
        let m = b.classify(&d("graphql/no-empty-type", Some("type:User.field:name"), Span::new(0, 4)), "a.graphql", "ffff");
        assert!(matches!(m, crate::baseline::BaselineMatch::Baselined(_)));
        // Different subject: new.
        let m = b.classify(&d("graphql/no-empty-type", Some("type:User.field:other"), Span::new(0, 4)), "a.graphql", "ffff");
        assert!(matches!(m, crate::baseline::BaselineMatch::New));
    }

    #[test]
    fn baseline_fingerprint_fallback_matches() {
        let b = Baseline {
            entries: vec![crate::baseline::BaselineEntry {
                rule_id: "r/x".into(),
                path: "a.rs".into(),
                subject: None,
                line: 3,
                fingerprint: "ffff".into(),
            }],
            ..Default::default()
        };
        let m = b.classify(&d("r/x", None, Span::new(0, 4)), "a.rs", "ffff");
        assert!(matches!(m, crate::baseline::BaselineMatch::Baselined(_)));
        let m = b.classify(&d("r/x", None, Span::new(0, 4)), "a.rs", "eeee");
        assert!(matches!(m, crate::baseline::BaselineMatch::New));
    }
}
