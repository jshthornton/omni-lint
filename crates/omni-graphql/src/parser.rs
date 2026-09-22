//! Parser for GraphQL SDL (type system definitions).
//!
//! Tolerant: on a local failure a diagnostic is produced and parsing resumes
//! at the next type keyword so the rest of the file still lints.

use crate::ast::*;
use crate::lexer::{TokKind, Token, tokenize};
use omni_core::{Diagnostic, Severity, Span, SourceId};

#[derive(Debug)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

pub fn parse_document(src: &str) -> (Document, Vec<ParseError>) {
    match tokenize(src) {
        Err(lex_errors) => {
            let doc = Document::default();
            let errs = lex_errors
                .into_iter()
                .map(|e| ParseError {
                    message: e.message,
                    span: Span::new(e.span.0, e.span.1),
                })
                .collect();
            return (doc, errs);
        }
        Ok(toks) => Parser {
            toks,
            pos: 0,
            doc: Document::default(),
            errors: Vec::new(),
            pending_def_fields: Vec::new(),
        }
        .parse(),
    }
}

pub fn parse_for(source_id: SourceId, src: &str) -> (Document, Vec<Diagnostic>) {
    let (doc, errors) = parse_document(src);
    let diags = errors
        .into_iter()
        .map(|e| Diagnostic {
            rule_id: "graphql/parse-error".into(),
            severity: Severity::Error,
            message: e.message,
            source_id,
            span: e.span,
            related: vec![],
            subject: None,
        })
        .collect();
    (doc, diags)
}

const TYPE_KEYWORDS: [&str; 7] = [
    "type", "interface", "input", "enum", "union", "scalar", "directive",
];

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    doc: Document,
    errors: Vec<ParseError>,
    /// Fields being collected for the current fielded definition.
    pending_def_fields: Vec<FieldDef>,
}

