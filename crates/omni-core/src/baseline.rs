//! Baseline ("TODO") records. Two suppression granularities, mixable in one
//! file:
//!
//! 1. **Per-finding identity** (default): rule id + path + stable subject
//!    identity (when the plugin provides one) + a bounded context fingerprint
//!    over the finding's span text. Line-safe; survives edits elsewhere in
//!    the file.
//!
//! 2. **Pair counts** (ratchet granularity): rule id + path + a recorded
//!    count, grouped under `pairs`. Findings fill a pair's count in order of
//!    appearance; the excess re-flags — so a pre-existing backlog can be
//!    recorded wholesale without hiding growth.

use crate::diagnostic::{Diagnostic, Severity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A per-(rule, path) suppression budget. Findings up to `count` are
/// suppressed in order of appearance; anything beyond re-flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselinePair {
    pub rule_id: String,
    pub path: String,
    pub count: u64,
}

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
    /// Per-finding entries (identity granularity).
    #[serde(default)]
    pub entries: Vec<BaselineEntry>,
    /// Per-(rule, path) suppression budgets (ratchet granularity). Findings
    /// fill these in order of appearance; the excess over `count` re-flags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<BaselinePair>,
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

    /// Classify a fresh finding as baselined or new.
    ///
    /// Identity match first (subject, then fingerprint); then a pair-count
    /// budget if the finding's (rule, path) pair has recorded counts.
    /// `pair_budget_left` is the mutable per-run remaining budget per pair.
    pub fn classify(
        &self,
        diag: &Diagnostic,
        path: &str,
        fingerprint: &str,
        pair_budget_left: &mut BTreeMap<(String, String), u64>,
    ) -> BaselineMatch {
        // Per-finding identity first.
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
        if let Some(i) = chosen {
            return BaselineMatch::Baselined(i);
        }
        // Pair-count budget: suppress in order of appearance until the
        // recorded count is exhausted; the excess re-flags.
        let key = (diag.rule_id.clone(), path.to_string());
        let left = pair_budget_left.entry(key).or_insert_with(|| {
            self.pairs
                .iter()
                .find(|p| p.rule_id == diag.rule_id && p.path == path)
                .map(|p| p.count)
                .unwrap_or(0)
        });
        if *left > 0 {
            *left -= 1;
            return BaselineMatch::Baselined(usize::MAX);
        }
        BaselineMatch::New
    }
}

pub enum BaselineMatch {
    /// An existing TODO covered this finding (entry index; MAX for pair).
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
        pairs: Vec::new(),
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

/// Hard limit on findings a single diagnostic may carry.
const MAX_RELATED: usize = 32;

/// Scan `source` for suppression markers. Generic across domains: any comment
/// (line starts with `#` or `//` after indent) containing
/// `omni-lint-disable-next-line` [optional space- or comma-separated rule ids]
/// suppresses every finding on the following line. `all` suppresses every
/// rule. Returns line (1-based) -> rule ids ('all' = empty set meaning all).
pub fn parse_suppress_markers(source: &str) -> BTreeMap<u32, std::collections::BTreeSet<String>> {
    let mut out: BTreeMap<u32, std::collections::BTreeSet<String>> = BTreeMap::new();
    let lines: Vec<&str> = source.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix("#")
            .or_else(|| trimmed.strip_prefix("//"))
        else {
            continue;
        };
        let Some(rest) = rest
            .trim_start()
            .strip_prefix("omni-lint-disable-next-line")
        else {
            continue;
        };
        let target_line = (idx + 2) as u32; // 1-based following line
        let ids: std::collections::BTreeSet<String> = rest
            .trim()
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        out.entry(target_line).or_default().extend(ids);
        if out.len() > 10_000 {
            break;
        }
    }
    let _ = MAX_RELATED;
    out
}

/// Is a finding on `line` suppressed for `rule_id`?
pub fn is_suppressed(
    line: u32,
    rule_id: &str,
    markers: &BTreeMap<u32, std::collections::BTreeSet<String>>,
) -> bool {
    if markers.is_empty() {
        return false;
    }
    match markers.get(&line) {
        None => false,
        Some(rules) => rules.is_empty() || rules.contains(rule_id),
    }
}

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
