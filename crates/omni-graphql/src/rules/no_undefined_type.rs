//! Rule `graphql/no-undefined-type`.
//!
//! Every referenced type must be defined. Workspace-scoped: reads the
//! cross-file `graphql.schema` model.

use super::shared::diag;
use crate::model::{BUILTIN_SCALARS, SchemaModel};
use omni_core::plugin::RuleContext;
use omni_core::{Rule, RuleMeta, Severity};

pub struct NoUndefinedType;

impl Rule for NoUndefinedType {
    fn meta(&self) -> RuleMeta {
        RuleMeta {
            id: "graphql/no-undefined-type",
            description: "Referenced types must be defined in the schema.",
            default_severity: Severity::Error,
            requires: "graphql.schema",
        }
    }

    fn run(&self, ctx: &RuleContext) {
        let Some(model) = ctx.workspace_artifact::<SchemaModel>("graphql.schema") else {
            return;
        };
        for (ty, spans) in &model.used {
            if BUILTIN_SCALARS.contains(&ty.as_str()) {
                continue;
            }
            if !model.types.contains_key(ty) {
                for (i, span) in spans.iter().enumerate() {
                    let source = model
                        .used_files
                        .get(ty)
                        .and_then(|fs| fs.get(i).copied())
                        .expect("parallel used spans/files");
                    ctx.emit(diag(
                        self.meta().id,
                        Severity::Error,
                        source,
                        *span,
                        format!("type `{ty}` is used but not defined in the schema"),
                        Some(format!("type-ref:{ty}")),
                    ));
                }
            }
        }
    }
}