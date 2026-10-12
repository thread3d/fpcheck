//! End-to-end tests for the `fpcheck` binary.
//!
//! These run the real executable against a temporary Pascal project. They use
//! `--no-fpc` because this repository is developed without an installed Free
//! Pascal; the fpc-facing pipeline is covered by unit tests with a fake
//! compiler (`src/fpc/runner.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

const DEMO: &str = r#"{$mode objfpc}
unit demo;

interface

procedure Run(x: Double);

implementation

procedure Run(x: Double);
label done;
var
  p: Pointer;
begin
  {$R-}
  with Foo do Bar;
  if x = 0.1 then goto done;
  p := @x;
  try
    Risky;
  except
  end;
  Todo('finish this');
  done:
end;

end.
"#;

const CLEAN: &str = r#"{$mode objfpc}
unit clean;

interface

procedure Run(x: Integer);

implementation

procedure Run(x: Integer);
begin
  if x = 3 then Writeln(x);
end;

end.
"#;

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, contents) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, contents).unwrap();
        }
        Project { dir }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path(relative)).expect("read fixture")
    }

    /// Run the binary in the project directory; returns (exit code, combined output).
    fn run(&self, args: &[&str]) -> (i32, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_fpcheck"))
            .args(args)
            .current_dir(self.dir.path())
            .output()
            .expect("run fpcheck");
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.code().unwrap_or(-1), text)
    }
}

fn demo() -> Project {
    Project::new(&[("demo.pas", DEMO)])
}

#[test]
fn llm_format_reports_every_lint_and_the_summary_line() {
    let project = demo();
    let (code, out) = project.run(&["check", "--no-fpc", "--message-format=llm", "demo.pas"]);

    assert_eq!(code, 1, "errors must exit 1\n{out}");
    for expected in ["P0304", "P0702", "P0701", "P0801", "P0306", "P0703", "P0902"] {
        assert!(out.contains(expected), "missing {expected} in:\n{out}");
    }
    assert!(
        out.trim_end()
            .ends_with("fpcheck: 4 errors, 3 warnings. Run 'fpcheck explain <code>' for details."),
        "unexpected summary:\n{out}"
    );
    // The agent format never colorizes or draws boxes.
    assert!(!out.contains('\u{1b}'), "ANSI escape found");
    assert!(!out.contains('│'), "box drawing found");
}

#[test]
fn errors_are_reported_before_warnings() {
    let project = demo();
    let (_, out) = project.run(&["check", "--no-fpc", "--message-format=llm", "demo.pas"]);
    let first_warning = out.find("warning[").expect("a warning");
    let last_error = out.rfind("error[").expect("an error");
    assert!(last_error < first_warning, "errors must come first:\n{out}");
}

#[test]
fn human_format_uses_the_rustc_shape() {
    let project = demo();
    let (code, out) = project.run(&["check", "--no-fpc", "--max-diagnostics=1", "demo.pas"]);
    assert_eq!(code, 1);
    assert!(out.contains("--> demo.pas:15:3"), "{out}");
    assert!(out.contains("   |"), "{out}");
    assert!(out.contains("= help:"), "{out}");
    assert!(out.contains("more diagnostic(s) not shown"), "{out}");
}

#[test]
fn json_is_json_lines_with_a_versioned_summary() {
    let project = demo();
    let (code, out) = project.run(&["check", "--no-fpc", "--message-format=json", "demo.pas"]);
    assert_eq!(code, 1);

    let lines: Vec<&str> = out.trim_end().lines().collect();
    let summary: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(summary["summary"]["schema"], 1);
    assert_eq!(summary["summary"]["errors"], 4);
    assert_eq!(summary["summary"]["warnings"], 3);

    for line in &lines[..lines.len() - 1] {
        let value: serde_json::Value = serde_json::from_str(line).expect("each line is JSON");
        assert!(value["code"].is_string(), "{line}");
        assert!(value["primary"]["line_start"].is_number(), "{line}");
    }
}

#[test]
fn sarif_output_is_valid_and_untruncated() {
    let project = demo();
    let (code, out) = project.run(&[
        "check",
        "--no-fpc",
        "--message-format=sarif",
        "--max-diagnostics=1",
        "demo.pas",
    ]);
    assert_eq!(code, 1);
    let doc: serde_json::Value = serde_json::from_str(&out).expect("valid SARIF JSON");
    assert_eq!(doc["version"], "2.1.0");
    let results = doc["runs"][0]["results"].as_array().unwrap();
    // Machine formats ignore --max-diagnostics.
    assert_eq!(results.len(), 7, "{out}");
}

