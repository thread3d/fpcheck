//! A lossless Pascal lexer.
//!
//! Two jobs, both required by the spec:
//!
//! 1. **Point → span expansion** (section 4.2 step 2): FPC reports a point, so
//!    the normalizer asks "what token starts here?" and widens the span.
//! 2. **Feeding lexical and syntactic lints** (section 6.2, the `P07xx`/`P08xx`
//!    families and friends).
//!
//! The lexer keeps *everything*: comments and compiler directives are real
//! tokens (marked as trivia) with their own spans, because annotations such as
//! `{@owned}` live in comments and `{$R-}` is itself a diagnostic (P0304).
//!
//! Pascal is case-insensitive, so comparisons should go through
//! [`Token::eq_ignore_case`] rather than `==` on `text`.

use crate::diagnostic::Span;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Ident,
    Keyword,
    Number,
    Str,
    Char,
    Symbol,
    /// `{ ... }`, `(* ... *)`, `// ...`
    Comment,
    /// `{$ ... }`, `(*$ ... *)`
    Directive,
    Eof,
}

impl TokKind {
    pub fn is_trivia(self) -> bool {
        matches!(self, TokKind::Comment | TokKind::Directive)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TokKind::Ident => "identifier",
            TokKind::Keyword => "keyword",
            TokKind::Number => "number",
            TokKind::Str => "string",
            TokKind::Char => "char",
            TokKind::Symbol => "symbol",
            TokKind::Comment => "comment",
            TokKind::Directive => "directive",
            TokKind::Eof => "end of file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokKind,
    /// Exact source text of the token.
    pub text: String,
    pub span: Span,
    /// Convenience copies of the span start.
    pub line: u32,
    pub col: u32,
}

impl Token {
    pub fn is_trivia(&self) -> bool {
        self.kind.is_trivia()
    }

    /// Case-insensitive comparison, as Pascal requires.
    pub fn eq_ignore_case(&self, s: &str) -> bool {
        self.text.eq_ignore_ascii_case(s)
    }

    pub fn is_keyword(&self, s: &str) -> bool {
        self.kind == TokKind::Keyword && self.eq_ignore_case(s)
    }

    pub fn is_word(&self, s: &str) -> bool {
        matches!(self.kind, TokKind::Ident | TokKind::Keyword) && self.eq_ignore_case(s)
    }

    /// For a `Directive` token, split `{$NAME arg}` into `("NAME", "arg")`.
    pub fn directive_parts(&self) -> Option<(String, String)> {
        if self.kind != TokKind::Directive {
            return None;
        }
        let mut body = self.text.as_str();
        if let Some(rest) = body.strip_prefix("{") {
            body = rest;
        } else if let Some(rest) = body.strip_prefix("(*") {
            body = rest;
        }
        let body = body.strip_prefix('$').unwrap_or(body);
        let body = body
            .strip_suffix('}')
            .or_else(|| body.strip_suffix("*)"))
            .unwrap_or(body);
        // The name is the leading alphanumeric run; everything after it is the
        // argument, so `{$R-}` is `("R", "-")` rather than `("R-", "")`.
        let body = body.trim();
        let name_end = body
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(body.len());
        let name = body[..name_end].to_string();
        let arg = body[name_end..]
            .trim()
            .trim_start_matches('=')
            .trim()
            .to_string();
        Some((name.to_ascii_uppercase(), arg))
    }

