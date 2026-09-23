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
    use std::collections::BTreeMap;

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
        let m = b.classify(&d("graphql/no-empty-type", Some("type:User.field:name"), Span::new(0, 4)), "a.graphql", "ffff", &mut BTreeMap::new());
        assert!(matches!(m, crate::baseline::BaselineMatch::Baselined(_)));
        // Different subject: new.
        let m = b.classify(&d("graphql/no-empty-type", Some("type:User.field:other"), Span::new(0, 4)), "a.graphql", "ffff", &mut BTreeMap::new());
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
        let m = b.classify(&d("r/x", None, Span::new(0, 4)), "a.rs", "ffff", &mut BTreeMap::new());
        assert!(matches!(m, crate::baseline::BaselineMatch::Baselined(_)));
        let m = b.classify(&d("r/x", None, Span::new(0, 4)), "a.rs", "eeee", &mut BTreeMap::new());
        assert!(matches!(m, crate::baseline::BaselineMatch::New));
    }
}

#[cfg(test)]
mod suppress_and_ratchet_tests {
    use crate::baseline::{Baseline, BaselinePair, parse_suppress_markers, is_suppressed, BaselineMatch};
    use crate::diagnostic::{Diagnostic, SourceId, Span};
    use std::collections::{BTreeMap, BTreeSet};

    fn d(rule: &str) -> Diagnostic {
        Diagnostic {
            rule_id: rule.into(),
            severity: crate::Severity::Warning,
            message: "m".into(),
            source_id: SourceId(0),
            span: Span::new(0, 1),
            related: vec![],
            subject: Some("subject".into()),
        }
    }

    #[test]
    fn suppress_markers_parse_and_match() {
        let src = "keep(1)\n# omni-lint-disable-next-line\ndropped(2)\n# omni-lint-disable-next-line rule/a rule/b\nkept(3)\n";
        let markers = parse_suppress_markers(src);
        assert_eq!(markers.len(), 2);
        // line 3: all rules suppressed
        assert!(is_suppressed(3, "any/rule", &markers));
        // line 5: only the two named rules suppressed
        assert!(is_suppressed(5, "rule/a", &markers));
        assert!(is_suppressed(5, "rule/b", &markers));
        assert!(!is_suppressed(5, "rule/c", &markers));
        assert!(!is_suppressed(1, "rule/a", &markers));
    }

    #[test]
    fn pair_budget_suppresses_then_re_flags_excess() {
        let b = Baseline {
            pairs: vec![BaselinePair { rule_id: "ex/rule".into(), path: "f.x".into(), count: 2 }],
            ..Default::default()
        };
        let mut budget = BTreeMap::new();
        // first two suppressed (ratchet fills the recorded backlog)
        assert!(matches!(b.classify(&d("ex/rule"), "f.x", "fp", &mut budget), BaselineMatch::Baselined(_)));
        assert!(matches!(b.classify(&d("ex/rule"), "f.x", "fp", &mut budget), BaselineMatch::Baselined(_)));
        // third one re-flags: over-baseline growth is never hidden
        assert!(matches!(b.classify(&d("ex/rule"), "f.x", "fp", &mut budget), BaselineMatch::New));
        // a different pair keeps its own budget
        assert!(matches!(b.classify(&d("ex/rule"), "g.y", "fp", &mut budget), BaselineMatch::New));
    }

    #[test]
    fn unknown_rule_errors_are_distinct_from_findings() {
        // Config errors route to exit 2 via CLI; this just pins the shape.
        assert_ne!(crate::runner::ExitCode::Config, crate::runner::ExitCode::Findings);
        let _ = BTreeSet::<String>::new();
    }
}
