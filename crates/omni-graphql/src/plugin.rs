//! The GraphQL domain plugin: parses `.graphql`/`.gql` files, exposes
//! `graphql.ast` (file) and `graphql.schema` (workspace), registers rules.

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
        "GraphQL SDL parser and schema model (crate omni-graphql)"
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
                name: "graphql.schema",
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
        let mut out: BTreeMap<String, Arc<dyn Any + Send + Sync>> = BTreeMap::new();
        out.insert("graphql.schema".to_string(), Arc::new(model));
        Ok(out)
    }
}