    /// A `Comment` token's inner text with the delimiters removed.
    pub fn comment_body(&self) -> Option<&str> {
        if self.kind != TokKind::Comment {
            return None;
        }
        let s = self.text.as_str();
        if let Some(rest) = s.strip_prefix("//") {
            return Some(rest);
        }
        if let Some(rest) = s.strip_prefix('{') {
            return rest.strip_suffix('}').or(Some(rest));
        }
        if let Some(rest) = s.strip_prefix("(*") {
            return rest.strip_suffix("*)").or(Some(rest));
        }
        Some(s)
    }
}

/// Reserved words. This is the FPC `objfpc` set plus the directive-ish words
/// that can appear as identifiers in some modes; being generous here is safe
/// because lints match on text, not on this classification alone.
pub const KEYWORDS: &[&str] = &[
    "absolute",
    "abstract",
    "and",
    "array",
    "as",
    "asm",
    "assembler",
    "automated",
    "begin",
    "case",
    "cdecl",
    "class",
    "const",
    "constructor",
    "destructor",
    "dispinterface",
    "div",
    "do",
    "downto",
    "dynamic",
    "else",
    "end",
    "except",
    "export",
    "exports",
    "external",
    "far",
    "file",
    "finalization",
    "finally",
    "for",
    "forward",
    "function",
    "generic",
    "goto",
    "helper",
    "if",
    "implementation",
    "in",
    "inherited",
    "initialization",
    "inline",
    "interface",
    "interrupt",
    "is",
    "label",
    "library",
    "local",
    "message",
    "mod",
    "near",
    "nil",
    "not",
    "object",
    "of",
    "on",
    "operator",
    "or",
    "out",
    "overload",
    "override",
    "packed",
    "pascal",
    "platform",
    "private",
    "procedure",
    "program",
    "property",
    "protected",
    "public",
    "published",
    "raise",
    "record",
    "reference",
    "register",
    "reintroduce",
    "repeat",
    "resourcestring",
    "safecall",
    "sealed",
    "set",
    "shl",
    "shr",
    "specialize",
    "static",
    "stdcall",
    "strict",
    "string",
    "then",
    "threadvar",
    "to",
    "try",
    "type",
    "unit",
    "until",
    "uses",
    "var",
    "varargs",
    "virtual",
    "while",
    "with",
    "xor",
];

fn is_keyword(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    KEYWORDS.iter().any(|k| *k == lower)
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: u32,
    col: u32,
    path: PathBuf,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn peek_byte(&self, n: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn token(&self, kind: TokKind, start_line: u32, start_col: u32, start_pos: usize) -> Token {
        Token {
            kind,
            text: self.src[start_pos..self.pos].to_string(),
            span: Span::new(
                self.path.clone(),
                start_line,
                start_col,
                self.line,
                self.col,
            ),
            line: start_line,
            col: start_col,
        }
    }

    /// Consume a `{...}` or `(*...*)` block, returning whether it is a directive.
    fn block_comment(&mut self, close: &str, directive: bool) -> TokKind {
        loop {
            if self.src[self.pos..].starts_with(close) {
                for _ in 0..close.len() {
                    self.bump();
                }
                break;
            }
            if self.bump().is_none() {
                break; // unterminated: FPC would error; we stop at EOF
            }
        }
        if directive {
            TokKind::Directive
        } else {
            TokKind::Comment
        }
    }

    fn next_token(&mut self) -> Token {
        self.skip_whitespace();
        let start_line = self.line;
        let start_col = self.col;
        let start_pos = self.pos;

        let Some(c) = self.peek() else {
            return self.token(TokKind::Eof, start_line, start_col, start_pos);
        };

        // Comments and directives.
        if c == '{' {
            self.bump();
            let directive = self.peek() == Some('$');
            let kind = self.block_comment("}", directive);
            return self.token(kind, start_line, start_col, start_pos);
        }
        if c == '(' && self.peek_byte(1) == Some(b'*') {
            self.bump();
            self.bump();
            let directive = self.peek() == Some('$');
            let kind = self.block_comment("*)", directive);
            return self.token(kind, start_line, start_col, start_pos);
        }
        if c == '/' && self.peek_byte(1) == Some(b'/') {
            while let Some(ch) = self.peek() {
                if ch == '\n' {
                    break;
                }
                self.bump();
            }
            return self.token(TokKind::Comment, start_line, start_col, start_pos);
        }

        // Strings: '' escapes a quote inside the literal.
        if c == '\'' {
            self.bump();
            loop {
                match self.bump() {
                    Some('\'') => {
                        if self.peek() == Some('\'') {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            return self.token(TokKind::Str, start_line, start_col, start_pos);
        }

        // Character codes: #13, #$0A, #65.
        if c == '#' {
            self.bump();
            if self.peek() == Some('$') {
                self.bump();
                while matches!(self.peek(), Some(ch) if ch.is_ascii_hexdigit()) {
                    self.bump();
                }
            } else {
                while matches!(self.peek(), Some(ch) if ch.is_ascii_digit()) {
                    self.bump();
                }
            }
            return self.token(TokKind::Char, start_line, start_col, start_pos);
        }

        // Numbers with a base prefix.
        if c == '$' && matches!(self.peek_at(1), Some(ch) if ch.is_ascii_hexdigit()) {
            self.bump();
            while matches!(self.peek(), Some(ch) if ch.is_ascii_hexdigit() || ch == '_') {
                self.bump();
            }
            return self.token(TokKind::Number, start_line, start_col, start_pos);
        }
        if c == '%' && matches!(self.peek_at(1), Some('0') | Some('1')) {
            self.bump();
            while matches!(self.peek(), Some('0') | Some('1') | Some('_')) {
                self.bump();
            }
            return self.token(TokKind::Number, start_line, start_col, start_pos);
        }
        if c == '&' && matches!(self.peek_at(1), Some(ch) if ch.is_ascii_digit()) {
            self.bump();
            while matches!(self.peek(), Some(ch) if ('0'..='7').contains(&ch) || ch == '_') {
                self.bump();
            }
            return self.token(TokKind::Number, start_line, start_col, start_pos);
        }

        // Decimal numbers.
        if c.is_ascii_digit() {
            while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
                self.bump();
            }
            // A fractional part, but `1..5` must stay `1` `..` `5`.
            if self.peek() == Some('.')
                && self.peek_at(1) != Some('.')
                && matches!(self.peek_at(1), Some(ch) if ch.is_ascii_digit())
            {
                self.bump();
                while matches!(self.peek(), Some(ch) if ch.is_ascii_digit() || ch == '_') {
                    self.bump();
                }
            }
            if matches!(self.peek(), Some('e') | Some('E')) {
                let save = (self.pos, self.line, self.col);
                self.bump();
                if matches!(self.peek(), Some('+') | Some('-')) {
                    self.bump();
                }
                if matches!(self.peek(), Some(ch) if ch.is_ascii_digit()) {
                    while matches!(self.peek(), Some(ch) if ch.is_ascii_digit()) {
                        self.bump();
                    }
                } else {
                    // Not an exponent after all (e.g. `1e` is nonsense): rewind.
                    self.pos = save.0;
                    self.line = save.1;
                    self.col = save.2;
                }
            }
            return self.token(TokKind::Number, start_line, start_col, start_pos);
        }

        // `&` escapes a keyword so it can be used as an identifier.
        if c == '&' && matches!(self.peek_at(1), Some(ch) if ch.is_alphabetic() || ch == '_') {
            self.bump();
            while matches!(self.peek(), Some(ch) if ch.is_alphanumeric() || ch == '_') {
                self.bump();
            }
            return self.token(TokKind::Ident, start_line, start_col, start_pos);
        }

        // Identifiers and keywords.
        if c.is_alphabetic() || c == '_' {
            while matches!(self.peek(), Some(ch) if ch.is_alphanumeric() || ch == '_') {
                self.bump();
            }
            let text = &self.src[start_pos..self.pos];
            let kind = if is_keyword(text) {
                TokKind::Keyword
            } else {
                TokKind::Ident
            };
            return self.token(kind, start_line, start_col, start_pos);
        }

        // Symbols, longest match first.
        const TWO: &[&str] = &[
            ":=", "<=", ">=", "<>", "..", "**", "><", "+=", "-=", "*=", "/=",
        ];
        if let Some(two) = TWO.iter().find(|t| self.src[self.pos..].starts_with(**t)) {
            for _ in 0..two.len() {
                self.bump();
            }
            return self.token(TokKind::Symbol, start_line, start_col, start_pos);
        }
        self.bump();
        self.token(TokKind::Symbol, start_line, start_col, start_pos)
    }
}

/// Tokenize a whole file. The result always ends with a single `Eof` token.
pub fn tokenize(path: &Path, src: &str) -> Vec<Token> {
    let mut lx = Lexer {
        src,
        pos: 0,
        line: 1,
        col: 1,
        path: path.to_path_buf(),
    };
    let mut out = Vec::new();
    loop {
        let t = lx.next_token();
        let eof = t.kind == TokKind::Eof;
        out.push(t);
        if eof {
            break;
        }
    }
    out
}

/// The first non-trivia token starting at or covering `(line, col)`.
///
/// FPC's column points at the start of the offending construct, so this is the
/// widening step from a zero-width point to a real span.
pub fn token_at(tokens: &[Token], line: u32, col: u32) -> Option<&Token> {
    // Span ends are exclusive, so a point at the end of one token belongs to
    // the next one: FPC's column is always a token *start*.
    fn covers(t: &Token, line: u32, col: u32) -> bool {
        (t.span.line_start, t.span.col_start) <= (line, col)
            && (line, col) < (t.span.line_end, t.span.col_end)
    }

    if let Some(i) = tokens
        .iter()
        .position(|t| !t.is_trivia() && t.kind != TokKind::Eof && covers(t, line, col))
    {
        return Some(&tokens[i]);
    }
    // The point landed inside a comment or directive: never highlight a
    // comment, report the construct that follows it instead.
    let i = tokens
        .iter()
        .position(|t| t.is_trivia() && covers(t, line, col))?;
    tokens[i + 1..]
        .iter()
        .find(|t| !t.is_trivia() && t.kind != TokKind::Eof)
}

/// The next non-trivia token at or after a point, used when FPC lands on
/// whitespace or on a token it did not lex the same way we do.
pub fn token_at_or_after(tokens: &[Token], line: u32, col: u32) -> Option<&Token> {
    token_at(tokens, line, col).or_else(|| {
        tokens
            .iter()
            .find(|t| !t.is_trivia() && t.kind != TokKind::Eof && (t.line, t.col) >= (line, col))
    })
}

/// Non-trivia tokens, i.e. the stream lints usually walk.
pub fn significant(tokens: &[Token]) -> impl Iterator<Item = &Token> {
    tokens
        .iter()
        .filter(|t| !t.is_trivia() && t.kind != TokKind::Eof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn lex(src: &str) -> Vec<Token> {
        tokenize(Path::new("t.pas"), src)
    }

    fn kinds(src: &str) -> Vec<TokKind> {
        lex(src).into_iter().map(|t| t.kind).collect()
    }

    fn texts(src: &str) -> Vec<String> {
        lex(src)
            .into_iter()
            .filter(|t| t.kind != TokKind::Eof)
            .map(|t| t.text)
            .collect()
    }

    #[test]
    fn always_ends_with_eof() {
        let t = lex("begin end.");
        assert_eq!(t.last().unwrap().kind, TokKind::Eof);
        assert_eq!(t.iter().filter(|t| t.kind == TokKind::Eof).count(), 1);
    }

    #[test]
    fn identifiers_and_keywords_are_distinguished_case_insensitively() {
        let t = lex("BEGIN Foo begin");
        assert_eq!(t[0].kind, TokKind::Keyword);
        assert!(t[0].is_keyword("begin"));
        assert_eq!(t[1].kind, TokKind::Ident);
        assert!(t[1].eq_ignore_case("foo"));
        assert_eq!(t[2].kind, TokKind::Keyword);
    }

    #[test]
    fn spans_are_one_based_and_cover_the_token() {
        let t = lex("  abc");
        assert_eq!(t[0].span.line_start, 1);
        assert_eq!(t[0].span.col_start, 3);
        assert_eq!(t[0].span.col_end, 6);
        assert_eq!(t[0].text, "abc");
    }

    #[test]
    fn spans_track_lines() {
        let t = lex("a\n  b");
        assert_eq!((t[1].line, t[1].col), (2, 3));
        assert_eq!((t[1].span.line_start, t[1].span.line_end), (2, 2));
    }

    #[test]
    fn brace_and_paren_comments_are_trivia() {
        let t = lex("{ hi } (* there *) // eol\nx");
        assert_eq!(t[0].kind, TokKind::Comment);
        assert_eq!(t[0].comment_body(), Some(" hi "));
        assert_eq!(t[1].kind, TokKind::Comment);
        assert_eq!(t[1].comment_body(), Some(" there "));
        assert_eq!(t[2].kind, TokKind::Comment);
        assert_eq!(t[2].comment_body(), Some(" eol"));
        assert_eq!(t[3].text, "x");
    }

    #[test]
    fn directives_are_separate_from_comments() {
        let t = lex("{$R-} (*$Q-*) {not a directive}");
        assert_eq!(t[0].kind, TokKind::Directive);
        assert_eq!(t[1].kind, TokKind::Directive);
        assert_eq!(t[2].kind, TokKind::Comment);
    }

    #[test]
    fn directive_parts_split_name_and_argument() {
        let t = lex("{$mode objfpc}{$R-}{$IFDEF FOO}");
        assert_eq!(
            t[0].directive_parts(),
            Some(("MODE".into(), "objfpc".into()))
        );
        assert_eq!(t[1].directive_parts(), Some(("R".into(), "-".into())));
        assert_eq!(t[2].directive_parts(), Some(("IFDEF".into(), "FOO".into())));
    }

    #[test]
    fn strings_handle_doubled_quotes() {
        let t = lex("'it''s'");
        assert_eq!(t[0].kind, TokKind::Str);
        assert_eq!(t[0].text, "'it''s'");
    }

    #[test]
    fn unterminated_string_stops_at_eof_not_hang() {
        let t = lex("'oops");
        assert_eq!(t[0].kind, TokKind::Str);
        assert_eq!(t[1].kind, TokKind::Eof);
    }

    #[test]
    fn range_after_integer_is_not_a_float() {
        let t = lex("1..5");
        assert_eq!(
            t.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["1", "..", "5", ""]
        );
    }

    #[test]
    fn numbers_of_every_base() {
        assert_eq!(
            texts("$FF &17 %1010 1.5e3 42"),
            vec!["$FF", "&17", "%1010", "1.5e3", "42"]
        );
        assert!(lex("$FF")
            .iter()
            .all(|t| t.kind != TokKind::Number || t.text == "$FF"));
    }

    #[test]
    fn percent_not_followed_by_binary_is_a_symbol() {
        let t = lex("a % b");
        assert_eq!(t[1].kind, TokKind::Symbol);
    }

    #[test]
    fn ampersand_escapes_keywords() {
        let t = lex("&begin");
        assert_eq!(t[0].kind, TokKind::Ident);
        assert_eq!(t[0].text, "&begin");
    }

    #[test]
    fn char_codes_lex_as_char() {
        let t = lex("#13#$0A#65");
        assert_eq!(t[0].kind, TokKind::Char);
        assert_eq!(t[1].kind, TokKind::Char);
        assert_eq!(t[2].kind, TokKind::Char);
    }

    #[test]
    fn two_char_symbols_win_over_one_char() {
        let t = lex(":= <= >= <> .. **");
        assert!(t.iter().take(6).all(|t| t.kind == TokKind::Symbol));
        assert_eq!(t[0].text, ":=");
        assert_eq!(t[5].text, "**");
    }

    #[test]
    fn token_at_finds_token_covering_point() {
        let t = lex("begin\n  Foo := 1;\nend.");
        let tok = token_at(&t, 2, 3).unwrap();
        assert_eq!(tok.text, "Foo");
    }

    #[test]
    fn token_at_skips_trivia() {
        let t = lex("{ c } Foo");
        assert_eq!(token_at(&t, 1, 3).map(|t| t.text.as_str()), Some("Foo"));
    }

    #[test]
    fn token_at_or_after_recovers_from_point_in_whitespace() {
        let t = lex("   Foo");
        let tok = token_at_or_after(&t, 1, 1).unwrap();
        assert_eq!(tok.text, "Foo");
    }

    #[test]
    fn token_at_returns_none_past_eof() {
        let t = lex("x");
        assert!(token_at(&t, 99, 1).is_none());
    }

    #[test]
    fn significant_skips_comments_and_directives() {
        let t = lex("{a} x {$R-} y");
        let sig: Vec<&str> = significant(&t).map(|t| t.text.as_str()).collect();
        assert_eq!(sig, vec!["x", "y"]);
    }

    #[test]
    fn utf8_columns_count_characters() {
        let t = lex("'ä'x");
        assert_eq!(t[1].text, "x");
        assert_eq!(t[1].col, 4, "quote, a-umlaut, quote => x starts at col 4");
    }

    #[test]
    fn kinds_are_reported_for_a_small_program() {
        let k = kinds("unit u;\ninterface\nimplementation\nend.");
        assert!(k.contains(&TokKind::Keyword));
        assert!(k.contains(&TokKind::Ident));
    }
}
