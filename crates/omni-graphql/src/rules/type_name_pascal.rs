//! Rule `graphql/type-name-pascal`.
//!
//! Definition names must be PascalCase (GraphQL convention).

use super::shared::{diag, doc_of, is_pascal_case};
use crate::ast::DefKind;
use omni_core::plugin::RuleContext;
use omni_core::{Rule, RuleMeta, Severity};

pub struct TypeNamePascal;

impl Rule for TypeNamePascal {
    fn meta(&self) -> RuleMeta {
        RuleMeta {
            id: "graphql/type-name-pascal",
            description: "Type names must be PascalCase.",
            default_severity: Severity::Warning,
            requires: "graphql.ast",
        }
    }

    fn run(&self, ctx: &RuleContext) {
        for file in ctx.parsed {
            let Some(doc) = doc_of(ctx, file) else { continue };
            for def in &doc.defs {
                if def.kind == DefKind::Schema || def.kind == DefKind::Directive {
                    continue;
                }
                if !is_pascal_case(&def.name) {
                    ctx.emit(diag(
                        self.meta().id,
                        Severity::Warning,
                        file.source.id,
                        def.name_span,
                        format!("type name `{}` must be PascalCase", def.name),
                        Some(format!("type:{}", def.name)),
                    ));
                }
            }
        }
    }
}