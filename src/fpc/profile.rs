//! Strict compiler profiles (spec section 4.1).
//!
//! The flag list is derived from the spec's stated *intent* and made
//! overridable: `[fpc] extra` appends flags, and the checks list selects which
//! runtime checks are enabled. The spec notes that every flag should be
//! verified against the installed FPC (`fpc -i`, `fpc -h`); this repository was
//! developed without an installed FPC, so the mapping below is deliberately
//! data-driven rather than clever, and unknown check names are ignored instead
//! of guessed at.

use crate::config::{Config, ProfileName};
use std::path::Path;

/// What the compiler is being asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpcMode {
    /// Analyze and type-check, do not link (spec section 3, `fpcheck check`).
    CheckOnly,
    /// Compile and link (`fpcheck build`).
    Build,
}

impl FpcMode {
    /// Extra flags for this mode. `-Cn` omits the linking stage.
    pub fn flags(self) -> &'static [&'static str] {
        match self {
            FpcMode::CheckOnly => &["-Cn"],
            FpcMode::Build => &[],
        }
    }
}

/// A resolved compiler invocation: which program, with which flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpcProfile {
    pub name: ProfileName,
    pub program: String,
    pub flags: Vec<String>,
}

impl FpcProfile {
    /// Arguments only (argv[0] is [`FpcProfile::program`]).
    pub fn argv(&self, source: &Path, mode: FpcMode, extras: &[String]) -> Vec<String> {
        let mut argv: Vec<String> = self.flags.clone();
        argv.extend(mode.flags().iter().map(|s| s.to_string()));
        argv.extend(extras.iter().cloned());
        argv.push(source.display().to_string());
        argv
    }

    /// Full command line, for diagnostics such as "I ran this".
    pub fn command_line(&self, source: &Path, mode: FpcMode, extras: &[String]) -> Vec<String> {
        let mut cmd = vec![self.program.clone()];
        cmd.extend(self.argv(source, mode, extras));
        cmd
    }
}

/// The FPC flag for a named runtime check, or `None` if unknown.
pub fn check_flag(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "range" => Some("-Cr"),
        "overflow" => Some("-Co"),
        "io" => Some("-Ci"),
        "stack" => Some("-Ct"),
        "method" => Some("-CR"),
        _ => None,
    }
}

