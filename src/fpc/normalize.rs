//! Normalizing raw compiler messages into the diagnostic model
//! (spec section 4.2 steps 2-6).
//!
//! Four jobs, in order:
//!
//! 1. **Catalog lookup** — give each message a stable code, a clearer headline,
//!    a span label and a `help:` line.
//! 2. **Point → span expansion** — widen FPC's `file(line,col)` point to the
//!    token that starts there, using the lexer.
//! 3. **Cascade suppression** — after "identifier not found" or "unit not
//!    found", drop the follow-up errors that exist only because the first one
//!    failed, and report root causes first.
//! 4. **Level remapping** — apply `[lints]` and `--deny/--warn/--allow`.

use crate::catalog::Catalog;
use crate::config::LevelSetting;
use crate::diagnostic::{Diagnostic, Level, Source, Span};
use crate::fpc::parser::{RawFpcMessage, RawKind};
use crate::lexer::{token_at_or_after, tokenize, Token};
use crate::source::SourceManager;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct NormalizeOptions {
    pub catalog: Catalog,
    /// `CODE -> level`, from `[lints]` merged with CLI flags.
    pub level_overrides: BTreeMap<String, LevelSetting>,
    /// Project root, for resolving relative paths FPC reported.
    pub base: PathBuf,
}

impl NormalizeOptions {
    pub fn new(catalog: Catalog, base: impl Into<PathBuf>) -> Self {
        NormalizeOptions {
            catalog,
            level_overrides: BTreeMap::new(),
            base: base.into(),
        }
    }

    pub fn with_override(mut self, code: &str, setting: LevelSetting) -> Self {
        self.level_overrides
            .insert(code.to_ascii_uppercase(), setting);
        self
    }

    pub fn with_overrides(mut self, overrides: BTreeMap<String, LevelSetting>) -> Self {
        for (k, v) in overrides {
            self.level_overrides.insert(k.to_ascii_uppercase(), v);
        }
        self
    }
}

#[derive(Debug, Default, Clone)]
pub struct NormalizedOutput {
    pub diagnostics: Vec<Diagnostic>,
    /// A `Fatal:` message was seen: compilation stopped.
    pub fatal: bool,
    pub suppressed_cascades: usize,
    pub suppressed_duplicates: usize,
}

