//! Diagnostic rendering: compact text and JSON.
//!
//! JSON is the structured format for tooling; text is the human default.

use crate::baseline::Reportable;
use serde::Serialize;
use std::io::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
}

impl OutputFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(OutputFormat::Text),
            "json" => Some(OutputFormat::Json),
            _ => None,
        }
    }
}

#[derive(Serialize)]
pub struct RelatedJson {
    message: String,
    path: String,
    line: u32,
    column: u32,
}

#[derive(Serialize)]
pub struct DiagnosticJson {
    rule_id: String,
    severity: String,
    path: String,
    line: u32,
    column: u32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    related: Vec<RelatedJson>,
}

#[derive(Serialize)]
pub struct ReportJson {
    pub files: usize,
    pub findings: Vec<DiagnosticJson>,
    /// Stable totals for machine consumption.
    pub totals: std::collections::BTreeMap<crate::Severity, usize>,
}

pub fn format_text(report: &Reportable, source: &crate::SourceFile) -> String {
    let loc = &report.located;
    let mut out = format!(
        "{}:{}:{}: {} [{}]: {}\n",
        loc.path,
        loc.line,
        loc.column,
        report.diag.severity,
        report.diag.rule_id,
        report.diag.message
    );
    let _suppress = ();
    let (line_text, line_no) = source.line_text(report.diag.span.start);
    if !line_text.is_empty() {
        let line_start = source.line_span_start(report.diag.span.start);
        let (start, end) = (
            report.diag.span.start,
            report.diag.span.end.max(report.diag.span.start),
        );
        let leading = line_text.bytes().take_while(|b| *b == b' ' || *b == b'\t').count() as u32;
        let caret_col = (start - line_start).saturating_sub(leading.min(start - line_start));
        let caret_len = (end - start).max(1);
        out.push_str(&format!("   |\n{line_no:>4} | {line_text}\n"));
        out.push_str(&format!(
            "   | {}{}\n",
            " ".repeat(caret_col as usize),
            "^".repeat(caret_len as usize)
        ));
    }
    out
}

pub fn write_json<W: Write>(
    w: &mut W,
    files: usize,
    items: &[Reportable],
    sources: &crate::SourceMap,
    totals: &crate::SeverityTotals,
) -> Result<(), String> {
    let findings: Vec<DiagnosticJson> = items
        .iter()
        .map(|r| DiagnosticJson {
            rule_id: r.diag.rule_id.clone(),
            severity: r.diag.severity.as_str().to_string(),
            path: r.path.clone(),
            line: r.located.line,
            column: r.located.column,
            message: r.diag.message.clone(),
            subject: r.diag.subject.clone(),
            related: r
                .diag
                .related
                .iter()
                .map(|rel| {
                    let (path, line, column) = sources
                        .get(&rel.source_id)
                        .map(|s| {
                            let loc = s.locate(rel.span);
                            (loc.path, loc.line, loc.column)
                        })
                        .unwrap_or((String::new(), 0, 0));
                    RelatedJson {
                        message: rel.message.clone(),
                        path,
                        line,
                        column,
                    }
                })
                .collect(),
        })
        .collect();
    let json = &ReportJson {
        files,
        findings,
        totals: totals.clone(),
    };
    let body = serde_json::to_string_pretty(&json).map_err(|e| format!("serialize report: {e}"))?;
    writeln!(w, "{}", body).ok();
    Ok(())
}
