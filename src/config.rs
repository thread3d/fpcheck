//! `fpcheck.toml` loading and resolution (spec section 10).
//!
//! The `[lints]` table is deliberately parsed by hand from a raw `toml::Table`:
//! it mixes scalar entries (`P0301 = "deny"`) with a nested table
//! (`[lints.per-unit]`), which serde's derive handles poorly. Everything else
//! uses derived deserialization with `#[serde(default)]` so a partial file is
//! valid and forward-compatible.

use crate::diagnostic::Level;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE_NAME: &str = "fpcheck.toml";

/// What `[lints] CODE = "..."` may say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LevelSetting {
    Deny,
    Warn,
    Allow,
}

impl LevelSetting {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "deny" => Ok(LevelSetting::Deny),
            "warn" => Ok(LevelSetting::Warn),
            "allow" => Ok(LevelSetting::Allow),
            other => Err(Error::config(format!(
                "invalid lint level `{other}` (expected deny, warn, or allow)"
            ))),
        }
    }

    /// `Allow` maps to "do not emit", which has no `Level` of its own.
    pub fn to_level(self) -> Option<Level> {
        match self {
            LevelSetting::Deny => Some(Level::Error),
            LevelSetting::Warn => Some(Level::Warning),
            LevelSetting::Allow => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LevelSetting::Deny => "deny",
            LevelSetting::Warn => "warn",
            LevelSetting::Allow => "allow",
        }
    }
}

/// Per-unit strictness (spec section 7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Strictness {
    Strict,
    /// The default for unannotated units.
    #[default]
    Lenient,
}