/// Turn a compiler run's parsed messages into diagnostics.
pub fn normalize(
    raws: &[RawFpcMessage],
    opts: &NormalizeOptions,
    sources: &mut SourceManager,
) -> NormalizedOutput {
    let mut token_cache: HashMap<PathBuf, Vec<Token>> = HashMap::new();
    let mut out = NormalizedOutput::default();
    let mut root_symbols: HashSet<String> = HashSet::new();
    let mut seen: HashSet<(Option<PathBuf>, u32, u32, String)> = HashSet::new();

    for raw in raws {
        if !raw.is_diagnostic() {
            continue;
        }
        if raw.kind == RawKind::Fatal {
            out.fatal = true;
            // "Compilation aborted" and "There were N errors..." summarise the
            // errors already reported; they are not diagnostics of their own.
            if raw.is_compile_summary() {
                continue;
            }
        }

        let text = raw.text.trim();
        if text.is_empty() {
            continue;
        }

        // Dedupe first: an exact repeat is a duplicate, not a cascade, and
        // counting it as a cascade would misreport what happened.
        let dup_key = (raw.file.clone(), raw.line, raw.col, text.to_string());
        if !seen.insert(dup_key) {
            out.suppressed_duplicates += 1;
            continue;
        }

        // Cascade suppression: this message may exist only because an earlier
        // root cause failed. Report the cause, drop the symptom.
        //
        // `<erroneous type>` is FPC's own marker for a value whose type it could
        // not determine *because* an earlier error occurred, so a message
        // carrying it is always a cascade. It is only suppressed once something
        // has already been reported, so a lone such line is never swallowed.
        if is_erroneous_cascade(text)
            && (!root_symbols.is_empty()
                || !out.diagnostics.is_empty()
                || out.suppressed_cascades > 0)
        {
            out.suppressed_cascades += 1;
            continue;
        }
        if root_symbols.iter().any(|s| mentions_symbol(text, s)) {
            out.suppressed_cascades += 1;
            continue;
        }

        // The compiler's own message number is the stable code. Both profiles
        // request `-vq`, so ids are present in practice; deriving a code from the
        // text is only the fallback for output that carries no id.
        let code = match &raw.fpc_id {
            Some(id) => format!("F{id}"),
            None => opts
                .catalog
                .match_message(text)
                .map(|m| m.code)
                .unwrap_or_else(|| Catalog::code_for_unmatched(text)),
        };

        // Look the entry up by code and recover its arguments from the text. If
        // there is no entry, or its template does not match this wording, `info`
        // is empty and the raw compiler text is used - never a wrong headline.
        let info = opts.catalog.describe(&code, text);

        let setting = opts
            .level_overrides
            .get(&code.to_ascii_uppercase())
            .copied();
        if setting == Some(LevelSetting::Allow) {
            continue;
        }
        let level = match setting {
            Some(s) => match s.to_level() {
                Some(l) => l,
                None => continue,
            },
            None => raw.severity.unwrap_or(Level::Error),
        };

        let primary = match &raw.file {
            Some(file) if raw.line > 0 => {
                let file = display_path(file, &opts.base);
                expand_span(&file, raw.line, raw.col, sources, &mut token_cache)
            }
            Some(file) => Span::point(
                display_path(file, &opts.base),
                raw.line.max(1),
                raw.col.max(1),
            ),
            None => continue,
        };

        let mut diag = Diagnostic::new(
            code,
            level,
            match &info {
                Some(m) => m.headline.clone(),
                None => text.to_string(),
            },
            primary,
            Source::Fpc,
        );
        diag.fpc_raw = Some(raw.raw.clone());
        if let Some(m) = &info {
            if let Some(label) = &m.label {
                diag.labels.push(crate::diagnostic::Label {
                    span: diag.primary.clone(),
                    text: label.clone(),
                });
            }
            diag.help.extend(m.help.iter().cloned());
            if let Some(note) = &m.note {
                diag.notes.push(note.clone());
            }
        }

        if let Some(symbol) = root_cause_symbol(text) {
            root_symbols.insert(symbol);
        }
        out.diagnostics.push(diag);
    }

    sort_diagnostics(&mut out.diagnostics);
    out
}

/// Errors first, then root causes, then source order.
fn sort_diagnostics(diags: &mut [Diagnostic]) {
    diags.sort_by(|a, b| {
        a.level
            .rank()
            .cmp(&b.level.rank())
            .then_with(|| a.primary.file.cmp(&b.primary.file))
            .then_with(|| a.primary.line_start.cmp(&b.primary.line_start))
            .then_with(|| a.primary.col_start.cmp(&b.primary.col_start))
            .then_with(|| a.code.cmp(&b.code))
    });
}

/// Widen a point to the span of the token starting there.
fn expand_span(
    path: &Path,
    line: u32,
    col: u32,
    sources: &mut SourceManager,
    cache: &mut HashMap<PathBuf, Vec<Token>>,
) -> Span {
    let col = col.max(1);
    let tokens = match cache.entry(path.to_path_buf()) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(v) => {
            if !sources.contains(path) {
                let _ = sources.load_opt(path);
            }
            let Some(file) = sources.get(path) else {
                return Span::point(path.to_path_buf(), line, col);
            };
            v.insert(tokenize(&file.path, &file.text))
        }
    };
    match token_at_or_after(tokens, line, col) {
        Some(token) => token.span.clone(),
        None => Span::point(path.to_path_buf(), line, col),
    }
}

/// FPC's own marker for a value whose type it could not determine *because* an
/// earlier error occurred.
fn is_erroneous_cascade(text: &str) -> bool {
    text.contains("erroneous type")
}

