//! Tokenizer for GraphQL SDL (type system documents).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Name,
    Punct,
    /// { } ( ) [ ] ! : , = | & @ $ . spread dots
    StringLit,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokKind,
    /// Byte span of this token.
    pub span: (u32, u32),
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct LexError {
    pub message: String,
    pub span: (u32, u32),
}

/// Tokenize SDL. Comments (`#`) are skipped. Strings keep their quotes in
/// `text` but the span covers the whole literal.
pub fn tokenize(src: &str) -> Result<Vec<Token>, Vec<LexError>> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut toks = Vec::new();
    let mut errors = Vec::new();
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' | b',' | 0xEF => {
                if c == 0xEF {
                    // skip a UTF-8 BOM / multi-byte char conservatively
                    while i < b.len() && (b[i] & 0xC0) == 0x80 {
                        i += 1;
                    }
                }
                i += 1;
            }
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                let start = i;
                i += 1;
                if i + 2 < b.len() && &b[i..i + 2] == b"\"" {
                    // triple-quoted block string
                    i += 2;
                    while i + 2 < b.len() && &b[i..i + 3] != b"\"\"\"" {
                        i += 1;
                    }
                    i = (i + 3).min(b.len());
                } else {
                    while i < b.len() && b[i] != b'"' && b[i] != b'\n' {
                        if b[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    if i < b.len() && b[i] == b'"' {
                        i += 1;
                    } else {
                        errors.push(LexError {
                            message: "unterminated string literal".into(),
                            span: (start as u32, i as u32),
                        });
                        break;
                    }
                }
                toks.push(Token {
                    kind: TokKind::StringLit,
                    span: (start as u32, i as u32),
                    text: String::from_utf8_lossy(&b[start..i]).into_owned(),
                });
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                toks.push(Token {
                    kind: TokKind::Name,
                    span: (start as u32, i as u32),
                    text: String::from_utf8_lossy(&b[start..i]).into_owned(),
                });
            }
            b'0'..=b'9' | b'-' | b'+' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                    i += 1;
                }
                toks.push(Token {
                    kind: TokKind::Name,
                    span: (start as u32, i as u32),
                    text: String::from_utf8_lossy(&b[start..i]).into_owned(),
                });
            }
            b'.' => {
                let start = i;
                if i + 2 < b.len() && &b[i..i + 3] == b"..." {
                    i += 3;
                } else {
                    i += 1;
                    errors.push(LexError {
                        message: "unexpected `.`".into(),
                        span: (start as u32, i as u32),
                    });
                }
                toks.push(Token {
                    kind: TokKind::Punct,
                    span: (start as u32, i as u32),
                    text: "...".into(),
                });
            }
            _ => {
                toks.push(Token {
                    kind: TokKind::Punct,
                    span: (i as u32, i as u32 + 1),
                    text: (c as char).to_string(),
                });
                i += 1;
            }
        }
    }
    if errors.is_empty() {
        Ok(toks)
    } else {
        Err(errors)
    }
}
