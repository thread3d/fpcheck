//! The message catalog (spec section 6.4): diagnostic text as data, not code.
//!
//! Two kinds of entry:
//!
//! * `catalog/lints.toml` — keyed by stable analyzer code (`P0702`), providing
//!   the lint's name and human-facing text.
//! * `catalog/fpc.toml` — keyed by the FPC compiler's message *template*,
//!   providing a clearer headline and a `help:` line for compiler messages.
//!
//! Both are embedded in the binary with [`include_str!`] so the tool works with
//! no files on disk, and a project `catalog/` directory can override or extend
//! them ([`Catalog::load_dir`]).
//!
//! ## Templating
//!
//! `{name}` is a substitution placeholder. To write a literal brace, double it:
//! `{{` renders as `{`. That is how annotation text such as `{@allow}` is
//! written in the data files.

use crate::diagnostic::Diagnostic;
use crate::error::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

const EMBEDDED_LINTS: &str = include_str!("../catalog/lints.toml");
const EMBEDDED_FPC: &str = include_str!("../catalog/fpc.toml");

const LINT_FILE: &str = "lints.toml";
const FPC_FILE: &str = "fpc.toml";

/// The lint catalog exactly as embedded in the binary.
///
/// The MCP server serves this as a resource (`fpcheck://catalog/lints`) when a
/// project has no `catalog/lints.toml` of its own, so the reference data is the
/// same bytes the analyzer uses.
pub fn embedded_lint_text() -> &'static str {
    EMBEDDED_LINTS
}

/// The compiler-message catalog exactly as embedded in the binary.
pub fn embedded_fpc_text() -> &'static str {
    EMBEDDED_FPC
}

/// One analyzer-lint catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LintEntry {
    pub name: String,
    pub headline: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub help: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// One compiler-message rule, as written in the data file.
#[derive(Debug, Clone, Deserialize)]
struct RawMessageEntry {
    template: String,
    #[serde(default)]
    code: Option<String>,
    headline: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    help: Vec<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFpcFile {
    #[serde(default)]
    message: Vec<RawMessageEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Capture(String),
}

/// A message template compiled for matching and rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
    source: String,
}

impl Template {
    pub fn compile(source: &str) -> Template {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut chars = source.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    if chars.peek() == Some(&'{') {
                        chars.next();
                        lit.push('{');
                    } else {
                        let mut name = String::new();
                        for c2 in chars.by_ref() {
                            if c2 == '}' {
                                break;
                            }
                            name.push(c2);
                        }
                        if !lit.is_empty() {
                            parts.push(Part::Literal(std::mem::take(&mut lit)));
                        }
                        parts.push(Part::Capture(name));
                    }
                }
                '}' => {
                    if chars.peek() == Some(&'}') {
                        chars.next();
                    }
                    lit.push('}');
                }
                other => lit.push(other),
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Literal(lit));
        }
        Template {
            parts,
            source: source.to_string(),
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Match `text`, capturing placeholder values. `None` means no match.
    ///
    /// A template is matched as a *substring* pattern (FPC prefixes many
    /// messages, e.g. `Syntax error, "x" expected but "y" found`), except that a
    /// placeholder-free template must equal the message exactly.
    pub fn match_text(&self, text: &str) -> Option<BTreeMap<String, String>> {
        if self.parts.len() == 1 {
            if let Part::Literal(l) = &self.parts[0] {
                return (text == l).then(BTreeMap::new);
            }
        }

        let mut caps = BTreeMap::new();
        let mut rest = text;
        let last = self.parts.len().saturating_sub(1);

        for (i, part) in self.parts.iter().enumerate() {
            match part {
                Part::Literal(l) => {
                    if i == last {
                        // A trailing literal must end the message.
                        rest = rest.strip_suffix(l.as_str())?;
                    } else {
                        let pos = rest.find(l.as_str())?;
                        rest = &rest[pos + l.len()..];
                    }
                }
                Part::Capture(name) => {
                    let next_lit = self.parts[i + 1..].iter().find_map(|p| match p {
                        Part::Literal(l) => Some(l.as_str()),
                        Part::Capture(_) => None,
                    });
                    match next_lit {
                        Some(l) => {
                            let pos = rest.find(l)?;
                            caps.insert(name.clone(), rest[..pos].to_string());
                            rest = &rest[pos..];
                        }
                        None => {
                            caps.insert(name.clone(), rest.to_string());
                            rest = "";
                        }
                    }
                }
            }
        }
        Some(caps)
    }

    /// Fill this template with captured values; unknown placeholders render empty.
    pub fn render(&self, caps: &BTreeMap<String, String>) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(l) => out.push_str(l),
                Part::Capture(n) => {
                    if let Some(v) = caps.get(n) {
                        out.push_str(v);
                    }
                }
            }
        }
        out
    }

    /// Render with no captures, for display of the raw template.
    pub fn render_literal(&self) -> String {
        self.render(&BTreeMap::new())
    }

    /// Render for documentation: literal braces are unescaped and placeholder
    /// names stay visible (`Identifier not found "{p1}"`), unlike
    /// [`Self::render`], which substitutes them with captured values.
    pub fn display(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(l) => out.push_str(l),
                Part::Capture(name) => {
                    out.push('{');
                    out.push_str(name);
                    out.push('}');
                }
            }
        }
        out
    }

    /// Number of literal characters, used to order templates by specificity.
    pub fn literal_len(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Literal(l) => l.chars().count(),
                Part::Capture(_) => 0,
            })
            .sum()
    }
}

