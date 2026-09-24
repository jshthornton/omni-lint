//! The JSON **facts** view of GraphQL SDL.
//!
//! Facts are the language-agnostic hand-off between an AST plugin and
//! independently authored rules: the GraphQL plugin publishes this JSON under
//! capability `graphql.facts` (per file) and `graphql.workspace` (cross-file),
//! so a one-rule WASM module written against this vocabulary runs unchanged
//! over any GraphQL AST provider. Native rules use the typed artifacts
//! (`graphql.ast` / `graphql.schema`) instead; this is the JSON twin.
//!
//! Shape (per file; the workspace view adds a `path` to every `def`/`use`):
//!
//! ```json
//! {
//!   "defs": [{
//!     "kind": "object", "name": "User", "span": [0, 4], "subject": "type:User",
//!     "fields": [{"name": "name", "span": [8, 12], "type_name": "String",
//!                 "required": true, "directives": [], "subject": "type:User.field:name"}],
//!     "implements": [], "union_members": [], "enum_values": [], "directives": []
//!   }],
//!   "uses": [{"name": "Post", "span": [22, 26]}],
//!   "directives_defined": [], "directives_used": ["deprecated"],
//!   "has_entry_block": false,
//!   "roots": [["query", "QueryRoot"]]
//! }
//! ```
//!
//! This vocabulary is exactly what third-party rule modules receive in their
//! run envelope (see the root README, "Authoring a rule").

use crate::ast::{DefKind, Document};
use crate::model::{BUILTIN_SCALARS, SchemaModel};
use omni_core::SourceId;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Stable wire name of a definition kind.
pub fn kind_str(kind: DefKind) -> &'static str {
    match kind {
        DefKind::Schema => "schema",
        DefKind::Scalar => "scalar",
        DefKind::Object => "object",
        DefKind::Interface => "interface",
        DefKind::Input => "input",
        DefKind::Enum => "enum",
        DefKind::Union => "union",
        DefKind::Directive => "directive",
    }
}

fn span_json(s: omni_core::Span) -> Value {
    json!([s.start, s.end])
}

/// Facts for one document (no `path` keys — the envelope adds those).
pub fn file_facts(doc: &Document) -> Value {
    let mut defs = Vec::new();
    let mut uses = Vec::new();
    let mut directives_defined = Vec::new();
    let mut directives_used = Vec::new();
    let mut has_entry_block = false;
    let mut roots: Vec<(String, String)> = Vec::new();

    for def in &doc.defs {
        if def.kind == DefKind::Directive {
            directives_defined.push(def.name.clone());
        }
        if def.kind == DefKind::Schema {
            has_entry_block = true;
            for (op, ty) in &def.root_ops {
                roots.push((op.clone(), ty.named.clone()));
                if !BUILTIN_SCALARS.contains(&ty.named.as_str()) {
                    uses.push(json!({ "name": ty.named, "span": span_json(ty.named_span) }));
                }
            }
        }

        let mut fields = Vec::new();
        for f in &def.fields {
            for d in &f.directives {
                directives_used.push(d.name.clone());
            }
            if !BUILTIN_SCALARS.contains(&f.ty.named.as_str()) {
                uses.push(json!({ "name": f.ty.named, "span": span_json(f.ty.named_span) }));
            }
            for a in &f.args {
                if !BUILTIN_SCALARS.contains(&a.ty.named.as_str()) {
                    uses.push(json!({ "name": a.ty.named, "span": span_json(a.ty.named_span) }));
                }
            }
            fields.push(json!({
                "name": f.name,
                "span": span_json(f.name_span),
                "type_name": f.ty.named,
                "required": f.ty.is_required(),
                "directives": f.directives.iter().map(|d| &d.name).collect::<Vec<_>>(),
                "subject": format!("type:{}.field:{}", def.name, f.name),
            }));
        }
        for iface in &def.implements {
            if !BUILTIN_SCALARS.contains(&iface.as_str()) {
                uses.push(json!({ "name": iface, "span": span_json(def.name_span) }));
            }
        }
        for member in &def.union_members {
            uses.push(json!({ "name": member, "span": span_json(def.name_span) }));
        }
        for d in &def.directives {
            directives_used.push(d.name.clone());
        }

        defs.push(json!({
            "kind": kind_str(def.kind),
            "name": def.name,
            "span": span_json(def.name_span),
            "subject": format!("type:{}", def.name),
            "fields": fields,
            "implements": def.implements,
            "union_members": def.union_members,
            "enum_values": def.enum_values,
            "directives": def.directives.iter().map(|d| &d.name).collect::<Vec<_>>(),
        }));
    }

    let mut out = Map::new();
    out.insert("defs".into(), Value::Array(defs));
    out.insert("uses".into(), Value::Array(uses));
    out.insert("directives_defined".into(), json!(directives_defined));
    out.insert("directives_used".into(), json!(directives_used));
    out.insert("has_entry_block".into(), json!(has_entry_block));
    out.insert("roots".into(), json!(roots.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>()));
    Value::Object(out)
}

/// Cross-file facts under capability `graphql.workspace`: same vocabulary as
/// [`file_facts`] with a `path` on every `def`/`use`, computed from the
/// workspace schema model.
pub fn workspace_facts(model: &SchemaModel, path_of: &BTreeMap<SourceId, String>) -> Value {
    let path = |id: SourceId| path_of.get(&id).cloned().unwrap_or_default();

    let mut defs = Vec::new();
    for entries in model.types.values() {
        for e in entries {
            defs.push(json!({
                "kind": kind_str(e.kind),
                "name": e.name,
                "span": span_json(e.name_span),
                "path": path(e.source),
                "subject": format!("type:{}", e.name),
                "fields": e.fields.iter().map(|f| json!({
                    "name": f.name,
                    "span": span_json(f.name_span),
                    "type_name": f.type_name,
                    "required": f.required,
                    "directives": f.directives,
                    "subject": f.subject,
                })).collect::<Vec<_>>(),
                "implements": e.implements.iter().collect::<Vec<_>>(),
                "union_members": e.union_members,
                "enum_values": e.enum_values,
                "directives": e.directives,
            }));
        }
    }

    let mut uses = Vec::new();
    for (name, spans) in &model.used {
        for (i, span) in spans.iter().enumerate() {
            let src = model
                .used_files
                .get(name)
                .and_then(|fs| fs.get(i).copied())
                .unwrap_or(SourceId(0));
            uses.push(json!({
                "name": name,
                "span": span_json(*span),
                "path": path(src),
            }));
        }
    }

    let mut out = Map::new();
    out.insert("defs".into(), Value::Array(defs));
    out.insert("uses".into(), Value::Array(uses));
    out.insert(
        "directives_defined".into(),
        json!(model.defined_directives.iter().collect::<Vec<_>>()),
    );
    out.insert(
        "directives_used".into(),
        json!(model.used_directives.iter().collect::<Vec<_>>()),
    );
    out.insert("has_entry_block".into(), json!(model.has_schema_block));
    out.insert(
        "roots".into(),
        json!(model
            .roots
            .iter()
            .map(|(k, v)| json!([k, v]))
            .collect::<Vec<_>>()),
    );
    Value::Object(out)
}