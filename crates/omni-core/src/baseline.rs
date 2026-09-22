//! Baseline ("TODO") records: stable identity + fingerprint matching.

use crate::diagnostic::{Diagnostic, Severity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// One baseline entry representing an existing, "won't fix now" finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub rule_id: String,
    pub path: String,
    /// Optional domain-provided subject identity (`type:User.field:name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Line where the finding occurred when recorded (informational).
    pub line: u32,
    /// Bounded context fingerprint at record time.
    pub fingerprint: String,
}

/// Baseline of known findings.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Baseline {
    #[serde(default)]
    pub generated_at: Option<String>,
    pub entries: Vec<BaselineEntry>,
}

impl Baseline {
    pub fn load(path: &Path) -> Result<Baseline, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        serde_json::from_str(&text)
            .map_err(|e| format!("invalid baseline {}: {e}", path.display()))
    }

    pub fn load_existing(path: &Path) -> Result<Option<Baseline>, String> {
        if path.is_file() {
            Self::load(path).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("serialize baseline: {e}"))?;
        std::fs::write(path, text + "\n")
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    /// Classify a fresh finding as baselined, new, or stale-consuming.
    pub fn classify(
        &self,
        diag: &Diagnostic,
        path: &str,
        fingerprint: &str,
    ) -> BaselineMatch {
        // First match on subject identity when available, else rule+path+fingerprint.
        let mut chosen: Option<usize> = None;
        for (i, e) in self.entries.iter().enumerate() {
            if e.rule_id != diag.rule_id || e.path != path {
                continue;
            }
            let stable = e
                .subject
                .as_deref()
                .map(|s| diag.subject.as_deref() == Some(s))
                .unwrap_or(false);
            if stable {
                chosen = Some(i);
                break;
            }
        }
        if chosen.is_none() {
            for (i, e) in self.entries.iter().enumerate() {
                if e.rule_id != diag.rule_id || e.path != path {
                    continue;
                }
                if e.fingerprint == fingerprint {
                    chosen = Some(i);
                    break;
                }
            }
        }
        match chosen {
            Some(i) => BaselineMatch::Baselined(i),
            None => BaselineMatch::New,
        }
    }
}

pub enum BaselineMatch {
    /// An existing TODO covered this finding.
    Baselined(usize),
    /// Finding is not in the baseline: report it.
    New,
}

/// A non-baselined finding to report.
#[derive(Debug)]
pub struct Reportable {
    pub diag: Diagnostic,
    pub path: String,
    pub located: crate::Located,
}

/// Count of same-rule/path diagnostics per fingerprint for the worklist.
pub type DistinctCount = BTreeMap<(String, String), usize>;

/// Record a set of fresh diagnostics into a baseline (TODO generate).
pub fn build_baseline(
    diags: Vec<(Diagnostic, String, u32, String)>, // diag, path, line, fingerprint
) -> Baseline {
    Baseline {
        generated_at: Some(now_iso()),
        entries: diags
            .into_iter()
            .map(
                |(d, path, line, fingerprint)| BaselineEntry {
                    rule_id: d.rule_id,
                    path,
                    subject: d.subject,
                    line,
                    fingerprint,
                },
            )
            .collect(),
    }
}

/// Remove baseline entries for findings that no longer exist in the tree.
/// `still_fresh` = rule_id+path+line+fingerprint of current unused-baseline
/// findings, plus currently-fired findings.
pub fn prune_baseline(
    mut baseline: Baseline,
    still_valid: &std::collections::BTreeSet<(String, String)>,
) -> Baseline {
    baseline
        .entries
        .retain(|e| still_valid.contains(&(e.rule_id.clone(), e.path.clone())));
    baseline
}

/// Handy totals for reporting.
pub fn count_severities(items: &[Reportable]) -> BTreeMap<Severity, usize> {
    let mut m = BTreeMap::new();
    for r in items {
        *m.entry(r.diag.severity).or_insert(0) += 1;
    }
    m
}

pub fn now_iso() -> String {
    // No chrono dependency; emit seconds-since-epoch with ISO-ish prefix.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch-{secs}")
}
