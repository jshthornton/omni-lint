//! Rule `graphql/deprecated-required-input`.
//!
//! Input object fields cannot be both required (non-null) and `@deprecated`.
//! Workspace-scoped (reads the schema model).

use super::shared::diag;
use crate::ast::DefKind;
use crate::model::SchemaModel;
use omni_core::plugin::RuleContext;
use omni_core::{Rule, RuleMeta, Severity};

pub struct DeprecatedRequiredInput;

impl Rule for DeprecatedRequiredInput {
    fn meta(&self) -> RuleMeta {
        RuleMeta {
            id: "graphql/deprecated-required-input",
            description: "Required input fields must not be deprecated.",
            default_severity: Severity::Error,
            requires: "graphql.schema",
        }
    }

    fn run(&self, ctx: &RuleContext) {
        let Some(model) = ctx.workspace_artifact::<SchemaModel>("graphql.schema") else {
            return;
        };
        for entries in model.types.values() {
            for e in entries {
                if e.kind != DefKind::Input {
                    continue;
                }
                for f in &e.fields {
                    if f.required && f.directives.iter().any(|d| d == "deprecated") {
                        ctx.emit(diag(
                            self.meta().id,
                            Severity::Error,
                            e.source,
                            f.name_span,
                            format!(
                                "input field `{}.{} {}` is required so it cannot be deprecated",
                                e.name, f.name, f.type_name
                            ),
                            Some(f.subject.clone()),
                        ));
                    }
                }
            }
        }
    }
}