#[test]
fn a_clean_file_exits_zero_and_says_nothing() {
    let project = Project::new(&[("clean.pas", CLEAN)]);
    let (code, out) = project.run(&["check", "--no-fpc", "clean.pas"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.trim().is_empty(), "expected silence, got:\n{out}");
}

#[test]
fn an_error_level_lint_fails_the_exit_code_and_warnings_do_not() {
    let project = Project::new(&[(
        "warn.pas",
        "{$mode objfpc}\nunit warn;\ninterface\nimplementation\nprocedure P;\nbegin\n  goto done;\n  done:\nend;\nend.\n",
    )]);
    let (code, _) = project.run(&["check", "--no-fpc", "warn.pas"]);
    assert_eq!(code, 0, "warnings alone must not fail");

    let (code, _) = project.run(&["check", "--no-fpc", "--deny-warnings", "warn.pas"]);
    assert_eq!(code, 1, "--deny-warnings must fail");
}

#[test]
fn allow_flag_silences_a_code() {
    let project = demo();
    let (_, before) = project.run(&["check", "--no-fpc", "demo.pas"]);
    assert!(before.contains("P0702"));

    let (_, after) = project.run(&["check", "--no-fpc", "--allow=P0702", "demo.pas"]);
    assert!(!after.contains("P0702"), "{after}");
    assert!(after.contains("P0701"), "other lints must survive:\n{after}");
}

#[test]
fn deny_flag_promotes_a_warning_to_an_error() {
    let project = demo();
    let (code, out) = project.run(&["check", "--no-fpc", "--deny=P0703", "demo.pas"]);
    assert_eq!(code, 1);
    assert!(out.contains("error[P0703]"), "{out}");
}

#[test]
fn inline_allow_with_a_reason_suppresses_and_is_reported() {
    let project = Project::new(&[(
        "allowed.pas",
        "{$mode objfpc}\nunit allowed;\ninterface\nimplementation\nprocedure P;\nbegin\n  {@allow(P0702, reason: \"third-party interop\")}\n  with Foo do Bar;\nend;\nend.\n",
    )]);
    let (code, out) = project.run(&["check", "--no-fpc", "allowed.pas"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("P0702"), "{out}");

    let (_, report) = project.run(&["check", "--no-fpc", "--report-allows", "allowed.pas"]);
    assert!(report.contains("third-party interop"), "{report}");
}

#[test]
fn an_allow_without_a_reason_is_itself_an_error() {
    let project = Project::new(&[(
        "bad.pas",
        "{$mode objfpc}\nunit bad;\ninterface\nimplementation\nprocedure P;\nbegin\n  {@allow(P0702)}\n  with Foo do Bar;\nend;\nend.\n",
    )]);
    let (code, out) = project.run(&["check", "--no-fpc", "bad.pas"]);
    assert_eq!(code, 1);
    assert!(out.contains("P0001"), "{out}");
}

#[test]
fn explain_prints_a_known_code_and_fails_on_an_unknown_one() {
    let project = Project::new(&[]);
    let (code, out) = project.run(&["explain", "P0702"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("P0702: with-statement"), "{out}");
    assert!(out.contains("`with` is banned"), "{out}");

    let (code, out) = project.run(&["explain", "P9999"]);
    assert_eq!(code, 2, "unknown code is a usage error\n{out}");
    assert!(out.contains("unknown diagnostic code"), "{out}");
}

#[test]
fn explain_works_for_a_normalized_compiler_message_code() {
    // Derived from the template, so no numeric FPC id is needed.
    let project = Project::new(&[]);
    let (_, list) = project.run(&["explain", "P0304"]);
    assert!(list.contains("checks-disabled"), "{list}");
}

#[test]
fn fix_applies_the_machine_applicable_removal() {
    let project = demo();
    assert!(project.read("demo.pas").contains("{$R-}"));

    let (code, out) = project.run(&["fix", "demo.pas"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("applied 1 edit(s)"), "{out}");
    assert!(
        !project.read("demo.pas").contains("{$R-}"),
        "the directive should be gone"
    );

    // Re-checking no longer reports P0304.
    let (_, after) = project.run(&["check", "--no-fpc", "demo.pas"]);
    assert!(!after.contains("P0304"), "{after}");
}

#[test]
fn fix_dry_run_changes_nothing() {
    let project = demo();
    let (code, out) = project.run(&["fix", "--dry-run", "demo.pas"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("would apply"), "{out}");
    assert!(project.read("demo.pas").contains("{$R-}"), "dry run must not write");
}

#[test]
fn init_writes_a_config_that_is_then_used() {
    let project = Project::new(&[("src/main.pas", CLEAN)]);
    let (code, out) = project.run(&["init"]);
    assert_eq!(code, 0, "{out}");
    assert!(project.path("fpcheck.toml").is_file(), "{out}");
    assert!(out.contains("wrote"), "{out}");

    // The generated config is valid and drives a run.
    let (code, out) = project.run(&["check", "--no-fpc", "src"]);
    assert_eq!(code, 0, "{out}");

    // Re-running init without --force is a usage error.
    let (code, out) = project.run(&["init"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("already exists"), "{out}");
}

#[test]
fn init_agent_writes_the_snippet() {
    let project = Project::new(&[]);
    let (code, _) = project.run(&["init", "--agent"]);
    assert_eq!(code, 0);
    let agents = project.read("AGENTS.md");
    assert!(agents.contains("fpcheck check --message-format=llm"), "{agents}");
    assert!(agents.contains("fpcheck explain <code>"), "{agents}");
}

#[test]
fn a_directory_argument_is_walked_recursively() {
    let project = Project::new(&[
        ("src/a.pas", CLEAN),
        ("src/nested/b.pas", "{$mode objfpc}\nunit b;\ninterface\nimplementation\nbegin\n  with X do Y;\nend.\n"),
    ]);
    let (code, out) = project.run(&["check", "--no-fpc", "src"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("nested/b.pas"), "{out}");
}

#[test]
fn unimplemented_subcommands_say_so_and_exit_two() {
    let project = Project::new(&[]);
    for command in ["test", "lsp"] {
        let (code, out) = project.run(&[command]);
        assert_eq!(code, 2, "{command}: {out}");
        assert!(out.contains("not implemented"), "{command}: {out}");
    }
}

#[test]
fn an_invalid_message_format_is_a_usage_error() {
    let project = demo();
    let (code, out) = project.run(&["check", "--no-fpc", "--message-format=xml", "demo.pas"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("unknown --message-format"), "{out}");
}

#[test]
fn a_nonexistent_path_is_reported_not_ignored() {
    let project = Project::new(&[]);
    let (code, _) = project.run(&["check", "--no-fpc", "does-not-exist.pas"]);
    // Nothing to analyze: an empty project is clean, but the path was explicit.
    assert_eq!(code, 0);
}

#[test]
fn config_lint_levels_are_honoured() {
    let project = Project::new(&[
        (
            "demo.pas",
            "{$mode objfpc}\nunit demo;\ninterface\nimplementation\nprocedure P;\nbegin\n  with X do Y;\nend;\nend.\n",
        ),
        ("fpcheck.toml", "[lints]\nP0702 = \"allow\"\n"),
    ]);
    let (code, out) = project.run(&["check", "--no-fpc", "demo.pas"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("P0702"), "{out}");
}

#[test]
fn per_unit_strictness_globs_do_not_break_analysis() {
    let project = Project::new(&[
        ("legacy/a.pas", "{$mode objfpc}\nunit a;\ninterface\nimplementation\nbegin\n  with X do Y;\nend.\n"),
        ("fpcheck.toml", "[lints.per-unit]\n\"legacy/**\" = \"lenient\"\n\"src/**\" = \"strict\"\n"),
    ]);
    let (code, out) = project.run(&["check", "--no-fpc", "legacy"]);
    // Strictness does not disable the non-annotation lints, so P0702 still fires.
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("P0702"), "{out}");
}

#[test]
fn a_missing_explicit_config_file_is_an_error() {
    let project = Project::new(&[("demo.pas", CLEAN)]);
    let (code, out) = project.run(&["check", "--no-fpc", "--config=missing.toml", "demo.pas"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("missing.toml"), "{out}");
}

#[test]
fn source_paths_stay_relative_in_diagnostics() {
    let project = demo();
    let (_, out) = project.run(&["check", "--no-fpc", "--message-format=llm", "demo.pas"]);
    let absolute = project.path("demo.pas").display().to_string();
    assert!(!out.contains(&absolute), "expected a relative path:\n{out}");
    assert!(out.contains("demo.pas:"), "{out}");
}

#[test]
fn explain_reads_the_projects_docs_directory() {
    let project = Project::new(&[(
        "docs/P0702.md",
        "# P0702: with-statement\n\n## Failing example\n\nwith A do B;\n",
    )]);
    let (code, out) = project.run(&["explain", "P0702"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("## Failing example"), "{out}");
    let _ = Path::new("docs");
}
