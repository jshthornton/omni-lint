//! The GraphQL language plugin: parses `.graphql`/`.gql` files and exposes
//! the AST and facts capabilities. Pure AST provider — rules are separate
//! units (native in `rules/`, or independent WASM rule modules).
//!
//! Capabilities:
//!   - `graphql.ast` (file)      — typed `Document` (native rules)
//!   - `graphql.facts` (file)    — JSON facts view (WASM rules)
//!   - `graphql.schema` (wks)    — typed `SchemaModel` (native rules)
//!   - `graphql.workspace` (wks) — JSON cross-file facts (WASM rules)

use crate::ast::Document;
use crate::model::SchemaModel;
use crate::parser;
use omni_core::plugin::{Capability, CapabilityScope, ParsedFile, Plugin, WorkspaceConfig};
use omni_core::SourceFile;
use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct GraphqlPlugin;

impl Plugin for GraphqlPlugin {
    fn id(&self) -> &'static str {
        "graphql"
    }

    fn describe(&self) -> &'static str {
        "GraphQL SDL parser, AST and facts model (crate omni-graphql)"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["graphql", "gql"]
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability {
                name: "graphql.ast",
                scope: CapabilityScope::File,
            },
            Capability {
                name: "graphql.facts",
                scope: CapabilityScope::File,
            },
            Capability {
                name: "graphql.schema",
                scope: CapabilityScope::Workspace,
            },
            Capability {
                name: "graphql.workspace",
                scope: CapabilityScope::Workspace,
            },
        ]
    }

    /// Parse one SDL file. All files must parse independently; cross-file
    /// linking happens in `build_workspace`.
    fn parse_file(&self, source: Arc<SourceFile>) -> ParsedFile {
        let src = source.text().to_string();
        let (doc, mut errors) = parser::parse_for(source.id, &src);
        // The lexer/parser report at most a bounded number of errors per file.
        errors.truncate(50);
        let mut artifacts: BTreeMap<String, Arc<dyn Any + Send + Sync>> = BTreeMap::new();
        // JSON facts view first: the surface third-party WASM rules consume.
        let facts = crate::facts::file_facts(&doc);
        artifacts.insert("graphql.facts".to_string(), Arc::new(facts));
        artifacts.insert("graphql.ast".to_string(), Arc::new(doc));
        ParsedFile {
            source,
            artifacts,
            findings: Vec::new(),
            errors,
        }
    }

    /// Build the workspace schema model from every parsed document.
    fn build_workspace(
        &self,
        files: &[ParsedFile],
        _config: &WorkspaceConfig,
    ) -> Result<BTreeMap<String, Arc<dyn Any + Send + Sync>>, String> {
        let docs: Vec<(omni_core::SourceId, &Document)> = files
            .iter()
            .map(|f| {
                let doc: &Document = f
                    .artifact::<Document>("graphql.ast")
                    .expect("parse_file always produces graphql.ast");
                (f.source.id, doc)
            })
            .collect();
        let model: SchemaModel = crate::model::build_model(&docs);
        let path_of: BTreeMap<omni_core::SourceId, String> = files
            .iter()
            .map(|f| (f.source.id, f.source.path.display().to_string()))
            .collect();
        let mut out: BTreeMap<String, Arc<dyn Any + Send + Sync>> = BTreeMap::new();
        let workspace_facts = crate::facts::workspace_facts(&model, &path_of);
        out.insert("graphql.workspace".to_string(), Arc::new(workspace_facts));
        out.insert("graphql.schema".to_string(), Arc::new(model));
        Ok(out)
    }
}
