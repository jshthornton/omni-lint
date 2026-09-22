//! Typed AST for the subset of GraphQL SDL we model.

use omni_core::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Schema,
    Scalar,
    Object,
    Interface,
    Input,
    Enum,
    Union,
    Directive,
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: String,
    pub name_span: Span,
    pub span: Span,
    pub ty: TypeRef,
    pub args: Vec<ArgDef>,
    pub directives: Vec<AppliedDirective>,
}

#[derive(Debug, Clone)]
pub struct ArgDef {
    pub name: String,
    pub ty: TypeRef,
    pub has_default: bool,
}

#[derive(Debug, Clone)]
pub struct AppliedDirective {
    pub name: String,
    pub name_span: Span,
}

#[derive(Debug, Clone)]
pub struct TypeRef {
    /// Named inner type name.
    pub named: String,
    /// span of the whole type expression.
    pub span: Span,
    /// span of the named type itself.
    pub named_span: Span,
    /// Outer non-null (`!` at the end).
    pub non_null: bool,
}

impl TypeRef {
    pub fn is_required(&self) -> bool {
        self.non_null
    }
}

#[derive(Debug, Clone)]
pub struct Def {
    pub kind: DefKind,
    pub name: String,
    pub name_span: Span,
    pub span: Span,
    /// Object/Interface/Input fields.
    pub fields: Vec<FieldDef>,
    /// Interfaces an object/interface implements.
    pub implements: Vec<String>,
    /// Union members.
    pub union_members: Vec<String>,
    /// Enum values.
    pub enum_values: Vec<String>,
    /// Applied directives at def level.
    pub directives: Vec<AppliedDirective>,
    /// Schema root operation map, e.g. `query` -> `Query`.
    pub root_ops: Vec<(String, TypeRef)>,
}

impl Def {
    pub fn is_fielded(&self) -> bool {
        matches!(
            self.kind,
            DefKind::Object | DefKind::Interface | DefKind::Input
        )
    }
}

/// The parsed SDL document.
#[derive(Debug, Default)]
pub struct Document {
    pub defs: Vec<Def>,
}