/// A compiler-message rule resolved and ready to apply.
#[derive(Debug, Clone)]
pub struct MessageRule {
    pub code: String,
    pub template: Template,
    pub headline: Template,
    pub label: Option<Template>,
    pub help: Vec<Template>,
    pub note: Option<Template>,
}

/// A matched compiler message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageMatch {
    pub code: String,
    pub headline: String,
    pub label: Option<String>,
    pub help: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    lints: BTreeMap<String, LintEntry>,
    messages: Vec<MessageRule>,
    by_code: BTreeMap<String, usize>,
    /// Indices into `messages`, most specific template first, for
    /// [`Catalog::match_message`].
    match_order: Vec<usize>,
}

/// Build the rendered form of a rule from captured values.
fn render_rule(rule: &MessageRule, caps: &BTreeMap<String, String>) -> MessageMatch {
    MessageMatch {
        code: rule.code.clone(),
        headline: rule.headline.render(caps),
        label: rule.label.as_ref().map(|l| l.render(caps)),
        help: rule.help.iter().map(|h| h.render(caps)).collect(),
        note: rule.note.as_ref().map(|n| n.render(caps)),
    }
}

impl Catalog {
    /// The catalog embedded in the binary.
    pub fn builtin() -> Self {
        let lints: BTreeMap<String, LintEntry> = toml::from_str(EMBEDDED_LINTS)
            .unwrap_or_else(|e| panic!("embedded catalog/lints.toml is invalid: {e}"));
        let lints: BTreeMap<String, LintEntry> = lints
            .into_iter()
            .map(|(k, v)| (k, normalize_lint_entry(v)))
            .collect();
        let fpc: RawFpcFile = toml::from_str(EMBEDDED_FPC)
            .unwrap_or_else(|e| panic!("embedded catalog/fpc.toml is invalid: {e}"));

        let mut catalog = Catalog {
            lints,
            messages: Vec::new(),
            by_code: BTreeMap::new(),
            match_order: Vec::new(),
        };
        for raw in fpc.message {
            catalog.push_message(raw);
        }
        catalog.rebuild_match_order();
        catalog
    }