/// Build the profile for `name` from configuration.
pub fn build_profile(cfg: &Config, name: ProfileName) -> FpcProfile {
    let profile = cfg.profiles.get(name);
    let mut flags: Vec<String> = Vec::new();

    for check in &profile.checks {
        if let Some(flag) = check_flag(check) {
            flags.push(flag.to_string());
        }
    }

    if profile.assertions {
        flags.push("-Sa".to_string());
    }

    match profile.debug.to_ascii_lowercase().as_str() {
        "dwarf" => flags.extend(["-g", "-gl", "-gw"].iter().map(|s| s.to_string())),
        "stabs" => flags.extend(["-g", "-gl"].iter().map(|s| s.to_string())),
        "line" => flags.push("-gl".to_string()),
        _ => {}
    }

    if profile.heaptrc {
        flags.push("-gh".to_string());
    }

    if profile.optimize == 0 {
        // Checks and heap traces must stay faithful (spec section 4.1).
        flags.push("-O-".to_string());
    } else {
        flags.push(format!("-O{}", profile.optimize));
    }

    // Warnings, notes, hints and info all on, so nothing is silently dropped.
    flags.push("-vewnhi".to_string());

    if !cfg.fpc.mode.is_empty() {
        flags.push(format!("-M{}", cfg.fpc.mode));
    }
    for define in &cfg.fpc.defines {
        flags.push(format!("-d{define}"));
    }
    for unit_path in &cfg.fpc.units {
        flags.push(format!("-Fu{unit_path}"));
    }

    flags.extend(cfg.fpc.extra.iter().cloned());

    FpcProfile { name, program: cfg.fpc.binary.clone(), flags }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn check_names_map_to_flags() {
        assert_eq!(check_flag("range"), Some("-Cr"));
        assert_eq!(check_flag("overflow"), Some("-Co"));
        assert_eq!(check_flag("method"), Some("-CR"));
        assert_eq!(check_flag("RANGE"), Some("-Cr"));
        assert_eq!(check_flag("nonsense"), None);
    }

    #[test]
    fn dev_profile_is_the_strict_one() {
        let cfg = Config::defaults();
        let p = build_profile(&cfg, ProfileName::Dev);
        for flag in ["-Cr", "-Co", "-Ci", "-Ct", "-CR", "-Sa", "-gh", "-O-"] {
            assert!(p.flags.contains(&flag.to_string()), "missing {flag}: {:?}", p.flags);
        }
        assert!(p.flags.contains(&"-Mobjfpc".to_string()));
        assert!(p.flags.contains(&"-vewnhi".to_string()));
    }

    #[test]
    fn release_profile_keeps_range_and_overflow_but_optimizes() {
        let cfg = Config::defaults();
        let p = build_profile(&cfg, ProfileName::Release);
        assert!(p.flags.contains(&"-Cr".to_string()));
        assert!(p.flags.contains(&"-Co".to_string()));
        assert!(p.flags.contains(&"-O2".to_string()));
        assert!(!p.flags.contains(&"-gh".to_string()));
        assert!(!p.flags.contains(&"-O-".to_string()));
    }

    #[test]
    fn config_extra_is_appended_last() {
        let cfg = Config::from_str("[fpc]\nextra = [\"-Xs\", \"-vw\"]\n").unwrap();
        let p = build_profile(&cfg, ProfileName::Dev);
        assert_eq!(&p.flags[p.flags.len() - 2..], &["-Xs".to_string(), "-vw".to_string()]);
    }

    #[test]
    fn defines_and_unit_paths_become_flags() {
        let cfg = Config::from_str(
            "[fpc]\ndefines = [\"LINUX\", \"DEBUG\"]\nunits = [\"lib/**\", \"/opt/src\"]\n",
        )
        .unwrap();
        let p = build_profile(&cfg, ProfileName::Dev);
        assert!(p.flags.contains(&"-dLINUX".to_string()));
        assert!(p.flags.contains(&"-dDEBUG".to_string()));
        assert!(p.flags.contains(&"-Fu/opt/src".to_string()));
    }

    #[test]
    fn debug_setting_selects_flags() {
        let dwarf = Config::from_str("[profile.dev]\ndebug = \"dwarf\"\n").unwrap();
        assert!(build_profile(&dwarf, ProfileName::Dev).flags.contains(&"-gw".to_string()));
        let none = Config::from_str("[profile.dev]\ndebug = \"\"\n").unwrap();
        let p = build_profile(&none, ProfileName::Dev);
        assert!(!p.flags.contains(&"-g".to_string()));
        assert!(!p.flags.contains(&"-gl".to_string()));
    }

    #[test]
    fn argv_puts_the_source_last() {
        let cfg = Config::defaults();
        let p = build_profile(&cfg, ProfileName::Dev);
        let argv = p.argv(Path::new("src/main.pas"), FpcMode::Build, &[]);
        assert_eq!(argv.last().unwrap(), "src/main.pas");
    }

    #[test]
    fn check_only_omits_linking() {
        let cfg = Config::defaults();
        let p = build_profile(&cfg, ProfileName::Dev);
        let argv = p.argv(Path::new("a.pas"), FpcMode::CheckOnly, &[]);
        assert!(argv.contains(&"-Cn".to_string()), "{argv:?}");
        let build = p.argv(Path::new("a.pas"), FpcMode::Build, &[]);
        assert!(!build.contains(&"-Cn".to_string()));
    }

    #[test]
    fn extras_precede_the_source() {
        let cfg = Config::defaults();
        let p = build_profile(&cfg, ProfileName::Dev);
        let argv = p.argv(Path::new("a.pas"), FpcMode::Build, &["-FEout".into()]);
        assert_eq!(argv[argv.len() - 2], "-FEout");
        assert_eq!(argv[argv.len() - 1], "a.pas");
    }

    #[test]
    fn command_line_includes_the_program() {
        let cfg = Config::defaults();
        let p = build_profile(&cfg, ProfileName::Dev);
        let cmd = p.command_line(Path::new("a.pas"), FpcMode::Build, &[]);
        assert_eq!(cmd[0], "fpc");
    }

    #[test]
    fn unknown_check_names_are_ignored_not_guessed() {
        let cfg = Config::from_str("[profile.dev]\nchecks = [\"range\", \"bogus\"]\n").unwrap();
        let p = build_profile(&cfg, ProfileName::Dev);
        assert!(p.flags.contains(&"-Cr".to_string()));
        assert_eq!(p.flags.iter().filter(|f| f.starts_with("-C")).count(), 1);
    }
}