impl Strictness {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "strict" => Ok(Strictness::Strict),
            "lenient" => Ok(Strictness::Lenient),
            other => Err(Error::config(format!(
                "invalid strictness `{other}` (expected strict or lenient)"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackageConfig {
    pub name: String,
    pub sources: Vec<String>,
    pub tests: Vec<String>,
    pub main: Option<String>,
}

impl Default for PackageConfig {
    fn default() -> Self {
        PackageConfig {
            name: String::new(),
            sources: vec!["src".to_string()],
            tests: vec!["tests".to_string()],
            main: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FpcConfig {
    pub binary: String,
    pub mode: String,
    pub units: Vec<String>,
    pub defines: Vec<String>,
    /// Appended verbatim to the profile flags.
    pub extra: Vec<String>,
}

impl Default for FpcConfig {
    fn default() -> Self {
        FpcConfig {
            binary: "fpc".to_string(),
            mode: "objfpc".to_string(),
            units: Vec::new(),
            defines: Vec::new(),
            extra: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileConfig {
    /// Subset of `range`, `overflow`, `io`, `stack`, `method`.
    pub checks: Vec<String>,
    pub heaptrc: bool,
    /// `dwarf`, `stabs`, `line`, or empty for none.
    pub debug: String,
    pub optimize: u8,
    pub assertions: bool,
}

impl ProfileConfig {
    pub fn dev() -> Self {
        ProfileConfig {
            checks: vec![
                "range".into(),
                "overflow".into(),
                "io".into(),
                "stack".into(),
                "method".into(),
            ],
            heaptrc: true,
            debug: "dwarf".into(),
            optimize: 0,
            assertions: true,
        }
    }

    pub fn release() -> Self {
        ProfileConfig {
            checks: vec!["range".into(), "overflow".into()],
            heaptrc: false,
            debug: String::new(),
            optimize: 2,
            assertions: false,
        }
    }
}

impl Default for ProfileConfig {
    fn default() -> Self {
        ProfileConfig::dev()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profiles {
    pub dev: ProfileConfig,
    pub release: ProfileConfig,
}

impl Default for Profiles {
    fn default() -> Self {
        Profiles { dev: ProfileConfig::dev(), release: ProfileConfig::release() }
    }
}

impl Profiles {
    pub fn get(&self, name: ProfileName) -> &ProfileConfig {
        match name {
            ProfileName::Dev => &self.dev,
            ProfileName::Release => &self.release,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileName {
    Dev,
    Release,
}

impl ProfileName {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "dev" => Ok(ProfileName::Dev),
            "release" => Ok(ProfileName::Release),
            other => Err(Error::config(format!(
                "unknown profile `{other}` (expected dev or release)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ProfileName::Dev => "dev",
            ProfileName::Release => "release",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub max_diagnostics: usize,
    pub context_lines: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        LlmConfig { max_diagnostics: 15, context_lines: 2 }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub package: PackageConfig,
    pub fpc: FpcConfig,
    pub profiles: Profiles,
    /// `CODE -> level`, keys upper-cased. Overrides the built-in default level.
    pub lint_levels: BTreeMap<String, LevelSetting>,
    /// Glob -> strictness, in file order (later entries win).
    pub per_unit: Vec<(String, Strictness)>,
    pub llm: LlmConfig,
    /// Directory containing the config file, when loaded from disk.
    pub root: Option<PathBuf>,
}

/// Deserialization shape for everything except `[lints]`.
#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawConfig {
    package: PackageConfig,
    fpc: FpcConfig,
    profile: Profiles,
    llm: LlmConfig,
    lints: toml::Table,
}

impl Default for RawConfig {
    fn default() -> Self {
        RawConfig {
            package: PackageConfig::default(),
            fpc: FpcConfig::default(),
            profile: Profiles::default(),
            llm: LlmConfig::default(),
            lints: toml::Table::new(),
        }
    }
}

impl Config {
    /// Built-in defaults, used when no config file exists.
    pub fn defaults() -> Self {
        Config {
            package: PackageConfig::default(),
            fpc: FpcConfig::default(),
            profiles: Profiles::default(),
            lint_levels: BTreeMap::new(),
            per_unit: Vec::new(),
            llm: LlmConfig::default(),
            root: None,
        }
    }

    pub fn from_str(text: &str) -> Result<Self> {
        let raw: RawConfig = toml::from_str(text)
            .map_err(|e| Error::config(format!("could not parse {CONFIG_FILE_NAME}: {e}")))?;

        let mut lint_levels = BTreeMap::new();
        let mut per_unit = Vec::new();

        for (key, value) in &raw.lints {
            if key.eq_ignore_ascii_case("per-unit") {
                let table = value.as_table().ok_or_else(|| {
                    Error::config("[lints.per-unit] must be a table of glob = \"strict|lenient\"")
                })?;
                for (glob, v) in table {
                    let s = v.as_str().ok_or_else(|| {
                        Error::config(format!("[lints.per-unit] `{glob}` must be a string"))
                    })?;
                    per_unit.push((glob.clone(), Strictness::parse(s)?));
                }
            } else {
                let s = value.as_str().ok_or_else(|| {
                    Error::config(format!("[lints] `{key}` must be \"deny\", \"warn\", or \"allow\""))
                })?;
                lint_levels.insert(key.to_ascii_uppercase(), LevelSetting::parse(s)?);
            }
        }

        Ok(Config {
            package: raw.package,
            fpc: raw.fpc,
            profiles: raw.profile,
            lint_levels,
            per_unit,
            llm: raw.llm,
            root: None,
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::io(path.display().to_string(), e))?;
        let mut cfg = Config::from_str(&text)?;
        cfg.root = path.parent().map(|p| p.to_path_buf());
        Ok(cfg)
    }

    /// Walk up from `start` looking for `fpcheck.toml`; defaults if none found.
    pub fn discover(start: &Path) -> Result<Self> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            let candidate = d.join(CONFIG_FILE_NAME);
            if candidate.is_file() {
                return Config::load(&candidate);
            }
            dir = d.parent();
        }
        Ok(Config::defaults())
    }

    /// The configured level for a code, if any. The analyzer falls back to the
    /// lint's own default when this returns `None`.
    pub fn level_for(&self, code: &str) -> Option<LevelSetting> {
        self.lint_levels.get(&code.to_ascii_uppercase()).copied()
    }

    /// Strictness for a source file, honoring `[lints.per-unit]` globs.
    /// Unannotated units default to `lenient` (spec section 7.3).
    pub fn strictness_for(&self, path: &Path) -> Strictness {
        let as_str = path.to_string_lossy();
        let mut result = Strictness::Lenient;
        for (glob, s) in &self.per_unit {
            if glob_match(glob, &as_str) {
                result = *s;
            }
        }
        result
    }

    /// A starter `fpcheck.toml` for `fpcheck init`.
    pub fn init_template(name: &str) -> String {
        format!(
            r#"# fpcheck configuration. See SPEC.md section 10.
[package]
name    = "{name}"
sources = ["src"]
tests   = ["tests"]
main    = "src/{name}.pas"

[fpc]
binary   = "fpc"
mode     = "objfpc"
units    = []
defines  = []
extra    = []

[profile.dev]
checks   = ["range", "overflow", "io", "stack", "method"]
heaptrc  = true
debug    = "dwarf"
optimize = 0

[profile.release]
checks   = ["range", "overflow"]
optimize = 2

[lints]
# P0301 = "deny"
# P0901 = "warn"

[lints.per-unit]
# "src/legacy/**" = "lenient"
# "src/**"        = "strict"

[llm]
max_diagnostics = 15
context_lines   = 2
"#
        )
    }
}

/// Minimal glob matcher supporting `*`, `?`, and `**`.
///
/// `**` matches across `/`, `*` and `?` do not. Patterns are matched against
/// both the full path and the path with a leading `./` removed.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let path = path.strip_prefix("./").unwrap_or(path);
    glob_match_inner(pattern.as_bytes(), path.as_bytes())
}

fn glob_match_inner(pat: &[u8], text: &[u8]) -> bool {
    // Iterative matcher with backtracking for `*`; `**` is treated as `*` that
    // may cross separators.
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star_p, mut star_t) = (usize::MAX, 0usize);
    let (mut star_cross, mut star_set) = (false, false);

    while t < text.len() {
        if p < pat.len() {
            match pat[p] {
                b'*' => {
                    let cross = p + 1 < pat.len() && pat[p + 1] == b'*';
                    let mut next = p + 1;
                    if cross {
                        next += 1;
                    }
                    star_p = next;
                    star_t = t;
                    star_cross = cross;
                    star_set = true;
                    p = next;
                    continue;
                }
                b'?' if text[t] != b'/' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                c if c == text[t] => {
                    p += 1;
                    t += 1;
                    continue;
                }
                _ => {}
            }
        }
        if star_set && (star_cross || text[star_t] != b'/') {
            star_t += 1;
            t = star_t;
            p = star_p;
            continue;
        }
        return false;
    }

    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::defaults();
        assert_eq!(c.fpc.mode, "objfpc");
        assert_eq!(c.llm.max_diagnostics, 15);
        assert_eq!(c.llm.context_lines, 2);
        assert_eq!(c.profiles.dev.optimize, 0);
        assert!(c.profiles.dev.heaptrc);
        assert_eq!(c.profiles.release.optimize, 2);
        assert!(!c.profiles.release.heaptrc);
    }

    #[test]
    fn full_config_parses() {
        let text = r#"
[package]
name    = "mdiscd"
sources = ["src"]
tests   = ["tests"]
main    = "src/mdiscd.pas"

[fpc]
binary  = "/opt/fpc/bin/fpc"
mode    = "delphi"
units   = ["lib/**"]
defines = ["LINUX"]

[profile.dev]
checks   = ["range", "overflow"]
heaptrc  = false
debug    = "stabs"
optimize = 1

[profile.release]
checks   = ["range"]
optimize = 3

[lints]
P0301 = "deny"
P0901 = "warn"
P0902 = "allow"

[lints.per-unit]
"src/legacy/**" = "lenient"
"src/**"        = "strict"

[llm]
max_diagnostics = 5
context_lines   = 3
"#;
        let c = Config::from_str(text).unwrap();
        assert_eq!(c.package.name, "mdiscd");
        assert_eq!(c.fpc.binary, "/opt/fpc/bin/fpc");
        assert_eq!(c.fpc.mode, "delphi");
        assert_eq!(c.fpc.defines, vec!["LINUX"]);
        assert_eq!(c.profiles.dev.optimize, 1);
        assert!(!c.profiles.dev.heaptrc);
        assert_eq!(c.profiles.release.optimize, 3);
        assert_eq!(c.llm.max_diagnostics, 5);
        assert_eq!(c.llm.context_lines, 3);
        assert_eq!(c.level_for("P0301"), Some(LevelSetting::Deny));
        assert_eq!(c.level_for("p0901"), Some(LevelSetting::Warn));
        assert_eq!(c.level_for("P0902"), Some(LevelSetting::Allow));
        assert_eq!(c.level_for("P9999"), None);
        assert_eq!(c.per_unit.len(), 2);
    }

    #[test]
    fn per_unit_is_not_mistaken_for_a_lint_code() {
        let c = Config::from_str("[lints.per-unit]\n\"src/**\" = \"strict\"\n").unwrap();
        assert!(c.lint_levels.is_empty());
        assert_eq!(c.per_unit, vec![("src/**".to_string(), Strictness::Strict)]);
    }

    #[test]
    fn later_per_unit_glob_wins() {
        let c = Config::from_str(
            "[lints.per-unit]\n\"src/**\" = \"strict\"\n\"src/legacy/**\" = \"lenient\"\n",
        )
        .unwrap();
        assert_eq!(c.strictness_for(Path::new("src/a.pas")), Strictness::Strict);
        assert_eq!(c.strictness_for(Path::new("src/legacy/a.pas")), Strictness::Lenient);
    }

    #[test]
    fn unannotated_units_default_to_lenient() {
        let c = Config::defaults();
        assert_eq!(c.strictness_for(Path::new("whatever.pas")), Strictness::Lenient);
    }

    #[test]
    fn bad_lint_level_is_rejected() {
        let e = Config::from_str("[lints]\nP0301 = \"nope\"\n").unwrap_err();
        assert!(format!("{e}").contains("invalid lint level"), "{e}");
    }

    #[test]
    fn bad_toml_is_a_config_error_not_a_panic() {
        let e = Config::from_str("this is not toml ===").unwrap_err();
        assert!(matches!(e, Error::Config(_)));
    }

    #[test]
    fn glob_matcher_semantics() {
        assert!(glob_match("src/**", "src/a/b.pas"));
        assert!(glob_match("*.pas", "a.pas"));
        assert!(!glob_match("*.pas", "src/a.pas"), "* must not cross /");
        assert!(glob_match("src/?.pas", "src/a.pas"));
        assert!(!glob_match("src/?.pas", "src/ab.pas"));
        assert!(glob_match("**/legacy/**", "a/b/legacy/c.pas"));
        assert!(glob_match("src/**", "./src/a.pas"));
        assert!(glob_match("exact.pas", "exact.pas"));
        assert!(!glob_match("exact.pas", "exact2.pas"));
    }

    #[test]
    fn init_template_is_valid_config() {
        let t = Config::init_template("demo");
        let c = Config::from_str(&t).unwrap();
        assert_eq!(c.package.name, "demo");
        assert_eq!(c.package.main.as_deref(), Some("src/demo.pas"));
    }

    #[test]
    fn profile_name_parsing() {
        assert_eq!(ProfileName::parse("dev").unwrap(), ProfileName::Dev);
        assert_eq!(ProfileName::parse("RELEASE").unwrap(), ProfileName::Release);
        assert!(ProfileName::parse("prod").is_err());
    }

    #[test]
    fn discover_finds_config_upward() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), "[package]\nname = \"up\"\n").unwrap();
        let c = Config::discover(&nested).unwrap();
        assert_eq!(c.package.name, "up");
        assert_eq!(c.root.as_deref(), Some(dir.path()));
    }

    #[test]
    fn discover_without_config_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::discover(dir.path()).unwrap();
        assert_eq!(c.package.name, "");
        assert!(c.root.is_none());
    }
}
