//! Rule `graphql/field-name-camel`.
//!
//! Field names (objects/interfaces/inputs) must be camelCase.

use super::shared::{diag, doc_of, is_camel_case};
use omni_core::plugin::RuleContext;
use omni_core::{Rule, RuleMeta, Severity};

pub struct FieldNameCamel;

impl Rule for FieldNameCamel {
    fn meta(&self) -> RuleMeta {
        RuleMeta {
            id: "graphql/field-name-camel",
            description: "Field names must be camelCase.",
            default_severity: Severity::Warning,
            requires: "graphql.ast",
        }
    }

    fn run(&self, ctx: &RuleContext) {
        for file in ctx.parsed {
            let Some(doc) = doc_of(ctx, file) else { continue };
            for def in &doc.defs {
                if !def.is_fielded() {
                    continue;
                }
                for field in &def.fields {
                    if !is_camel_case(&field.name) {
                        ctx.emit(diag(
                            self.meta().id,
                            Severity::Warning,
                            file.source.id,
                            field.name_span,
                            format!("field name `{}` on `{}` must be camelCase", field.name, def.name),
                            Some(format!("type:{}.field:{}", def.name, field.name)),
                        ));
                    }
                }
            }
        }
    }
}