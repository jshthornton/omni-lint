//! Cross-file GraphQL schema semantic model.
//!
//! Built once per workspace from all parsed documents. Computes which types
//! are referenced, which are defined, and where definitions live so
//! workspace rules (undefined / unused references) don't re-scan files.

use crate::ast::{Def, DefKind, FieldDef};

fn def_directives(def: &Def) -> Vec<String> {
    def.directives.iter().map(|d| d.name.clone()).collect()
}
use omni_core::SourceId;
use std::collections::{BTreeMap, BTreeSet};

/// Built-in scalar names that can be used without a definition.
pub const BUILTIN_SCALARS: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];

#[derive(Debug, Clone)]
pub struct TypeEntry {
    pub kind: DefKind,
    pub name: String,
    pub source: SourceId,
    pub name_span: omni_core::Span,
    /// Populated for Object/Interface/Input.
    pub fields: Vec<FieldEntry>,
    pub implements: BTreeSet<String>,
    pub union_members: Vec<String>,
    pub enum_values: Vec<String>,
    pub directives: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FieldEntry {
    pub name: String,
    pub name_span: omni_core::Span,
    pub span: omni_core::Span,
    pub type_name: String,
    pub required: bool,
    pub directives: Vec<String>,
    /// Stable subject id for TODO baselines (`type:User.field:name`).
    pub subject: String,
}

/// Workspace model, published under capability `graphql.schema`.
#[derive(Debug, Default)]
pub struct SchemaModel {
    /// Type name -> definition(s) (duplicates preserved across files).
    pub types: BTreeMap<String, Vec<TypeEntry>>,
    /// Types referenced anywhere, with file/span of each reference.
    pub used: BTreeMap<String, Vec<omni_core::Span>>,
    /// Reference file map parallel to `used`: (type name -> source id).
    pub used_files: BTreeMap<String, Vec<SourceId>>,
    /// Directives referenced via `@name`.
    pub used_directives: BTreeSet<String>,
    /// Defined directive names.
    pub defined_directives: BTreeSet<String>,
    /// True when a `schema { ... }` block exists anywhere.
    pub has_schema_block: bool,
    /// Root operation names, e.g. `query` -> `Query`.
    pub roots: BTreeMap<String, String>,
}

fn field_entry(field: &FieldDef, type_name: &str) -> FieldEntry {
    FieldEntry {
        subject: format!("type:{type_name}.field:{}", field.name),
        name_span: field.name_span,
        span: field.span,
        type_name: field.ty.named.to_string(),
        required: field.ty.is_required(),
        directives: field.directives.iter().map(|d| d.name.clone()).collect(),
        name: field.name.clone(),
    }
}

fn push_used(
    used: &mut BTreeMap<String, Vec<omni_core::Span>>,
    used_files: &mut BTreeMap<String, Vec<SourceId>>,
    name: &str,
    source: SourceId,
    span: omni_core::Span,
) {
    used.entry(name.to_string()).or_default().push(span);
    used_files.entry(name.to_string()).or_default().push(source);
}

/// Build a SchemaModel from (source id, document) pairs.
pub fn build_model(docs: &[(SourceId, &crate::ast::Document)]) -> SchemaModel {
    let mut model = SchemaModel::default();

    for (source, doc) in docs {
        for def in &doc.defs {
            let entry = TypeEntry {
                kind: def.kind,
                name: def.name.clone(),
                source: *source,
                name_span: def.name_span,
                fields: def.fields.iter().map(|f| field_entry(f, &def.name)).collect(),
                implements: def.implements.iter().cloned().collect(),
                union_members: def.union_members.clone(),
                enum_values: def.enum_values.clone(),
                directives: def_directives(def),
            };
            if def.kind == DefKind::Directive {
                model.defined_directives.insert(def.name.clone());
            }
            if def.kind == DefKind::Schema {
                model.has_schema_block = true;
                for (op, ty) in &def.root_ops {
                    model.roots.insert(op.clone(), ty.named.clone());
                    if !BUILTIN_SCALARS.contains(&ty.named.as_str()) {
                        push_used(&mut model.used, &mut model.used_files, &ty.named, *source, ty.named_span);
                    }
                }
            }
            // Field/arg references.
            for f in &def.fields {
                if !BUILTIN_SCALARS.contains(&f.ty.named.as_str()) {
                    push_used(&mut model.used, &mut model.used_files, &f.ty.named, *source, f.ty.named_span);
                }
                model.used_directives.extend(f.directives.iter().map(|d| d.name.clone()));
                for a in &f.args {
                    if !BUILTIN_SCALARS.contains(&a.ty.named.as_str()) {
                        push_used(&mut model.used, &mut model.used_files, &a.ty.named, *source, a.ty.named_span);
                    }
                }
            }
            for iface in &def.implements {
                push_used(&mut model.used, &mut model.used_files, iface, *source, def.name_span);
            }
            for member in &def.union_members {
                push_used(&mut model.used, &mut model.used_files, member, *source, def.name_span);
            }
            model.types.entry(def.name.clone()).or_default().push(entry);
        }
    }
    model
}
