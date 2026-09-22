mod ast;
mod lexer;
mod model;
pub mod parser;
mod plugin;
pub mod rules;

pub use ast::*;
pub use model::SchemaModel;
pub use plugin::GraphqlPlugin;

#[cfg(test)]
mod tests {
    use super::parser::parse_document;
    use crate::ast::DefKind;

    #[test]
    fn parses_basic_object() {
        let (doc, errs) = parse_document("type User {\n  name: String!\n  posts: [Post!]!\n}");
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(doc.defs.len(), 1);
        let def = &doc.defs[0];
        assert_eq!(def.kind, DefKind::Object);
        assert_eq!(def.name, "User");
        assert_eq!(def.fields.len(), 2);
        assert_eq!(def.fields[0].ty.named, "String");
        assert!(def.fields[0].ty.non_null);
        assert_eq!(def.fields[1].ty.named, "Post");
    }

    #[test]
    fn parses_directives_applied() {
        let (doc, errs) = parse_document(
            "input CreatePostInput {\n  old: String @deprecated(reason: \"old\")\n  req: String!\n}",
        );
        assert!(errs.is_empty(), "{errs:?}");
        let def = &doc.defs[0];
        assert_eq!(def.kind, DefKind::Input);
        assert_eq!(def.fields[0].directives.len(), 1);
        assert_eq!(def.fields[0].directives[0].name, "deprecated");
        assert!(def.fields[1].ty.non_null);
    }

    #[test]
    fn recovers_when_block_is_unclosed() {
        // Unclosed `type Broken` block: the next definition still parses.
        let (doc, errs) = parse_document("type Broken {\n  name: String\n\ntype Ok {\n  x: Int\n}\n");
        assert_eq!(doc.defs.iter().find(|d| d.name == "Ok").map(|d| d.fields.len()), Some(1));
        assert!(errs.len() >= 1, "an unclosed field block must report a diagnostic");
    }

    #[test]
    fn parses_union_and_enum() {
        let (doc, errs) = parse_document("union Search = User | Post\nenum Status { ACTIVE INACTIVE }");
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(doc.defs[0].union_members, vec!["User", "Post"]);
        assert_eq!(doc.defs[1].enum_values, vec!["ACTIVE", "INACTIVE"]);
    }

    #[test]
    fn parses_schema_block() {
        let (doc, errs) = parse_document("schema {\n  query: QueryRoot\n}");
        assert!(errs.is_empty(), "{errs:?}");
        let schema = &doc.defs[0];
        assert_eq!(schema.root_ops.len(), 1);
        assert_eq!(schema.root_ops[0].0, "query");
        assert_eq!(schema.root_ops[0].1.named, "QueryRoot");
    }
}
