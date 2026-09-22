//! Rules for GraphQL SDL. File rules read `graphql.ast`; workspace rules
//! read `graphql.schema`. All rules are registered by `GraphqlPlugin`.

use crate::ast::{DefKind, Document};
use crate::model::{BUILTIN_SCALARS, SchemaModel};
use omni_core::plugin::RuleContext;
use omni_core::{Diagnostic, Rule, RuleMeta, Severity, SourceId, Span};
use std::collections::BTreeSet;
use std::sync::Arc;

fn diag(
    rule_id: &str,
    severity: Severity,
    source_id: SourceId,
    span: Span,
    message: impl Into<String>,
    subject: Option<String>,
) -> Diagnostic {
    Diagnostic {
        rule_id: rule_id.into(),
        severity,
        message: message.into(),
        source_id,
        span,
        related: vec![],
        subject,
    }
}

fn doc_of<'p>(ctx: &'p RuleContext<'_>, file: &'p omni_core::ParsedFile) -> Option<&'p Document> {
    ctx.file_artifact::<Document>(file, "graphql.ast")
}

/// graphql/no-empty-type: object/interface/input/union definitions must have
/// at least one field/member.
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

/// graphql/type-name-lowercase? no: GraphQL convention is PascalCase.
/// graphql/type-name-pascal — definition names must be PascalCase.
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

fn is_pascal_case(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// graphql/field-name-camel — field names (objects/interfaces/inputs) must
/// be camelCase. Supports auto-fixable end lines.
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

fn is_camel_case(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    s.chars().skip(1).all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// graphql/no-undefined-type — every referenced type must be defined
/// (workspace rule over `graphql.schema`).
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

/// graphql/no-unused-type — defined types that are never referenced are
/// reported, but only when a schema block exists (otherwise every entry point
/// type would look unused — matching the design's "skip when ambiguous"
/// stance).
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
                    "graphql/no-unused-type",
                    Severity::Info,
                    model
                        .types
                        .values()
                        .next()
                        .and_then(|v| v.first())
                        .map(|first| first.source)
                        .unwrap_or(SourceId(0)),
                    Span::new(0, 0),
                    format!("directive `@{d}` is defined but never applied"),
                    Some(format!("directive:{d}")),
                ));
            }
        }
    }
}

/// graphql/deprecated-required-input — input object fields cannot be both
/// required (non-null) and @deprecated.
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

/// graphql/duplicate-type — a type must be defined only once.
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
                related.push(omni_core::RelatedSpan {
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

/// All Graphql rule constructors, in registration order.
pub fn all_rules() -> Vec<Arc<dyn Rule>> {
    vec![
        Arc::new(NoEmptyType),
        Arc::new(TypeNamePascal),
        Arc::new(FieldNameCamel),
        Arc::new(DeprecatedRequiredInput),
        Arc::new(NoUndefinedType),
        Arc::new(DuplicateType),
        Arc::new(NoUnusedType),
    ]
}