    /// Built-in catalog, extended/overridden by a project `catalog/` directory.
    pub fn load_dir(dir: &Path) -> Result<Self> {
        let mut catalog = Catalog::builtin();

        let lints_path = dir.join(LINT_FILE);
        if lints_path.is_file() {
            let text = std::fs::read_to_string(&lints_path)
                .map_err(|e| Error::io(lints_path.display().to_string(), e))?;
            let extra: BTreeMap<String, LintEntry> = toml::from_str(&text)
                .map_err(|e| Error::Catalog(format!("{}: {e}", lints_path.display())))?;
            for (code, entry) in extra {
                catalog
                    .lints
                    .insert(code.to_ascii_uppercase(), normalize_lint_entry(entry));
            }
        }

        let fpc_path = dir.join(FPC_FILE);
        if fpc_path.is_file() {
            let text = std::fs::read_to_string(&fpc_path)
                .map_err(|e| Error::io(fpc_path.display().to_string(), e))?;
            let file: RawFpcFile = toml::from_str(&text)
                .map_err(|e| Error::Catalog(format!("{}: {e}", fpc_path.display())))?;
            for raw in file.message {
                catalog.push_message(raw);
            }
            catalog.rebuild_match_order();
        }

        Ok(catalog)
    }

    /// Order templates most-specific-first so a general one cannot shadow a
    /// specific one during [`Catalog::match_message`].
    fn rebuild_match_order(&mut self) {
        let mut order: Vec<usize> = (0..self.messages.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.messages[i].template.literal_len()));
        self.match_order = order;
    }

    fn push_message(&mut self, raw: RawMessageEntry) {
        let template = Template::compile(&raw.template);
        let code = raw
            .code
            .clone()
            .unwrap_or_else(|| synthetic_code(&raw.template));
        let rule = MessageRule {
            code: code.clone(),
            template,
            headline: Template::compile(&raw.headline),
            label: raw.label.as_deref().map(Template::compile),
            help: raw.help.iter().map(|h| Template::compile(h)).collect(),
            note: raw.note.as_deref().map(Template::compile),
        };
        // A later entry for the same code replaces the earlier one, so a project
        // catalog can refine a built-in message.
        if let Some(&idx) = self.by_code.get(&code) {
            self.messages[idx] = rule;
        } else {
            self.by_code.insert(code.clone(), self.messages.len());
            self.messages.push(rule);
        }
    }

    pub fn lint(&self, code: &str) -> Option<&LintEntry> {
        self.lints.get(&code.to_ascii_uppercase())
    }

    /// Look a compiler message up by its text.
    ///
    /// This is the fallback path, used only for compiler output produced without
    /// `-vq`. Templates are tried most-specific-first so a general one cannot
    /// shadow a specific one.
    pub fn match_message(&self, text: &str) -> Option<MessageMatch> {
        let (rule, caps) = self.match_order.iter().find_map(|&i| {
            let rule = &self.messages[i];
            rule.template.match_text(text).map(|caps| (rule, caps))
        })?;
        Some(render_rule(rule, &caps))
    }

    /// Describe a message by its code, capturing arguments from `text`.
    ///
    /// This is the id-first path and the one that normally runs: the profile
    /// requests `-vq`, so every diagnostic carries the compiler's own message
    /// number and the entry is found by code (`F5000`). Template matching is
    /// then used only to recover the arguments for the headline.
    ///
    /// `None` means there is no entry for the code, or its template does not
    /// match this text — callers fall back to the raw message.
    pub fn describe(&self, code: &str, text: &str) -> Option<MessageMatch> {
        let rule = self.message_by_code(code)?;
        let caps = rule.template.match_text(text)?;
        Some(render_rule(rule, &caps))
    }

    /// The rule registered for a code, for `fpcheck explain F1234`.
    pub fn message_by_code(&self, code: &str) -> Option<&MessageRule> {
        self.by_code.get(code).map(|&i| &self.messages[i])
    }

    /// Stable code for a compiler message with no usable numeric id.
    pub fn code_for_unmatched(text: &str) -> String {
        synthetic_code(&canonicalize(text))
    }

    pub fn lint_codes(&self) -> impl Iterator<Item = &String> {
        self.lints.keys()
    }

    /// Add a lint entry programmatically (used by tests and by `explain` output).
    pub fn insert_lint(&mut self, code: &str, entry: LintEntry) {
        self.lints.insert(code.to_ascii_uppercase(), entry);
    }

    /// Fill in `notes`/`help` for an analyzer diagnostic from the catalog,
    /// without overwriting what the lint already said.
    pub fn enrich(&self, diag: &mut Diagnostic) {
        let Some(entry) = self.lint(&diag.code) else {
            return;
        };
        if diag.help.is_empty() {
            diag.help.extend(entry.help.iter().cloned());
        }
        if let Some(note) = &entry.note {
            if !diag.notes.iter().any(|n| n == note) {
                diag.notes.push(note.clone());
            }
        }
    }
}

