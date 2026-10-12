//! The command-line driver (spec section 3).
//!
//! Exit codes are part of the contract: `0` clean, `1` diagnostics at error
//! level, `2` the tool or its configuration failed. `2` is deliberately
//! distinct from "your code is wrong".

use crate::analyzer::{self, AnalysisOptions};
use crate::catalog::Catalog;
use crate::config::{Config, LevelSetting, ProfileName, CONFIG_FILE_NAME};
use crate::diagnostic::{Diagnostic, Level};
use crate::error::{Error, Result};
use crate::fpc::{self, FpcMode};
use crate::render::{self, MessageFormat, RenderOptions};
use crate::source::SourceManager;
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "fpcheck",
    version,
    about = "A Rust-style feedback layer for Free Pascal",
    long_about = "Precise, structured, actionable diagnostics for Free Pascal.\n\
                  `fpcheck check --message-format=llm` is the loop an agent should use."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Path to fpcheck.toml (default: search upwards from the source path)
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// human | json | llm | sarif
    #[arg(long, global = true, value_name = "FORMAT", default_value = "human")]
    pub message_format: String,

    /// Cap the human and llm renderers at N diagnostics (0 = no cap)
    #[arg(long, global = true, value_name = "N", default_value_t = 15)]
    pub max_diagnostics: usize,

    /// dev | release
    #[arg(long, global = true, value_name = "NAME", default_value = "dev")]
    pub profile: String,

    /// Force a diagnostic code to error level
    #[arg(long, global = true, value_name = "CODE")]
    pub deny: Vec<String>,

    /// Force a diagnostic code to warning level
    #[arg(long, global = true, value_name = "CODE")]
    pub warn: Vec<String>,

    /// Silence a diagnostic code
    #[arg(long, global = true, value_name = "CODE")]
    pub allow: Vec<String>,

    /// Exit non-zero when warnings are present
    #[arg(long, global = true)]
    pub deny_warnings: bool,

    /// Accepted for compatibility; the llm format never colorizes
    #[arg(long, global = true)]
    pub color: bool,

    /// Package name to report (informational)
    #[arg(short = 'p', long = "package", global = true, value_name = "NAME")]
    pub package: Option<String>,

    /// Source files or directories (default: the configured sources)
    #[arg(value_name = "PATH", global = true)]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Analyze sources and type-check with fpc; produces no binary
    Check {
        /// Run the analyzer only, without invoking fpc
        #[arg(long)]
        no_fpc: bool,
        /// List every `{@allow}` suppression with its reason
        #[arg(long)]
        report_allows: bool,
    },
    /// check, then compile and link with the selected profile
    Build,
    /// Build and run FPCUnit tests (milestone M2)
    Test,
    /// Apply every MachineApplicable suggestion
    Fix {
        /// Show what would change without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Reformat sources with FPC's ptop
    Fmt {
        /// Report files that would change instead of writing them
        #[arg(long)]
        check: bool,
    },
    /// Long-form explanation of a diagnostic code
    Explain { code: String },
    /// Write a starter fpcheck.toml
    Init {
        /// Also write an AGENTS.md snippet for coding agents
        #[arg(long)]
        agent: bool,
        /// Overwrite existing files
        #[arg(long)]
        force: bool,
    },
    /// Language server (milestone M4)
    Lsp,
}

/// Parse arguments, run, and translate the result into an exit code.
pub fn main_entry() -> i32 {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            error.exit_code()
        }
    }
}

