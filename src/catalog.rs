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
        Template { parts, source: source.to_string() }
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
        };
        for raw in fpc.message {
            catalog.push_message(raw);
        }
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
        }

        Ok(catalog)
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
    pub fn match_message(&self, text: &str) -> Option<MessageMatch> {
        let (rule, caps) = self
            .messages
            .iter()
            .find_map(|r| r.template.match_text(text).map(|caps| (r, caps)))?;
        Some(MessageMatch {
            code: rule.code.clone(),
            headline: rule.headline.render(&caps),
            label: rule.label.as_ref().map(|l| l.render(&caps)),
            help: rule.help.iter().map(|h| h.render(&caps)).collect(),
            note: rule.note.as_ref().map(|n| n.render(&caps)),
        })
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
        assert!(!entry.help.iter().any(|h| h.contains("{{")), "{:?}", entry.help);
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
        assert!(t.match_text("Syntax error, \"x\" expected but \"y\" found").is_some());
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
    fn matching_message_uses_the_template_derived_code() {
        // A matching message takes the entry's code, and that code is identical
        // for every instance of the same message.
        let c = Catalog::builtin();
        let a = c.match_message("Can't find unit SysUtils used by prog").unwrap();
        let b = c.match_message("Can't find unit Other used by app").unwrap();
        assert_eq!(a.code, b.code);
        assert_eq!(a.code, synthetic_code("Can't find unit {name} used by {user}"));
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
        assert_eq!(c.match_message("Boom now").unwrap().headline, "exploded at now");
        // Still has the built-ins.
        assert!(c.match_message("Identifier not found \"z\"").is_some());
    }

    #[test]
    fn load_dir_reports_bad_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LINT_FILE), "=== not toml").unwrap();
        assert!(matches!(Catalog::load_dir(dir.path()), Err(Error::Catalog(_))));
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
