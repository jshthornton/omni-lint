//! Rule `graphql/duplicate-type`.
//!
//! A type must be defined only once. Workspace-scoped: duplicates usually
//! live in different files.

use crate::model::SchemaModel;
use omni_core::plugin::RuleContext;
use omni_core::{Diagnostic, RelatedSpan, Rule, RuleMeta, Severity};

pub struct DuplicateType;

impl Rule for DuplicateType {
    fn meta(&self) -> RuleMeta {
        RuleMeta {
            id: "graphql/duplicate-type",
            description: "Type names must be unique across the schema.",
            default_severity: Severity::Error,
            requires: "graphql.schema",
        }
    }

    fn run(&self, ctx: &RuleContext) {
        let Some(model) = ctx.workspace_artifact::<SchemaModel>("graphql.schema") else {
            return;
        };
        for (name, entries) in &model.types {
            if entries.len() < 2 {
                continue;
            }
            let first = &entries[0];
            let others = entries[1..].to_vec();
            for e in others {
                let mut related = Vec::new();
                related.push(RelatedSpan {
                    message: format!("`{name}` also defined here"),
                    source_id: e.source,
                    span: e.name_span,
                });
                ctx.emit(Diagnostic {
                    rule_id: self.meta().id.into(),
                    severity: Severity::Error,
                    message: format!("type `{name}` is defined {} times in the schema", entries.len()),
                    source_id: first.source,
                    span: first.name_span,
                    related,
                    subject: Some(format!("type:{name}")),
                });
            }
        }
    }
}