pub fn run(cli: Cli) -> Result<i32> {
    match &cli.command {
        Command::Check {
            no_fpc,
            report_allows,
        } => check(&cli, FpcMode::CheckOnly, *no_fpc, *report_allows),
        Command::Build => check(&cli, FpcMode::Build, false, false),
        Command::Fix { dry_run } => fix(&cli, *dry_run),
        Command::Fmt { check } => fmt(&cli, *check),
        Command::Explain { code } => {
            let session = Session::load(&cli)?;
            let text = crate::explain::render(code, &session.catalog, &session.root.join("docs"))?;
            print!("{text}");
            Ok(0)
        }
        Command::Init { agent, force } => init(&cli, *agent, *force),
        Command::Test => Err(Error::Usage(
            "`fpcheck test` is not implemented yet: it needs an FPC toolchain for the FPCUnit \
             runner and heaptrc parsing (milestone M2)"
                .into(),
        )),
        Command::Lsp => Err(Error::Usage(
            "`fpcheck lsp` is not implemented yet (milestone M4)".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

struct Session {
    config: Config,
    root: PathBuf,
    catalog: Catalog,
    overrides: BTreeMap<String, LevelSetting>,
    profile_name: ProfileName,
    format: MessageFormat,
    render_opts: RenderOptions,
    deny_warnings: bool,
}

impl Session {
    fn load(cli: &Cli) -> Result<Self> {
        let mut config = load_config(cli)?;
        // `FPCHECK_FPC` wins over configuration, so a specific compiler can be
        // forced; the real-compiler tests and CI use this to select one that is
        // not on PATH.
        if let Ok(binary) = std::env::var("FPCHECK_FPC") {
            let binary = binary.trim();
            if !binary.is_empty() {
                config.fpc.binary = binary.to_string();
            }
        }
        let root = match &config.root {
            Some(root) => root.clone(),
            None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        };

        let catalog_dir = root.join("catalog");
        let catalog = if catalog_dir.is_dir() {
            Catalog::load_dir(&catalog_dir)?
        } else {
            Catalog::builtin()
        };

        let mut overrides = config.lint_levels.clone();
        for code in &cli.deny {
            overrides.insert(code.to_ascii_uppercase(), LevelSetting::Deny);
        }
        for code in &cli.warn {
            overrides.insert(code.to_ascii_uppercase(), LevelSetting::Warn);
        }
        for code in &cli.allow {
            overrides.insert(code.to_ascii_uppercase(), LevelSetting::Allow);
        }

        Ok(Session {
            profile_name: ProfileName::parse(&cli.profile)?,
            format: MessageFormat::parse(&cli.message_format)?,
            render_opts: RenderOptions {
                max_diagnostics: cli.max_diagnostics,
                context_lines: config.llm.context_lines,
                color: cli.color,
            },
            deny_warnings: cli.deny_warnings,
            config,
            root,
            catalog,
            overrides,
        })
    }

    fn analysis_options(&self, path: &Path) -> AnalysisOptions {
        AnalysisOptions {
            level_overrides: self.overrides.clone(),
            catalog: self.catalog.clone(),
            strictness: self.config.strictness_for(path),
        }
    }
}

fn load_config(cli: &Cli) -> Result<Config> {
    if let Some(path) = &cli.config {
        return Config::load(path);
    }
    let start = cli
        .paths
        .first()
        .map(|p| {
            if p.is_dir() {
                p.clone()
            } else {
                p.parent()
                    .map(|d| d.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            }
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    Config::discover(&start)
}

/// Source roots: explicit paths win, then the configured sources.
fn source_roots(cli: &Cli, session: &Session) -> Vec<String> {
    if !cli.paths.is_empty() {
        return cli.paths.iter().map(|p| p.display().to_string()).collect();
    }
    let configured = if session.config.package.sources.is_empty() {
        vec!["src".to_string()]
    } else {
        session.config.package.sources.clone()
    };
    configured
        .iter()
        .map(|root| resolve_against(&session.root, root))
        .collect()
}

/// Keep paths relative when the project root is the working directory (so
/// diagnostics stay readable), otherwise make them absolute.
fn resolve_against(root: &Path, path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return path.to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) if cwd == root => path.to_string(),
        _ => root.join(candidate).display().to_string(),
    }
}

// ---------------------------------------------------------------------------
// check / build
// ---------------------------------------------------------------------------

fn check(cli: &Cli, mode: FpcMode, no_fpc: bool, report_allows: bool) -> Result<i32> {
    let session = Session::load(cli)?;
    let roots = source_roots(cli, &session);
    let files = analyzer::collect_sources(&roots)?;

    let mut sources = SourceManager::with_base(session.root.clone());
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut suppressed_by_allow = 0usize;

    for path in &files {
        let result = analyzer::analyze_file(path, &session.analysis_options(path))?;
        suppressed_by_allow += result.suppressed_by_allow;
        diagnostics.extend(result.diagnostics);
    }

    let mut fatal = false;
    if !no_fpc {
        if let Some(target) = target_source(cli, &session, &files) {
            let profile = fpc::build_profile(&session.config, session.profile_name);
            let out_dir = session
                .root
                .join("target")
                .join(session.profile_name.as_str());
            std::fs::create_dir_all(&out_dir)
                .map_err(|e| Error::io(out_dir.display().to_string(), e))?;
            let extras = vec![
                format!("-FE{}", out_dir.display()),
                format!("-FU{}", out_dir.display()),
            ];

            let reported = relative_to(&target, &session.root);
            let output = fpc::run_fpc(&profile, &reported, mode, &extras, &session.root)?;

            let normalize_opts =
                fpc::NormalizeOptions::new(session.catalog.clone(), session.root.clone())
                    .with_overrides(session.overrides.clone());
            let raw = fpc::parse_output(&output.output);
            let normalized = fpc::normalize(&raw, &normalize_opts, &mut sources);
            fatal = normalized.fatal;
            diagnostics.extend(normalized.diagnostics);
        }
    }

    sort_diagnostics(&mut diagnostics);
    let text = render::render(
        session.format,
        &diagnostics,
        &mut sources,
        &session.render_opts,
    );
    if !text.is_empty() {
        print!("{text}");
        let _ = std::io::stdout().flush();
    }

    if report_allows {
        report_allows_for(&files, suppressed_by_allow)?;
    }

    if mode == FpcMode::Build && !fatal {
        // Nothing extra to print: a successful build is silent, like cargo.
    }

    Ok(exit_code(&diagnostics, session.deny_warnings))
}

/// Which file to hand to fpc.
fn target_source(cli: &Cli, session: &Session, files: &[PathBuf]) -> Option<PathBuf> {
    if let Some(explicit) = cli.paths.iter().find(|p| p.is_file()) {
        return Some(explicit.clone());
    }
    if let Some(main) = &session.config.package.main {
        return Some(PathBuf::from(resolve_against(&session.root, main)));
    }
    files.first().cloned()
}

fn relative_to(path: &Path, base: &Path) -> PathBuf {
    path.strip_prefix(base)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf())
}

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

fn exit_code(diags: &[Diagnostic], deny_warnings: bool) -> i32 {
    let has_error = diags.iter().any(|d| d.level == Level::Error);
    let has_warning = diags.iter().any(|d| d.level == Level::Warning);
    if has_error || (deny_warnings && has_warning) {
        1
    } else {
        0
    }
}

/// `fpcheck check --report-allows` (spec section 6.3): show every suppression
/// so a reviewer can audit it.
fn report_allows_for(files: &[PathBuf], suppressed: usize) -> Result<()> {
    println!("suppressions: {suppressed}");
    for path in files {
        let text =
            std::fs::read_to_string(path).map_err(|e| Error::io(path.display().to_string(), e))?;
        let tokens = crate::lexer::tokenize(path, &text);
        for allow in analyzer::collect_allows(&tokens) {
            let codes = if allow.codes.is_empty() {
                "(no code)".to_string()
            } else {
                allow.codes.join(", ")
            };
            let reason = allow
                .reason
                .clone()
                .unwrap_or_else(|| "<missing reason>".to_string());
            println!(
                "  {}:{}  {codes}  reason: {reason}",
                path.display(),
                allow.span.line_start
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// fix
// ---------------------------------------------------------------------------

fn fix(cli: &Cli, dry_run: bool) -> Result<i32> {
    let session = Session::load(cli)?;
    let roots = source_roots(cli, &session);
    let files = analyzer::collect_sources(&roots)?;

    let mut diagnostics = Vec::new();
    for path in &files {
        diagnostics
            .extend(analyzer::analyze_file(path, &session.analysis_options(path))?.diagnostics);
    }

    let plan = crate::fix::plan(&diagnostics)?;
    if !dry_run {
        crate::fix::apply(&plan)?;
    }
    println!("{}", crate::fix::summarize(&plan, dry_run));
    if dry_run {
        for file in &plan {
            if file.changed() {
                println!("--- {}", file.path.display());
            }
        }
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// fmt
// ---------------------------------------------------------------------------

fn fmt(cli: &Cli, check_only: bool) -> Result<i32> {
    let session = Session::load(cli)?;
    let roots = source_roots(cli, &session);
    let files = analyzer::collect_sources(&roots)?;

    let mut changed = 0usize;
    for path in &files {
        let original =
            std::fs::read_to_string(path).map_err(|e| Error::io(path.display().to_string(), e))?;
        let formatted = run_filter("ptop", &original)?;
        if formatted != original {
            changed += 1;
            if check_only {
                println!("would reformat {}", path.display());
            } else {
                std::fs::write(path, formatted)
                    .map_err(|e| Error::io(path.display().to_string(), e))?;
                println!("reformatted {}", path.display());
            }
        }
    }

    if check_only && changed > 0 {
        return Ok(1);
    }
    Ok(0)
}

/// Pipe `input` through a filter program and return its stdout.
fn run_filter(program: &str, input: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::FpcNotFound(format!(
                    "`{program}` was not found; install Free Pascal (it ships ptop) or format \
                     with another tool"
                ))
            } else {
                Error::io(program.to_string(), e)
            }
        })?;
    child
        .stdin
        .as_mut()
        .ok_or_else(|| Error::internal("could not open stdin for ptop"))?
        .write_all(input.as_bytes())
        .map_err(|e| Error::io(program.to_string(), e))?;
    let output = child
        .wait_with_output()
        .map_err(|e| Error::io(program.to_string(), e))?;
    if !output.status.success() {
        return Err(Error::Usage(format!(
            "`{program}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

fn init(_cli: &Cli, agent: bool, force: bool) -> Result<i32> {
    let cwd = std::env::current_dir().map_err(|e| Error::io(".", e))?;
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());

    let config_path = cwd.join(CONFIG_FILE_NAME);
    if config_path.exists() && !force {
        return Err(Error::Usage(format!(
            "{} already exists (use --force to overwrite)",
            config_path.display()
        )));
    }
    std::fs::write(&config_path, Config::init_template(&name))
        .map_err(|e| Error::io(config_path.display().to_string(), e))?;
    println!("wrote {}", config_path.display());

    match detect_fpc(&cwd) {
        Some(version) => println!("detected fpc {version}"),
        None => println!("note: no `fpc` on PATH; fpcheck check will need one (or --no-fpc)"),
    }

    if agent {
        let agents_path = cwd.join("AGENTS.md");
        if agents_path.exists() && !force {
            println!(
                "left {} untouched (use --force to overwrite)",
                agents_path.display()
            );
        } else {
            std::fs::write(&agents_path, AGENT_SNIPPET)
                .map_err(|e| Error::io(agents_path.display().to_string(), e))?;
            println!("wrote {}", agents_path.display());
        }
    }

    Ok(0)
}

/// The agent-facing instructions from spec section 12.
pub const AGENT_SNIPPET: &str = "\
# Working with fpcheck

- After every edit run `fpcheck check --message-format=llm` and fix all errors before moving on.
- Fix root causes first; later errors often disappear.
- Run `fpcheck explain <code>` when a diagnostic is unclear.
- Never add `{@allow}` or `{@unsafe}` to silence a diagnostic without a real reason string.
- Apply `fpcheck fix` for MachineApplicable suggestions, then re-check.
- Before declaring a task done: `fpcheck test` must pass with no leaks.
- Prefer `TOption`/`TResult`/`TBox` from rustlike.pas over nil, error codes, and bare Create/Free.
";

fn detect_fpc(cwd: &Path) -> Option<String> {
    let output = std::process::Command::new("fpc")
        .arg("-iV")
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Source, Span};

    fn parse(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("fpcheck").chain(args.iter().copied()))
    }

    #[test]
    fn check_flags_parse() {
        let cli = parse(&["check"]);
        assert!(matches!(
            cli.command,
            Command::Check {
                no_fpc: false,
                report_allows: false
            }
        ));
    }

    #[test]
    fn check_flag_variants_parse() {
        let cli = parse(&["check", "--no-fpc", "--report-allows"]);
        match cli.command {
            Command::Check {
                no_fpc,
                report_allows,
            } => {
                assert!(no_fpc);
                assert!(report_allows);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn global_flags_parse() {
        let cli = parse(&[
            "check",
            "--message-format=llm",
            "--max-diagnostics=3",
            "--profile=release",
            "--deny=P0301",
            "--warn",
            "P0902",
            "--allow=P0703",
            "--deny-warnings",
        ]);
        assert_eq!(cli.message_format, "llm");
        assert_eq!(cli.max_diagnostics, 3);
        assert_eq!(cli.profile, "release");
        assert_eq!(cli.deny, vec!["P0301"]);
        assert_eq!(cli.warn, vec!["P0902"]);
        assert_eq!(cli.allow, vec!["P0703"]);
        assert!(cli.deny_warnings);
    }

    #[test]
    fn subcommands_parse() {
        assert!(matches!(parse(&["build"]).command, Command::Build));
        assert!(matches!(parse(&["test"]).command, Command::Test));
        assert!(matches!(parse(&["lsp"]).command, Command::Lsp));
        match parse(&["fix", "--dry-run"]).command {
            Command::Fix { dry_run } => assert!(dry_run),
            other => panic!("{other:?}"),
        }
        match parse(&["fmt", "--check"]).command {
            Command::Fmt { check } => assert!(check),
            other => panic!("{other:?}"),
        }
        match parse(&["explain", "P0201"]).command {
            Command::Explain { code } => assert_eq!(code, "P0201"),
            other => panic!("{other:?}"),
        }
        match parse(&["init", "--agent", "--force"]).command {
            Command::Init { agent, force } => {
                assert!(agent);
                assert!(force);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn paths_are_positional() {
        let cli = parse(&["check", "src", "tests"]);
        assert_eq!(
            cli.paths,
            vec![PathBuf::from("src"), PathBuf::from("tests")]
        );
    }

    #[test]
    fn exit_code_zero_when_clean() {
        assert_eq!(exit_code(&[], false), 0);
    }

    #[test]
    fn exit_code_one_on_errors() {
        let d = Diagnostic::new(
            "P0702",
            Level::Error,
            "x",
            Span::point("a.pas", 1, 1),
            Source::Fpcheck,
        );
        assert_eq!(exit_code(&[d], false), 1);
    }

    #[test]
    fn warnings_pass_unless_denied() {
        let d = Diagnostic::new(
            "P0703",
            Level::Warning,
            "x",
            Span::point("a.pas", 1, 1),
            Source::Fpcheck,
        );
        assert_eq!(exit_code(std::slice::from_ref(&d), false), 0);
        assert_eq!(exit_code(&[d], true), 1);
    }

    #[test]
    fn notes_never_fail_the_build() {
        let d = Diagnostic::new(
            "P0009",
            Level::Note,
            "x",
            Span::point("a.pas", 1, 1),
            Source::Fpcheck,
        );
        assert_eq!(exit_code(&[d], true), 0);
    }

    #[test]
    fn relative_to_strips_the_project_root() {
        assert_eq!(
            relative_to(Path::new("/p/src/a.pas"), Path::new("/p")),
            PathBuf::from("src/a.pas")
        );
        assert_eq!(
            relative_to(Path::new("/other/a.pas"), Path::new("/p")),
            PathBuf::from("/other/a.pas")
        );
    }
}
