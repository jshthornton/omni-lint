//! Rule `graphql/no-unused-type`.
//!
//! Defined-but-never-referenced types are dead code. Reported only when a
//! schema block exists (without one, roots are convention-inferred and
//! unused detection would be ambiguous). Workspace-scoped.

use super::shared::diag;
use crate::model::SchemaModel;
use omni_core::plugin::RuleContext;
use omni_core::{Rule, RuleMeta, Severity, Span};
use std::collections::BTreeSet;

pub struct NoUnusedType;

impl Rule for NoUnusedType {
    fn meta(&self) -> RuleMeta {
        RuleMeta {
            id: "graphql/no-unused-type",
            description: "Types not referenced anywhere and not schema roots are dead code.",
            default_severity: Severity::Info,
            requires: "graphql.schema",
        }
    }

    fn run(&self, ctx: &RuleContext) {
        let Some(model) = ctx.workspace_artifact::<SchemaModel>("graphql.schema") else {
            return;
        };
        if !model.has_schema_block {
            // Without a schema block the roots are inferred by convention,
            // which is plugin-defined; unused detection is suppressed.
            return;
        }
        let roots: BTreeSet<&String> = model.roots.values().collect();
        for (name, entries) in &model.types {
            if roots.contains(&name) || name == "schema" {
                continue;
            }
            if model.used.contains_key(name) {
                continue;
            }
            for e in entries {
                ctx.emit(diag(
                    self.meta().id,
                    Severity::Info,
                    e.source,
                    e.name_span,
                    format!("type `{name}` is defined but never referenced"),
                    Some(format!("type:{name}")),
                ));
            }
        }
        // Unused directives too.
        for d in &model.defined_directives {
            if !model.used_directives.contains(d) && d != "skip" && d != "include" && d != "deprecated" {
                ctx.emit(diag(
                    self.meta().id,
                    Severity::Info,
                    model
                        .types
                        .values()
                        .next()
                        .and_then(|v| v.first())
                        .map(|first| first.source)
                        .unwrap_or(omni_core::SourceId(0)),
                    Span::new(0, 0),
                    format!("directive `@{d}` is defined but never applied"),
                    Some(format!("directive:{d}")),
                ));
            }
        }
    }
}