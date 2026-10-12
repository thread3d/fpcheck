//! Source text management: line index, span extraction, and the snippet
//! information renderers need.
//!
//! FPC reports a *point*; renderers need the surrounding lines and the width of
//! the offending token. Both come from here.

use crate::diagnostic::Span;
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub text: String,
    /// Byte offset of the start of each line. Always at least one entry.
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0usize];
        for (i, b) in text.bytes().enumerate() {
            // A newline that ends the file does not start a further empty line.
            if b == b'\n' && i + 1 < text.len() {
                line_starts.push(i + 1);
            }
        }
        SourceFile { path: path.into(), text, line_starts }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// 1-based line text, without its line terminator.
    pub fn line(&self, line: u32) -> Option<&str> {
        let (start, end) = self.line_content_bytes(line)?;
        self.text.get(start..end)
    }

    /// Byte range of a 1-based line, excluding its terminator.
    pub fn line_content_bytes(&self, line: u32) -> Option<(usize, usize)> {
        let (start, end) = self.line_bytes(line)?;
        let mut e = end;
        while e > start && matches!(self.text.as_bytes()[e - 1], b'\n' | b'\r') {
            e -= 1;
        }
        Some((start, e))
    }

    /// Byte range of a 1-based line.
    pub fn line_bytes(&self, line: u32) -> Option<(usize, usize)> {
        let idx = line.checked_sub(1)? as usize;
        let start = *self.line_starts.get(idx)?;
        let end = self
            .line_starts
            .get(idx + 1)
            .copied()
            .unwrap_or(self.text.len());
        Some((start, end))
    }

    /// Byte offset of a 1-based (line, col). Columns past end-of-line clamp to
    /// the terminator, so a point reported at end-of-line is still addressable.
    pub fn offset(&self, line: u32, col: u32) -> Option<usize> {
        let (start, end) = self.line_content_bytes(line)?;
        let text = &self.text[start..end];
        let mut off = start;
        let mut seen = 1u32;
        for ch in text.chars() {
            if seen >= col {
                break;
            }
            off += ch.len_utf8();
            seen += 1;
        }
        Some(off.min(end))
    }

    /// 1-based (line, col) of a byte offset.
    pub fn location(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.text.len());
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let start = self.line_starts[line_idx];
        let col = self.text[start..offset].chars().count() as u32 + 1;
        (line_idx as u32 + 1, col)
    }

    /// Exact source text covered by a span, clamped to the file.
    pub fn snippet(&self, span: &Span) -> String {
        let (Some(s), Some(e)) = (self.offset(span.line_start, span.col_start), self.offset(span.line_end, span.col_end)) else {
            return String::new();
        };
        if e <= s {
            return String::new();
        }
        self.text[s..e].to_string()
    }

    /// The lines a renderer needs for a span: everything from `line_start -
    /// before` through `line_end + after`, clamped to the file, as (line, text).
    pub fn context(&self, span: &Span, before: u32, after: u32) -> Vec<(u32, &str)> {
        let first = span.line_start.saturating_sub(before).max(1);
        let last = (span.line_end + after).min(self.line_count() as u32);
        (first..=last).filter_map(|n| self.line(n).map(|t| (n, t))).collect()
    }
}

/// Loads and caches source files so a diagnostic's file is read once.
///
/// Paths are kept exactly as the compiler reported them (usually relative, so
/// diagnostics stay readable) while reads are resolved against `base`.
#[derive(Debug, Default)]
pub struct SourceManager {
    files: HashMap<PathBuf, SourceFile>,
    base: PathBuf,
}

impl SourceManager {
    pub fn new() -> Self {
        SourceManager { files: HashMap::new(), base: PathBuf::new() }
    }

    /// A manager that resolves relative paths against `base` (the project root).
    pub fn with_base(base: impl Into<PathBuf>) -> Self {
        SourceManager { files: HashMap::new(), base: base.into() }
    }

    /// The on-disk path for a possibly-relative reported path.
    pub fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() || self.base.as_os_str().is_empty() {
            path.to_path_buf()
        } else {
            self.base.join(path)
        }
    }

    /// Read from disk (cached). A missing file is an error only when a renderer
    /// actually needs the source; callers decide.
    pub fn load(&mut self, path: &Path) -> Result<&SourceFile> {
        let key = self.resolve(path);
        if !self.files.contains_key(&key) {
            let text = std::fs::read_to_string(&key)
                .map_err(|e| Error::io(key.display().to_string(), e))?;
            self.files.insert(key.clone(), SourceFile::new(path, text));
        }
        Ok(self.files.get(&key).expect("just inserted"))
    }

    /// Same as [`SourceManager::load`] but returns `None` instead of failing, so
    /// rendering never breaks because a header moved.
    pub fn load_opt(&mut self, path: &Path) -> Option<&SourceFile> {
        self.load(path).ok()
    }

    pub fn insert(&mut self, file: SourceFile) {
        let key = self.resolve(&file.path.clone());
        self.files.insert(key, file);
    }

    pub fn get(&self, path: &Path) -> Option<&SourceFile> {
        self.files.get(&self.resolve(path))
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.files.contains_key(&self.resolve(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf() -> SourceFile {
        SourceFile::new("t.pas", "program t;\nbegin\n  x := 1;\nend.\n")
    }

    #[test]
    fn line_index_handles_trailing_newline() {
        let f = sf();
        assert_eq!(f.line_count(), 4);
        assert_eq!(f.line(1), Some("program t;"));
        assert_eq!(f.line(3), Some("  x := 1;"));
        assert_eq!(f.line(4), Some("end."));
        assert_eq!(f.line(5), None);
    }

    #[test]
    fn offset_and_location_round_trip() {
        let f = sf();
        let off = f.offset(3, 3).unwrap();
        assert_eq!(&f.text[off..off + 1], "x");
        assert_eq!(f.location(off), (3, 3));
    }

    #[test]
    fn snippet_extracts_span_text() {
        let f = sf();
        let span = Span::new("t.pas", 3, 3, 3, 4);
        assert_eq!(f.snippet(&span), "x");
    }

    #[test]
    fn multi_line_snippet_spans_newline() {
        let f = sf();
        let span = Span::new("t.pas", 2, 1, 3, 4);
        assert_eq!(f.snippet(&span), "begin\n  x");
    }

    #[test]
    fn crlf_lines_are_trimmed() {
        let f = SourceFile::new("t.pas", "a\r\nb\r\n");
        assert_eq!(f.line(1), Some("a"));
        assert_eq!(f.line(2), Some("b"));
    }

    #[test]
    fn context_clamps_to_file() {
        let f = sf();
        let span = Span::point("t.pas", 1, 1);
        let ctx = f.context(&span, 2, 2);
        assert_eq!(ctx.first().map(|(n, _)| *n), Some(1));
        assert!(ctx.len() <= 3);
    }

    #[test]
    fn columns_clamp_past_end_of_line() {
        let f = sf();
        // Column 99 on line 4 clamps to the line end, not past EOF.
        let off = f.offset(4, 99).unwrap();
        assert!(off <= f.text.len());
        assert_eq!(&f.text[off..], "\n");
    }

    #[test]
    fn manager_errors_on_missing_file() {
        let mut m = SourceManager::new();
        assert!(m.load(Path::new("/definitely/not/here.pas")).is_err());
        assert!(m.load_opt(Path::new("/definitely/not/here.pas")).is_none());
    }
}