/// Collapse doubled braces in lint documentation text (`{{` becomes `{`), while
/// leaving single-braced placeholders such as `{callee}` visible for `explain`.
pub fn unescape_braces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                out.push('{');
                if chars.peek() == Some(&'{') {
                    chars.next();
                }
            }
            '}' => {
                out.push('}');
                if chars.peek() == Some(&'}') {
                    chars.next();
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn normalize_lint_entry(mut entry: LintEntry) -> LintEntry {
    entry.headline = unescape_braces(&entry.headline);
    entry.label = entry.label.map(|s| unescape_braces(&s));
    entry.help = entry.help.iter().map(|s| unescape_braces(s)).collect();
    entry.note = entry.note.map(|s| unescape_braces(&s));
    entry
}

/// FNV-1a over the canonical text, mapped into `F1000`..`F9999`.
///
/// The spec (section 4.1) tells implementers to verify real FPC message numbers
/// against an installed compiler and make the catalog overridable. This project
/// was built without an installed FPC, so ids could not be verified; when the
/// compiler does print an id, the normalizer uses `F<id>` and only falls back to
/// this derivation. Codes stay stable because the input is canonicalized first.
pub fn synthetic_code(canonical: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for b in canonical.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("F{:04}", 1000 + (hash % 9000))
}

/// Replace the variable parts of a message so different instances of the same
/// error produce the same code: quoted text becomes `"{name}"`, digit runs
/// become `{n}`.
pub fn canonicalize(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            // Consume up to the closing quote (FPC does not nest them).
            let mut body = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                body.push(c2);
            }
            if !body.is_empty() && body.chars().all(|c| c.is_ascii_digit()) {
                out.push_str("{n}");
            } else {
                out.push_str("\"{name}\"");
            }
        } else if c.is_ascii_digit() {
            while matches!(chars.peek(), Some(d) if d.is_ascii_digit()) {
                chars.next();
            }
            out.push_str("{n}");
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_and_has_the_seed_entries() {
        let c = Catalog::builtin();
        assert!(c.lint("P0702").is_some(), "seed P0702 must load");
        assert!(c.lint("P0304").is_some());
        assert_eq!(c.lint("P0702").unwrap().name, "with-statement");
        // Lookup is case-insensitive.
        assert!(c.lint("p0702").is_some());
    }

    #[test]
    fn literal_braces_render_as_one_brace() {
        let c = Catalog::builtin();
        let entry = c.lint("P0304").unwrap();
        assert!(
            entry.help.iter().any(|h| h.contains("{@unsafe")),
            "expected a literal annotation, got {:?}",
            entry.help
        );
        assert!(
            !entry.help.iter().any(|h| h.contains("{{")),
            "{:?}",
            entry.help
        );
    }

    #[test]
    fn template_captures_and_renders() {
        let t = Template::compile("Identifier not found \"{name}\"");
        let caps = t.match_text("Identifier not found \"Foo\"").unwrap();
        assert_eq!(caps.get("name").map(|s| s.as_str()), Some("Foo"));
        assert!(t.match_text("something else").is_none());
    }

    #[test]
    fn template_requires_suffix_literal_to_end_the_message() {
        let t = Template::compile("expected but \"{found}\" found");
        assert!(t
            .match_text("Syntax error, \"x\" expected but \"y\" found")
            .is_some());
        assert!(t.match_text("expected but \"y\" found later").is_none());
    }

    #[test]
    fn template_placeholder_at_end_captures_the_rest() {
        let t = Template::compile("Can't find unit {name}");
        let caps = t.match_text("Can't find unit SysUtils").unwrap();
        assert_eq!(caps.get("name").map(|s| s.as_str()), Some("SysUtils"));
    }

    #[test]
    fn doubling_escapes_braces_in_templates() {
        let t = Template::compile("use {{@allow(P0101)}} here");
        assert_eq!(t.render_literal(), "use {@allow(P0101)} here");
    }

    #[test]
    fn catalog_matches_a_compiler_message() {
        let c = Catalog::builtin();
        let m = c.match_message("Identifier not found \"Widget\"").unwrap();
        assert_eq!(m.headline, "undeclared identifier `Widget`");
        assert!(!m.help.is_empty());
    }

    #[test]
    fn unmatched_message_is_none() {
        let c = Catalog::builtin();
        assert!(c.match_message("some message we do not know").is_none());
    }

    #[test]
    fn canonicalize_stabilises_instances() {
        assert_eq!(
            canonicalize("Identifier not found \"Foo\""),
            canonicalize("Identifier not found \"Bar\"")
        );
        assert_eq!(canonicalize("at line 42"), "at line {n}");
    }

    #[test]
    fn synthetic_code_is_stable_across_instances() {
        let a = Catalog::code_for_unmatched("Identifier not found \"Foo\"");
        let b = Catalog::code_for_unmatched("Identifier not found \"Bar\"");
        assert_eq!(a, b);
        assert!(a.starts_with('F') && a.len() == 5, "{a}");
    }

    #[test]
    fn matching_message_uses_the_compilers_real_message_number() {
        // Entries carry the compiler's own number, so the code is F10022 rather
        // than something derived from the template text.
        let c = Catalog::builtin();
        let a = c
            .match_message("Can't find unit SysUtils used by prog")
            .unwrap();
        let b = c
            .match_message("Can't find unit Other used by app")
            .unwrap();
        assert_eq!(a.code, "F10022");
        assert_eq!(a.code, b.code);
    }

    #[test]
    fn catalogue_carries_the_compilers_real_message_numbers() {
        // Regression guard on the generated catalogue. These numbers come from
        // FPC 3.2.2's msg/errore.msg and were confirmed against real compiler
        // output (see tests/real_fpc.rs).
        let c = Catalog::builtin();
        for (text, code) in [
            ("Identifier not found \"x\"", "F5000"),
            ("Illegal qualifier", "F3205"),
            ("Illegal expression", "F3203"),
            ("Unexpected end of file", "F2000"),
            ("Duplicate identifier \"x\"", "F5002"),
            ("Function result does not seem to be set", "F5033"),
            ("Variable \"x\" does not seem to be initialized", "F5037"),
            (
                "Wrong number of parameters specified for call to \"q\"",
                "F3026",
            ),
        ] {
            assert_eq!(
                c.match_message(text).map(|m| m.code),
                Some(code.to_string()),
                "wrong code for {text}"
            );
        }
    }

    #[test]
    fn describe_renders_the_entry_for_a_code() {
        let c = Catalog::builtin();
        let m = c
            .describe("F5000", "Identifier not found \"Widget\"")
            .unwrap();
        assert_eq!(m.headline, "undeclared identifier `Widget`");
        assert_eq!(m.label.as_deref(), Some("not found in this scope"));
        assert!(!m.help.is_empty());
    }

    #[test]
    fn describe_is_none_when_the_template_does_not_match() {
        // A code we know, paired with text from a different message: the caller
        // must fall back to the raw text rather than print a wrong headline.
        let c = Catalog::builtin();
        assert!(c.describe("F5000", "Illegal qualifier").is_none());
    }

    #[test]
    fn describe_is_none_for_an_unknown_code() {
        let c = Catalog::builtin();
        assert!(c.describe("F999999", "anything").is_none());
    }

    #[test]
    fn the_catalogue_is_the_full_compiler_set_with_real_ids() {
        let c = Catalog::builtin();
        assert!(
            c.messages.len() > 900,
            "expected the full compiler catalogue, got {}",
            c.messages.len()
        );
        for rule in &c.messages {
            // `F` plus the compiler's number as printed, without leading zeros:
            // 5000 becomes F5000, and 10022 becomes F10022.
            let digits = rule
                .code
                .strip_prefix('F')
                .unwrap_or_else(|| panic!("not an F code: {}", rule.code));
            assert!(
                !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit()),
                "{}",
                rule.code
            );
            assert!((4..=5).contains(&digits.len()), "{}", rule.code);
            assert!(!rule.template.source().is_empty(), "{}", rule.code);
        }
    }

    #[test]
    fn template_matching_is_ordered_most_specific_first() {
        let c = Catalog::builtin();
        let len = |i: usize| c.messages[i].template.literal_len();
        let order = &c.match_order;
        assert_eq!(order.len(), c.messages.len());
        assert!(len(order[0]) >= len(order[order.len() - 1]));
    }

    #[test]
    fn display_unescapes_braces_and_keeps_placeholder_names() {
        let t = Template::compile("use {{@allow}} with {p1} here");
        assert_eq!(t.display(), "use {@allow} with {p1} here");
    }

    #[test]
    fn no_real_template_displays_a_doubled_brace() {
        // Guards the generator's brace escaping across the whole catalogue,
        // including messages that quote compiler directives like {$M+}.
        let c = Catalog::builtin();
        for rule in &c.messages {
            let template = rule.template.display();
            assert!(!template.contains("{{"), "{}: {}", rule.code, template);
            let headline = rule.headline.display();
            assert!(!headline.contains("{{"), "{}: {}", rule.code, headline);
        }
    }

    #[test]
    fn message_by_code_finds_the_rule() {
        let c = Catalog::builtin();
        let code = c.match_message("Identifier not found \"X\"").unwrap().code;
        assert!(c.message_by_code(&code).is_some());
    }

    #[test]
    fn load_dir_overrides_a_builtin_lint() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(LINT_FILE),
            "[P0702]\nname = \"with-statement\"\nheadline = \"overridden\"\n",
        )
        .unwrap();
        let c = Catalog::load_dir(dir.path()).unwrap();
        assert_eq!(c.lint("P0702").unwrap().headline, "overridden");
        // Built-ins that were not overridden survive.
        assert!(c.lint("P0304").is_some());
    }

    #[test]
    fn load_dir_extends_compiler_messages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FPC_FILE),
            "[[message]]\ntemplate = \"Boom {x}\"\nheadline = \"exploded at {x}\"\n",
        )
        .unwrap();
        let c = Catalog::load_dir(dir.path()).unwrap();
        assert_eq!(
            c.match_message("Boom now").unwrap().headline,
            "exploded at now"
        );
        // Still has the built-ins.
        assert!(c.match_message("Identifier not found \"z\"").is_some());
    }

    #[test]
    fn load_dir_reports_bad_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LINT_FILE), "=== not toml").unwrap();
        assert!(matches!(
            Catalog::load_dir(dir.path()),
            Err(Error::Catalog(_))
        ));
    }

    #[test]
    fn enrich_adds_help_and_note_without_clobbering() {
        let c = Catalog::builtin();
        assert!(
            c.lint("P0001").and_then(|e| e.note.as_ref()).is_some(),
            "seed entry P0001 must carry a note"
        );
        let mut d = Diagnostic::lint(
            "P0001",
            crate::diagnostic::Level::Error,
            "allow without reason",
            crate::diagnostic::Span::point("a.pas", 1, 1),
        );
        d.help.push("lint-specific help".into());
        c.enrich(&mut d);
        assert_eq!(d.help[0], "lint-specific help");
        assert!(!d.notes.is_empty());
    }
}