impl Parser {
    fn parse(mut self) -> (Document, Vec<ParseError>) {
        while !self.at_end() {
            let before = self.pos;
            self.parse_definition();
            if self.pos == before {
                // no progress: force-consume a token to avoid infinite loops
                self.pos += 1;
            }
        }
        (self.doc, self.errors)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn err_here(&mut self, message: impl Into<String>) {
        let span = self
            .peek()
            .map(|t| Span::new(t.span.0, t.span.1))
            .unwrap_or_else(|| {
                self.toks
                    .last()
                    .map(|t| Span::new(t.span.1, t.span.1))
                    .unwrap_or(Span::new(0, 0))
            });
        self.errors.push(ParseError {
            message: message.into(),
            span,
        });
    }

    fn skip_to_like_definition(&mut self) {
        while let Some(t) = self.peek().cloned() {
            if t.kind == TokKind::Name && TYPE_KEYWORDS.contains(&t.text.as_str()) {
                return; // caller consumes this keyword itself
            }
            if t.text == "extend" {
                return;
            }
            self.pos += 1;
            // A brace block is part of the damaged definition: skip it whole.
            if t.text == "{" {
                self.skip_balanced("{", "}");
            }
        }
    }

    /// Skip a brace-delimited block or value.
    fn parse_definition(&mut self) {
        let Some(start_tok) = self.peek().cloned() else { return };
        if start_tok.kind != TokKind::Name {
            // Stray punctuation: skip it.
            self.pos += 1;
            return;
        }
        let start_span = Span::new(start_tok.span.0, start_tok.span.1);
        let kw = start_tok.text.clone();
        self.pos += 1;
        match kw.as_str() {
            "schema" => self.parse_schema(start_span),
            "scalar" => self.parse_scalar(start_span),
            "type" | "interface" | "input" => self.parse_fielded(&kw, start_span),
            "enum" => self.parse_enum(start_span),
            "union" => self.parse_union(start_span),
            "directive" => self.parse_directive_def(start_span),
            "extend" => self.parse_extend(),
            other => {
                self.err_here(format!(
                    "expected a type definition keyword, found `{other}`"
                ));
                self.skip_to_like_definition();
            }
        }
    }

    fn parse_extend(&mut self) {
        // `extend type Name { ... }` — merge into same-named def if present.
        let Some(kw) = self.peek().cloned() else { return };
        if kw.kind == TokKind::Name {
            self.pos += 1;
            if kw.text == "type" || kw.text == "interface" || kw.text == "input" {
                self.parse_fielded(&kw.text, Span::new(kw.span.0, kw.span.1));
            } else if kw.text == "enum" {
                self.parse_enum(Span::new(kw.span.0, kw.span.1));
            } else {
                let e = ParseError {
                    message: format!("cannot extend `{}` in SDL subset", kw.text),
                    span: Span::new(kw.span.0, kw.span.1),
                };
                self.errors.push(e);
            }
        }
    }

    fn parse_schema(&mut self, start: Span) {
        let mut ops = Vec::new();
        let mut end = start;
        // optionally: `schema { query: Query ... }` or `schema @dir`
        // applied directives
        let _dir = self.skip_applied_directives();
        while let Some(t) = self.peek() {
            if t.text != "{" {
                break;
            }
            let _open = self.next();
            while let Some(t) = self.peek() {
                if t.text == "}" {
                    self.pos += 1;
                    break;
                }
                let op = self.next().unwrap();
                if let Some(c) = self.peek() {
                    if c.text == ":" {
                        self.pos += 1;
                    }
                }
                let ty = self.parse_type_ref();
                ops.push((op.text.clone(), ty));
            }
            end = start;
        }
        if ops.is_empty() {
            // schema with no roots: tolerate
        }
        self.doc.defs.push(Def {
            kind: DefKind::Schema,
            name: "schema".into(),
            name_span: start,
            span: start,
            fields: vec![],
            implements: vec![],
            union_members: vec![],
            enum_values: vec![],
            directives: vec![],
            root_ops: ops,
        });
        let _ = end;
    }

    fn parse_scalar(&mut self, start: Span) {
        let name = self.expect_name();
        let _ = self.skip_applied_directives();
        if let Some(name) = name {
            self.doc.defs.push(simple_def(DefKind::Scalar, name, start));
        }
    }

    fn parse_fielded(&mut self, kw: &str, start: Span) {
        let kind = match kw {
            "type" => DefKind::Object,
            "interface" => DefKind::Interface,
            _ => DefKind::Input,
        };
        let name = self.expect_name();
        let mut implements = Vec::new();
        let mut directives = Vec::new();
        // fields accumulate in self.pending_def_fields
        // optional `implements I1 & I2`
        while let Some(t) = self.peek() {
            if t.text == "implements" {
                self.pos += 1;
                loop {
                    match self.peek() {
                        Some(t) if t.text == "&" => {
                            self.pos += 1;
                        }
                        Some(t) if t.kind == TokKind::Name && t.text != "{" => {
                            implements.push(std::mem::take(&mut {
                                let mut s = t.text.clone();
                                // types may be spread over multiple tokens;
                                // interfaces are simple names here.
                                let _ = &mut s;
                                s
                            }));
                            self.pos += 1;
                        }
                        _ => break,
                    }
                }
            } else if t.text == "@" {
                directives.extend(self.skip_applied_directives());
            } else {
                break;
            }
        }
        if let Some(t) = self.peek() {
            if t.text == "{" {
                self.pos += 1;
                while let Some(t) = self.peek().cloned() {
                    if t.text == "}" {
                        self.pos += 1;
                        break;
                    }
                    // A definition keyword inside the field block means the
                    // block was unclosed: leave it to the outer loop.
                    if t.kind == TokKind::Name && TYPE_KEYWORDS.contains(&t.text.as_str()) {
                        self.errors.push(ParseError {
                            message: format!(
                                "`{}` block is not closed: found `{}` where `}}` was expected",
                                name.as_deref().unwrap_or("definition"),
                                t.text
                            ),
                            span: Span::new(t.span.0, t.span.1),
                        });
                        break;
                    }
                    let before = self.pos;
                    self.parse_field(kind);
                    if self.pos == before {
                        self.pos += 1;
                    }
                }
            } else if t.kind != TokKind::Name || !TYPE_KEYWORDS.contains(&t.text.as_str()) {
                // Not a clean end (e.g. junk); report but keep the def.
                self.err_here(format!("expected `{{` to begin {} `{}`", kw, name.as_deref().unwrap_or("?")));
                self.pos += 1;
            }
        }
        if let Some(name) = name {
            self.doc.defs.push(Def {
                kind,
                name,
                name_span: start,
                span: start,
                fields: std::mem::take(&mut self.pending_def_fields),
                implements,
                union_members: vec![],
                enum_values: vec![],
                directives,
                root_ops: vec![],
            });
        }
    }

    fn parse_field(&mut self, def_kind: DefKind) {
        let Some(name_tok) = self.peek().cloned() else { return };
        let name = name_tok.text.clone();
        let name_span = Span::new(name_tok.span.0, name_tok.span.1);
        let field_start = name_span;
        self.pos += 1;
        let mut args = Vec::new();
        if let Some(t) = self.peek() {
            if t.text == "(" {
                self.pos += 1;
                while let Some(t) = self.peek() {
                    if t.text == ")" {
                        self.pos += 1;
                        break;
                    }
                    let before = self.pos;
                    args.extend(self.parse_input_value());
                    if self.pos == before {
                        self.pos += 1;
                    }
                }
            }
        }
        let Some(colon) = self.peek() else {
            self.err_here(format!("field `{name}` is missing a type"));
            return;
        };
        if colon.text != ":" {
            self.err_here(format!("expected `:` after field `{name}`"));
            return;
        }
        self.pos += 1;
        let ty = self.parse_type_ref();
        let directives = self.skip_applied_directives();
        let end = directives
            .last()
            .map(|d| d.name_span.end)
            .unwrap_or(ty.span.end);
        // Tolerate trailing non-null `!` and default `= expr` on any def kind.
        while let Some(t) = self.peek() {
            match t.text.as_str() {
                "=" => {
                    self.pos += 1;
                    self.skip_value();
                }
                "!" => self.pos += 1,
                _ => break,
            }
        }
        let _ = def_kind;
        if !valid_name(&name) && !name.starts_with("__") {
            self.err_here(format!(
                "field name `{name}` is not a valid GraphQL name"
            ));
        }
        let _ = field_start;
        self.pending_def_fields.push(FieldDef {
            name,
            name_span,
            span: Span::new(name_span.start, end),
            ty,
            args,
            directives,
        });
    }

    fn parse_type_ref(&mut self) -> TypeRef {
        let mut non_null = false;
        let mut start = None;
        let mut end = 0;
        let mut depth = 0usize;
        // consume `[`, Name, `]`, `!`
        let mut named = String::new();
        let mut named_span = Span::new(0, 0);
        while let Some(t) = self.peek() {
            match t.text.as_str() {
                "[" => {
                    depth += 1;
                    if start.is_none() {
                        start = Some(t.span.0);
                    }
                    end = t.span.1;
                    self.pos += 1;
                }
                "!" => {
                    non_null = true;
                    end = t.span.1;
                    self.pos += 1;
                }
                "]" => {
                    depth = depth.saturating_sub(1);
                    end = t.span.1;
                    self.pos += 1;
                }
                _ if t.kind == TokKind::Name => {
                    named = t.text.clone();
                    named_span = Span::new(t.span.0, t.span.1);
                    if start.is_none() {
                        start = Some(t.span.0);
                    }
                    end = t.span.1;
                    self.pos += 1;
                    break;
                }
                _ => break,
            }
        }
        // after the name, allow `!]` chains
        while let Some(t) = self.peek() {
            match t.text.as_str() {
                "]" => {
                    depth = depth.saturating_sub(1);
                    end = t.span.1;
                    self.pos += 1;
                }
                "!" => {
                    non_null = true;
                    end = t.span.1;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        TypeRef {
            named,
            span: Span::new(start.unwrap_or(0), end),
            named_span,
            non_null,
        }
    }

    fn parse_input_value(&mut self) -> Option<ArgDef> {
        let Some(name_tok) = self.peek().cloned() else { return None };
        let name = name_tok.text.clone();
        self.pos += 1;
        if let Some(t) = self.peek() {
            if t.text == ":" {
                self.pos += 1;
            }
        }
        let ty = self.parse_type_ref();
        let mut has_default = false;
        while let Some(t) = self.peek() {
            match t.text.as_str() {
                "=" => {
                    has_default = true;
                    self.pos += 1;
                    self.skip_value();
                }
                "!" => self.pos += 1,
                _ => break,
            }
        }
        let _ = self.skip_applied_directives();
        Some(ArgDef {
            name,
            ty,
            has_default,
        })
    }

    /// Consume one value literal (rough — only enough for SDL defaults).
    fn skip_value(&mut self) {
        let Some(t) = self.peek() else { return };
        match t.text.as_str() {
            "[" => self.skip_balanced("[", "]"),
            "{" => self.skip_balanced("{", "}"),
            _ => self.pos += 1,
        }
    }

    fn skip_balanced(&mut self, open: &str, close: &str) {
        let mut depth = 0usize;
        let mut seen_open = false;
        while let Some(t) = self.next() {
            if t.text == open {
                seen_open = true;
                depth += 1;
            } else if t.text == close {
                if !seen_open {
                    // Stray close before any open in this region: leave it
                    // consumed and stop so the next definition can start.
                    return;
                }
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return;
                }
            }
        }
    }

    fn parse_enum(&mut self, start: Span) {
        let name = self.expect_name();
        let mut values = Vec::new();
        let mut directives = Vec::new();
        while let Some(t) = self.peek() {
            if t.text == "@" {
                directives.extend(self.skip_applied_directives());
                continue;
            }
            if t.text != "{" {
                break;
            }
            self.pos += 1;
            while let Some(t) = self.peek() {
                if t.text == "}" {
                    self.pos += 1;
                    break;
                }
                if t.kind == TokKind::Name {
                    values.push(t.text.clone());
                }
                self.pos += 1;
                let _ = self.skip_applied_directives();
            }
        }
        if let Some(name) = name {
            self.doc.defs.push(Def {
                kind: DefKind::Enum,
                name,
                name_span: start,
                span: start,
                fields: vec![],
                implements: vec![],
                union_members: vec![],
                enum_values: values,
                directives,
                root_ops: vec![],
            });
        }
    }

    fn parse_union(&mut self, start: Span) {
        let name = self.expect_name();
        let mut members = Vec::new();
        while let Some(t) = self.peek() {
            match t.text.as_str() {
                "=" | "|" => {
                    self.pos += 1;
                }
                _ if t.kind == TokKind::Name && !TYPE_KEYWORDS.contains(&t.text.as_str()) => {
                    members.push(t.text.clone());
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let _ = self.skip_applied_directives();
        if let Some(name) = name {
            self.doc.defs.push(Def {
                kind: DefKind::Union,
                name,
                name_span: start,
                span: start,
                fields: vec![],
                implements: vec![],
                union_members: members,
                enum_values: vec![],
                directives: vec![],
                root_ops: vec![],
            });
        }
    }

    fn parse_directive_def(&mut self, start: Span) {
        // `directive @name(args) repeatable? on LOC1 | LOC2`
        let mut name = None;
        if let Some(t) = self.peek() {
            if t.text == "@" {
                self.pos += 1;
                name = self.expect_name();
            }
        }
        while let Some(t) = self.peek() {
            match t.text.as_str() {
                "(" => self.skip_balanced("(", ")"),
                "repeatable" | "on" | "|" => self.pos += 1,
                _ if t.kind == TokKind::Punct => break,
                _ => self.pos += 1,
            }
        }
        if let Some(name) = name {
            self.doc.defs.push(simple_def(DefKind::Directive, name, start));
        }
    }

    /// Consume applied directives `@name(args...)`, returning their spans.
    fn skip_applied_directives(&mut self) -> Vec<AppliedDirective> {
        let mut out = Vec::new();
        while let Some(t) = self.peek() {
            if t.text != "@" {
                break;
            }
            self.pos += 1;
            let Some(name_tok) = self.next() else { break };
            let name_start = name_tok.span.0;
            let mut end = name_tok.span.1;
            if let Some(t) = self.peek() {
                if t.text == "(" {
                    self.skip_balanced("(", ")");
                }
            }
            end = end.max(self.toks.get(self.pos.saturating_sub(1)).map(|t| t.span.1).unwrap_or(end));
            out.push(AppliedDirective {
                name: name_tok.text.clone(),
                name_span: Span::new(name_start, end),
            });
        }
        out
    }

    fn expect_name(&mut self) -> Option<String> {
        match self.peek() {
            Some(t) if t.kind == TokKind::Name => {
                let text = t.text.clone();
                self.pos += 1;
                Some(text)
            }
            Some(t) => {
                let msg = format!("expected a name, found `{}`", t.text);
                let span = Span::new(t.span.0, t.span.1);
                self.errors.push(ParseError { message: msg, span });
                None
            }
            None => {
                self.errors.push(ParseError {
                    message: "unexpected end of file".into(),
                    span: Span::new(0, 0),
                });
                None
            }
        }
    }
}


fn simple_def(kind: DefKind, name: String, start: Span) -> Def {
    Def {
        kind,
        name,
        name_span: start,
        span: start,
        fields: vec![],
        implements: vec![],
        union_members: vec![],
        enum_values: vec![],
        directives: vec![],
        root_ops: vec![],
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