/// A compiler-reported path, made relative to the project root when it is inside
/// it.
///
/// `-vb` makes FPC report absolute paths so they can always be resolved; this
/// strips the root again so diagnostics stay readable, as rustc's do. Both the
/// literal and the canonicalized root are tried, because on macOS the compiler
/// resolves symlinks (`/tmp` becomes `/private/tmp`) while the configured root
/// may not be resolved.
fn display_path(path: &Path, base: &Path) -> PathBuf {
    if base.as_os_str().is_empty() {
        return path.to_path_buf();
    }
    if let Ok(rest) = path.strip_prefix(base) {
        return rest.to_path_buf();
    }
    if let Ok(canonical) = base.canonicalize() {
        if let Ok(rest) = path.strip_prefix(&canonical) {
            return rest.to_path_buf();
        }
    }
    path.to_path_buf()
}

/// The symbol a message blames, when it is a cascade root cause.
fn root_cause_symbol(text: &str) -> Option<String> {
    const QUOTED: &[&str] = &[
        "Identifier not found \"",
        "Undeclared identifier \"",
        "Unknown identifier \"",
        "identifier not found \"",
    ];
    for pattern in QUOTED {
        if let Some(rest) = find_ci(text, pattern) {
            if let Some(end) = rest.find('"') {
                let symbol = &rest[..end];
                if !symbol.is_empty() {
                    return Some(symbol.to_string());
                }
            }
        }
    }
    if let Some(rest) = find_ci(text, "Can't find unit ") {
        let symbol = rest.split_whitespace().next().unwrap_or("");
        let symbol = symbol.trim_end_matches([',', ';']);
        if !symbol.is_empty() {
            return Some(symbol.to_string());
        }
    }
    None
}

