//! Source file representation and lazy line indexing.

use crate::diagnostic::{Located, SourceId, Span};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One input file discovered and loaded by the engine.
pub struct SourceFile {
    pub id: SourceId,
    pub path: PathBuf, // path relative to the lint root
    pub bytes: Vec<u8>,
    line_starts: OnceLock<Vec<u32>>,
}

impl SourceFile {
    pub fn new(id: SourceId, path: PathBuf, bytes: Vec<u8>) -> Self {
        SourceFile {
            id,
            path,
            bytes,
            line_starts: OnceLock::new(),
        }
    }

    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }

    /// Byte offset of the starts of each line (line 0 starts at 0).
    fn line_starts(&self) -> &[u32] {
        self.line_starts.get_or_init(|| {
            let mut v = vec![0u32];
            for (i, b) in self.bytes.iter().enumerate() {
                if *b == b'\n' {
                    v.push(i as u32 + 1);
                }
            }
            v
        })
    }

    /// Convert a byte span to a 1-based line/column location for reporting.
    pub fn locate(&self, span: Span) -> Located {
        let starts = self.line_starts();
        let line_idx = match starts.binary_search(&span.start) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let col = span.start - starts[line_idx] + 1;
        Located {
            path: self.path.display().to_string(),
            line: line_idx as u32 + 1,
            column: col,
        }
    }

    /// Extract the text of a span (lossy UTF-8).
    pub fn span_text(&self, span: Span) -> String {
        let start = (span.start as usize).min(self.bytes.len());
        let end = (span.end as usize).min(self.bytes.len());
        String::from_utf8_lossy(&self.bytes[start..end]).into_owned()
    }

    /// Byte offset of the start of the line containing `offset`.
    pub fn line_span_start(&self, offset: u32) -> u32 {
        let starts = self.line_starts();
        let i = match starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        starts[i]
    }

    /// Text of the whole line containing `offset` (for reporting).
    pub fn line_text(&self, offset: u32) -> (&str, u32) {
        let starts = self.line_starts();
        let line_idx = match starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let start = starts[line_idx] as usize;
        let end = starts
            .get(line_idx + 1)
            .map(|&e| e as usize - 1) // drop trailing \n
            .unwrap_or(self.bytes.len())
            .min(self.bytes.len());
        let text = std::str::from_utf8(&self.bytes[start..end.max(start)]).unwrap_or("");
        (text, line_idx as u32 + 1)
    }

    /// The extension, lowercase without the dot, for plugin matching.
    pub fn extension(&self) -> Option<String> {
        Path::new(&self.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
    }
}
