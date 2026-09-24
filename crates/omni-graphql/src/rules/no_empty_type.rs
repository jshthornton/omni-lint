//! Rule `graphql/no-empty-type`.
//!
//! Object/interface/input/union/enum definitions must not be empty.
//! File-scoped: reads `graphql.ast` per document.

use super::shared::{diag, doc_of};
use crate::ast::DefKind;
use omni_core::plugin::RuleContext;
use omni_core::{Rule, RuleMeta, Severity};

pub struct NoEmptyType;

impl Rule for NoEmptyType {
    fn meta(&self) -> RuleMeta {
        RuleMeta {
            id: "graphql/no-empty-type",
            description: "Object, interface, input and union definitions must not be empty.",
            default_severity: Severity::Error,
            requires: "graphql.ast",
        }
    }

    fn run(&self, ctx: &RuleContext) {
        for file in ctx.parsed {
            let Some(doc) = doc_of(ctx, file) else { continue };
            for def in &doc.defs {
                match def.kind {
                    DefKind::Object | DefKind::Interface | DefKind::Input => {
                        if def.fields.is_empty() {
                            ctx.emit(diag(
                                self.meta().id,
                                Severity::Error,
                                file.source.id,
                                def.name_span,
                                format!(
                                    "type `{}` is empty; it must define at least one field",
                                    def.name
                                ),
                                Some(format!("type:{}", def.name)),
                            ));
                        }
                    }
                    DefKind::Union => {
                        if def.union_members.is_empty() {
                            ctx.emit(diag(
                                self.meta().id,
                                Severity::Error,
                                file.source.id,
                                def.name_span,
                                format!("union `{}` is empty; it must have at least one member", def.name),
                                Some(format!("type:{}", def.name)),
                            ));
                        }
                    }
                    DefKind::Enum => {
                        if def.enum_values.is_empty() {
                            ctx.emit(diag(
                                self.meta().id,
                                Severity::Error,
                                file.source.id,
                                def.name_span,
                                format!("enum `{}` is empty; it must have at least one value", def.name),
                                Some(format!("type:{}", def.name)),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}