/// Whether a message refers to `symbol`, either quoted (`"y"`, as FPC writes
/// identifiers) or as a whole word.
///
/// Substring matching alone is too loose: the cascade after an unknown `y` must
/// not swallow `got "untyped" expected "LongInt"`, which merely contains a `y`.
fn mentions_symbol(text: &str, symbol: &str) -> bool {
    if symbol.is_empty() {
        return false;
    }
    if text.contains(&format!("\"{symbol}\"")) {
        return true;
    }
    let lowered = text.to_ascii_lowercase();
    let needle = symbol.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let mut from = 0usize;
    while let Some(pos) = lowered[from..].find(&needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_word_byte(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_word_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = end.max(start + 1);
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Case-insensitive `find`, returning the text *after* the match.
fn find_ci<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    let lowered = haystack.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    lowered
        .find(&needle_lower)
        .map(|i| &haystack[i + needle.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fpc::parser::parse_output;

    const SOURCE: &str = "program hello;\nvar\n  x: Integer;\nbegin\n  Writeln(y);\nend.\n";

    fn project() -> (tempfile::TempDir, SourceManager, NormalizeOptions) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.pas"), SOURCE).unwrap();
        let sources = SourceManager::with_base(dir.path());
        let opts = NormalizeOptions::new(Catalog::builtin(), dir.path());
        (dir, sources, opts)
    }

    fn run(output: &str, sources: &mut SourceManager, opts: &NormalizeOptions) -> NormalizedOutput {
        normalize(&parse_output(output), opts, sources)
    }

    #[test]
    fn a_point_is_expanded_to_the_offending_token() {
        let (_d, mut sources, opts) = project();
        // Line 5 is "  Writeln(y);" so `y` starts at column 11.
        let out = run(
            "hello.pas(5,11) Error: Identifier not found \"y\"\n",
            &mut sources,
            &opts,
        );
        assert_eq!(out.diagnostics.len(), 1);
        let d = &out.diagnostics[0];
        assert_eq!((d.primary.line_start, d.primary.col_start), (5, 11));
        assert_eq!((d.primary.line_end, d.primary.col_end), (5, 12));
        let file = sources.get(&d.primary.file).unwrap();
        assert_eq!(file.snippet(&d.primary), "y");
    }

    #[test]
    fn catalog_gives_a_clearer_headline_label_and_help() {
        let (_d, mut sources, opts) = project();
        let out = run(
            "hello.pas(5,11) Error: Identifier not found \"y\"\n",
            &mut sources,
            &opts,
        );
        let d = &out.diagnostics[0];
        assert_eq!(d.message, "undeclared identifier `y`");
        assert_eq!(d.labels.len(), 1);
        assert!(d.labels[0].text.contains("not found"));
        assert!(!d.help.is_empty());
        assert_eq!(d.source, Source::Fpc);
        assert!(d
            .fpc_raw
            .as_deref()
            .unwrap()
            .contains("Identifier not found"));
    }

    #[test]
    fn an_unmapped_message_keeps_its_text_and_gets_a_stable_code() {
        let (_d, mut sources, opts) = project();
        let raw = "hello.pas(5,3) Error: Some brand new compiler complaint\n";
        let a = run(raw, &mut sources, &opts);
        let b = run(raw, &mut sources, &opts);
        assert_eq!(
            a.diagnostics[0].message,
            "Some brand new compiler complaint"
        );
        assert_eq!(a.diagnostics[0].code, b.diagnostics[0].code);
        assert!(a.diagnostics[0].code.starts_with('F'));
    }

    #[test]
    fn numeric_ids_win_over_derived_codes() {
        let (_d, mut sources, opts) = project();
        // 5000 is the compiler's real number for "Identifier not found", which
        // is what `-vq` makes FPC print.
        let out = run(
            "hello.pas(5,11) Error: (5000) Identifier not found \"y\"\n",
            &mut sources,
            &opts,
        );
        assert_eq!(out.diagnostics[0].code, "F5000");
        // The catalogue still improves the message.
        assert_eq!(out.diagnostics[0].message, "undeclared identifier `y`");
    }

    #[test]
    fn an_id_with_no_matching_wording_keeps_the_raw_text() {
        let (_d, mut sources, opts) = project();
        // Id 5000 paired with text from a different message: the code is still
        // the real id, but the headline must not be invented from the entry.
        let out = run(
            "hello.pas(5,11) Error: (5000) something the catalogue does not know\n",
            &mut sources,
            &opts,
        );
        assert_eq!(out.diagnostics[0].code, "F5000");
        assert_eq!(
            out.diagnostics[0].message,
            "something the catalogue does not know"
        );
    }

    #[test]
    fn cascades_after_a_root_cause_are_suppressed() {
        let (_d, mut sources, opts) = project();
        let output = "\
hello.pas(5,11) Error: Identifier not found \"y\"
hello.pas(6,1) Error: Identifier not found \"y\"
hello.pas(6,3) Error: Incompatible types: got \"untyped\" expected \"LongInt\"
";
        let out = run(output, &mut sources, &opts);
        assert_eq!(out.suppressed_cascades, 1, "{:?}", out.diagnostics);
        assert_eq!(out.diagnostics.len(), 2);
        assert!(out
            .diagnostics
            .iter()
            .all(|d| !d.message.contains("`y`") || d.primary.line_start == 5));
    }

    #[test]
    fn a_unrelated_error_is_not_suppressed() {
        let (_d, mut sources, opts) = project();
        let output = "\
hello.pas(5,11) Error: Identifier not found \"y\"
hello.pas(6,1) Error: Identifier not found \"z\"
";
        let out = run(output, &mut sources, &opts);
        assert_eq!(out.suppressed_cascades, 0);
        assert_eq!(out.diagnostics.len(), 2);
    }

    #[test]
    fn exact_duplicates_are_dropped() {
        let (_d, mut sources, opts) = project();
        let output = "\
hello.pas(5,11) Error: Identifier not found \"y\"
hello.pas(5,11) Error: Identifier not found \"y\"
";
        let out = run(output, &mut sources, &opts);
        assert_eq!(out.diagnostics.len(), 1);
        assert_eq!(out.suppressed_duplicates, 1);
    }

    #[test]
    fn errors_sort_before_warnings_and_notes() {
        let (_d, mut sources, opts) = project();
        let output = "\
hello.pas(5,11) Hint: consider something
hello.pas(5,3) Warning: be careful
hello.pas(5,1) Error: bad
";
        let out = run(output, &mut sources, &opts);
        let levels: Vec<Level> = out.diagnostics.iter().map(|d| d.level).collect();
        assert_eq!(levels, vec![Level::Error, Level::Warning, Level::Help]);
    }

    #[test]
    fn compile_summaries_are_not_emitted_but_mark_the_run_fatal() {
        let (_d, mut sources, opts) = project();
        // Both closing forms are summaries of the errors already reported, so
        // neither becomes a diagnostic; the run is still marked fatal.
        let output = "\
hello.pas(5,11) Error: (5000) Identifier not found \"y\"
hello.pas(6,1) Fatal: (10026) There were 1 errors compiling module, stopping
Fatal: (1018) Compilation aborted
";
        let out = run(output, &mut sources, &opts);
        assert!(out.fatal);
        assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
        assert_eq!(out.diagnostics[0].code, "F5000");
        assert!(!out.diagnostics.iter().any(|d| {
            let message = d.message.to_lowercase();
            message.contains("compilation aborted") || message.contains("compiling module")
        }));
    }

    #[test]
    fn a_banner_and_progress_lines_produce_no_diagnostics() {
        let (_d, mut sources, opts) = project();
        let output = "\
Free Pascal Compiler version 3.2.2 [2021/05/16] for x86_64
Compiling hello.pas
Linking hello
5 lines compiled, 0.0 sec
";
        let out = run(output, &mut sources, &opts);
        assert!(out.diagnostics.is_empty());
        assert!(!out.fatal);
    }

    #[test]
    fn allow_override_drops_a_diagnostic() {
        let (_d, mut sources, opts) = project();
        let opts = opts.with_override("F5000", LevelSetting::Allow);
        let out = run(
            "hello.pas(5,11) Error: (5000) Identifier not found \"y\"\n",
            &mut sources,
            &opts,
        );
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn warn_override_downgrades_an_error() {
        let (_d, mut sources, opts) = project();
        let opts = opts.with_override("F5000", LevelSetting::Warn);
        let out = run(
            "hello.pas(5,11) Error: (5000) Identifier not found \"y\"\n",
            &mut sources,
            &opts,
        );
        assert_eq!(out.diagnostics[0].level, Level::Warning);
    }

    #[test]
    fn deny_override_promotes_a_hint() {
        let (_d, mut sources, opts) = project();
        // 5037 is the real number for "Variable does not seem to be initialized",
        // which FPC emits as a warning; promote it to an error.
        let text = "Variable \"x\" does not seem to be initialized";
        let out = run(
            &format!("hello.pas(5,1) Warning: (5037) {text}\n"),
            &mut sources,
            &opts.with_override("F5037", LevelSetting::Deny),
        );
        assert_eq!(out.diagnostics[0].code, "F5037");
        assert_eq!(out.diagnostics[0].level, Level::Error);
    }

    #[test]
    fn mention_matching_needs_a_word_boundary_or_quotes() {
        // `untyped` contains a `y` but does not mention the symbol `y`.
        assert!(!mentions_symbol(
            "got \"untyped\" expected \"LongInt\"",
            "y"
        ));
        assert!(mentions_symbol("Identifier not found \"y\"", "y"));
        assert!(mentions_symbol("cannot use y here", "y"));
        assert!(mentions_symbol(
            "Can't find unit SysUtils used by x",
            "SysUtils"
        ));
        assert!(!mentions_symbol("anything at all", ""));
    }

    #[test]
    fn unit_not_found_is_a_root_cause() {
        assert_eq!(
            root_cause_symbol("Can't find unit SysUtils used by hello"),
            Some("SysUtils".to_string())
        );
        assert_eq!(
            root_cause_symbol("Identifier not found \"Widget\""),
            Some("Widget".to_string())
        );
        assert_eq!(root_cause_symbol("Something else entirely"), None);
    }

    #[test]
    fn a_missing_source_file_still_yields_a_usable_diagnostic() {
        let (_d, mut sources, opts) = project();
        let out = run("not/here.pas(3,2) Error: boom\n", &mut sources, &opts);
        assert_eq!(out.diagnostics.len(), 1);
        // Falls back to the raw point rather than dropping the diagnostic.
        assert_eq!(
            (
                out.diagnostics[0].primary.line_start,
                out.diagnostics[0].primary.col_start
            ),
            (3, 2)
        );
    }

    #[test]
    fn a_column_zero_location_is_handled() {
        let (_d, mut sources, opts) = project();
        let out = run(
            "hello.pas(5) Error: Identifier not found \"y\"\n",
            &mut sources,
            &opts,
        );
        assert_eq!(out.diagnostics.len(), 1);
        let d = &out.diagnostics[0];
        assert_eq!(d.primary.line_start, 5);
        assert!(d.primary.col_start >= 1);
    }

    #[test]
    fn a_locationless_error_is_skipped_not_rendered_at_1_1() {
        let (_d, mut sources, opts) = project();
        let out = run("Error: something global went wrong\n", &mut sources, &opts);
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn empty_output_is_clean() {
        let (_d, mut sources, opts) = project();
        let out = run("", &mut sources, &opts);
        assert!(out.diagnostics.is_empty());
        assert!(!out.fatal);
    }

    #[test]
    fn erroneous_type_cascades_are_suppressed_after_a_root_cause() {
        let (_d, mut sources, opts) = project();
        // 3079 is what FPC says when a `with` expression failed: the type is
        // unknown because of the earlier error, so this is a cascade.
        let output = "\
hello.pas(5,11) Error: (5000) Identifier not found \"y\"
hello.pas(5,20) Error: (3079) Expression type must be class or record type, got <erroneous type>
";
        let out = run(output, &mut sources, &opts);
        assert_eq!(out.suppressed_cascades, 1);
        assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
        assert_eq!(out.diagnostics[0].code, "F5000");
    }

    #[test]
    fn an_erroneous_type_message_alone_is_not_swallowed() {
        let (_d, mut sources, opts) = project();
        // With nothing reported before it, an `<erroneous type>` message is all
        // we have and must not be dropped.
        let out = run(
            "hello.pas(5,11) Error: (3079) Expression type must be class or record type, got <erroneous type>\n",
            &mut sources,
            &opts,
        );
        assert_eq!(out.diagnostics.len(), 1, "{:?}", out.diagnostics);
    }

    #[test]
    fn absolute_paths_from_the_compiler_are_made_relative_to_the_root() {
        assert_eq!(
            display_path(Path::new("/proj/src/a.pas"), Path::new("/proj")),
            PathBuf::from("src/a.pas")
        );
        assert_eq!(
            display_path(Path::new("/elsewhere/a.pas"), Path::new("/proj")),
            PathBuf::from("/elsewhere/a.pas")
        );
        assert_eq!(
            display_path(Path::new("already/relative.pas"), Path::new("/proj")),
            PathBuf::from("already/relative.pas")
        );
    }

    #[test]
    fn a_full_path_from_the_compiler_resolves_to_a_real_file() {
        // `-vb` makes FPC report an absolute path; the normalizer strips the
        // root so the span is readable *and* still loadable.
        let (dir, mut sources, opts) = project();
        let absolute = dir.path().join("hello.pas");
        let output = format!(
            "{}(5,11) Error: (5000) Identifier not found \"y\"\n",
            absolute.display()
        );
        let out = run(&output, &mut sources, &opts);
        assert_eq!(out.diagnostics.len(), 1);
        let d = &out.diagnostics[0];
        assert_eq!(
            d.primary.file,
            PathBuf::from("hello.pas"),
            "{:?}",
            d.primary
        );
        // Widened from the lexer, which proves the file was actually loaded.
        assert_eq!((d.primary.col_start, d.primary.col_end), (11, 12));
    }
}
