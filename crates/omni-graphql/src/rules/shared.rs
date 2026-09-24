//! Helpers shared by the GraphQL example rules.

use crate::ast::Document;
use omni_core::plugin::RuleContext;
use omni_core::{Diagnostic, Severity, SourceId, Span};

pub fn diag(
    rule_id: &str,
    severity: Severity,
    source_id: SourceId,
    span: Span,
    message: impl Into<String>,
    subject: Option<String>,
) -> Diagnostic {
    Diagnostic {
        rule_id: rule_id.into(),
        severity,
        message: message.into(),
        source_id,
        span,
        related: vec![],
        subject,
    }
}

pub fn doc_of<'p>(ctx: &'p RuleContext<'_>, file: &'p omni_core::ParsedFile) -> Option<&'p Document> {
    ctx.file_artifact::<Document>(file, "graphql.ast")
}

pub fn is_pascal_case(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn is_camel_case(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    s.chars().skip(1).all(|c| c.is_ascii_alphanumeric() || c == '_')
}