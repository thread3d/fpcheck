//! Tests that drive the **real** Free Pascal compiler.
//!
//! These are what make the profile flags and the generated message catalogue
//! trustworthy: they compile real Pascal and check fpcheck against what the
//! compiler actually prints. Every id and template in `catalog/fpc.toml` was
//! confirmed this way against FPC 3.2.2.
//!
//! They **skip** (rather than fail) when no compiler is available, so
//! `cargo test` still passes on a machine without FPC. Set `FPCHECK_FPC` to
//! point at a specific binary.

use std::path::PathBuf;
use std::process::Command;

fn fpc_binary() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("FPCHECK_FPC") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join("fpc"))
        .find(|candidate| candidate.is_file())
}

/// Returns the compiler, or returns early from the test when there is none.
macro_rules! require_fpc {
    () => {
        match fpc_binary() {
            Some(binary) => binary,
            None => {
                eprintln!(
                    "skipping: no fpc on PATH (set FPCHECK_FPC to run the real-compiler tests)"
                );
                return;
            }
        }
    };
}

struct Project {
    dir: tempfile::TempDir,
    fpc: PathBuf,
}

impl Project {
    fn new(fpc: PathBuf, files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, contents) in files {
            std::fs::write(dir.path().join(name), contents).unwrap();
        }
        Project { dir, fpc }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_fpcheck"))
            .args(args)
            .env("FPCHECK_FPC", &self.fpc)
            .current_dir(self.dir.path())
            .output()
            .expect("run fpcheck");
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        (output.status.code().unwrap_or(-1), text)
    }

    /// Run `check --message-format=json` and return the parsed diagnostics.
    fn json_diagnostics(&self, extra: &[&str]) -> Vec<serde_json::Value> {
        let mut args = vec!["check", "--message-format=json"];
        args.extend_from_slice(extra);
        let (_, out) = self.run(&args);
        out.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| value.get("summary").is_none())
            .collect()
    }
}

const CLEAN_PROGRAM: &str = "{$mode objfpc}{$H+}\nprogram clean;\nvar\n  i: Integer;\nbegin\n  i := 2;\n  Writeln(i);\nend.\n";

#[test]
fn clean_code_compiles_with_the_dev_profile_and_exits_zero() {
    // This is the empirical check that every flag `build_profile` emits is
    // accepted by the real compiler: an unknown flag makes compilation fail.
    let fpc = require_fpc!();
    let project = Project::new(fpc, &[("clean.pas", CLEAN_PROGRAM)]);
    let (code, out) = project.run(&["check", "clean.pas"]);
    assert_eq!(code, 0, "expected a clean compile, got:\n{out}");
}

#[test]
fn clean_code_compiles_with_the_release_profile_too() {
    let fpc = require_fpc!();
    let project = Project::new(fpc, &[("clean.pas", CLEAN_PROGRAM)]);
    let (code, out) = project.run(&["check", "--profile=release", "clean.pas"]);
    assert_eq!(code, 0, "expected a clean release compile, got:\n{out}");
}

#[test]
fn build_links_a_real_binary() {
    let fpc = require_fpc!();
    let project = Project::new(fpc, &[("clean.pas", CLEAN_PROGRAM)]);
    let (code, out) = project.run(&["build", "clean.pas"]);
    assert_eq!(code, 0, "expected a successful build, got:\n{out}");
}

#[test]
fn a_real_identifier_error_uses_the_compilers_own_number() {
    let fpc = require_fpc!();
    let project = Project::new(
        fpc,
        &[(
            "broken.pas",
            "{$mode objfpc}{$H+}\nprogram broken;\nbegin\n  Writeln(nope);\nend.\n",
        )],
    );
    let (code, out) = project.run(&["check", "broken.pas"]);
    assert_eq!(code, 1, "expected exit 1, got:\n{out}");

    let diagnostics = project.json_diagnostics(&["broken.pas"]);
    let undeclared = diagnostics
        .iter()
        .find(|d| d["code"] == "F5000")
        .unwrap_or_else(|| panic!("expected F5000 in {diagnostics:#?}"));
    // The catalogue's curated prose, not the raw compiler wording.
    assert!(
        undeclared["message"]
            .as_str()
            .unwrap()
            .contains("undeclared identifier"),
        "{}",
        undeclared["message"]
    );
    assert_eq!(undeclared["source"], "fpc");
    assert_eq!(
        undeclared["explain"], "fpcheck explain F5000",
        "the stable code must be explainable"
    );
}

#[test]
fn the_span_is_expanded_to_the_offending_identifier() {
    let fpc = require_fpc!();
    let project = Project::new(
        fpc,
        &[(
            "broken.pas",
            "{$mode objfpc}{$H+}\nprogram broken;\nbegin\n  Writeln(nope);\nend.\n",
        )],
    );
    let diagnostics = project.json_diagnostics(&["broken.pas"]);
    let d = diagnostics
        .iter()
        .find(|d| d["code"] == "F5000")
        .expect("F5000");
    let primary = &d["primary"];
    assert_eq!(primary["line_start"], 4);
    // `  Writeln(nope);` puts `nope` at column 11.
    assert_eq!(primary["col_start"], 11, "{primary}");
    // FPC reports a point; the span must have been widened over the identifier.
    assert_eq!(primary["col_end"], 15, "{primary}");
}

#[test]
fn cascades_from_a_real_compile_are_suppressed() {
    let fpc = require_fpc!();
    // `nope` is used twice after being reported once: FPC emits the same
    // "Identifier not found" again, which is a cascade, not a second bug.
    let project = Project::new(
        fpc,
        &[(
            "cascade.pas",
            "{$mode objfpc}{$H+}\nprogram cascade;\nbegin\n  Writeln(nope);\n  Writeln(nope);\n  Writeln(nope);\nend.\n",
        )],
    );
    let diagnostics = project.json_diagnostics(&["cascade.pas"]);
    let undeclared: Vec<_> = diagnostics
        .iter()
        .filter(|d| d["code"] == "F5000")
        .collect();
    assert_eq!(
        undeclared.len(),
        1,
        "expected one root cause, got {}: {diagnostics:#?}",
        undeclared.len()
    );
    assert_eq!(undeclared[0]["primary"]["line_start"], 4);
}

#[test]
fn every_real_compiler_message_matches_its_catalogue_entry() {
    // The fidelity check for catalog/fpc.toml: compile a corpus of genuinely
    // broken programs and require that each emitted compiler message is both
    // given a real numeric code and matched by that code's template.
    let fpc = require_fpc!();
    use fpcheck::catalog::Catalog;

    let corpus: &[(&str, &str)] = &[
        ("undeclared", "begin\n  Writeln(nope);\nend.\n"),
        (
            "illegal_qualifier",
            "var i: Integer;\nbegin\n  i := i.field;\nend.\n",
        ),
        ("illegal_expr", "var i: Integer;\nbegin\n  i := ;\nend.\n"),
        (
            "missing_semi",
            "var i, j: Integer;\nbegin\n  i := 1\n  j := 2;\nend.\n",
        ),
        (
            "typemismatch",
            "var i: Integer;\nbegin\n  i := 'hello';\nend.\n",
        ),
        (
            "duplicate",
            "var x: Integer;\n    x: Integer;\nbegin\nend.\n",
        ),
        (
            "result_not_set",
            "function f: Integer;\nbegin\nend;\nbegin\n  Writeln(f);\nend.\n",
        ),
        ("uninit", "var x: Integer;\nbegin\n  Writeln(x);\nend.\n"),
        (
            "unused_local",
            "procedure q;\nvar unusedvar: Integer;\nbegin\nend;\nbegin\n  q;\nend.\n",
        ),
        (
            "wrong_params",
            "procedure q(a: Integer); begin end;\nbegin\n  q(1, 2);\nend.\n",
        ),
        ("no_type", "var a: Integer\n    b: Integer;\nbegin\nend.\n"),
        ("call_nonproc", "var i: Integer;\nbegin\n  i(3);\nend.\n"),
    ];
    let unexpected_eof = "begin\n  Writeln(1);\n";
    let include_missing = "{$I nosuchinclude.inc}\nbegin\nend.\n";

    let mut sources: Vec<(String, String)> = corpus
        .iter()
        .map(|(name, body)| {
            (
                format!("{name}.pas"),
                format!("{{$mode objfpc}}{{$H+}}\nprogram {name};\n{body}"),
            )
        })
        .collect();
    sources.push((
        "unexpected_eof.pas".into(),
        format!("{{$mode objfpc}}{{$H+}}\nprogram unexpected_eof;\n{unexpected_eof}"),
    ));
    sources.push((
        "include_missing.pas".into(),
        format!("{{$mode objfpc}}{{$H+}}\nprogram include_missing;\n{include_missing}"),
    ));

    let files: Vec<(&str, &str)> = sources
        .iter()
        .map(|(n, s)| (n.as_str(), s.as_str()))
        .collect();
    let project = Project::new(fpc, &files);

    let catalog = Catalog::builtin();
    let mut checked = 0usize;
    let mut unmatched: Vec<String> = Vec::new();

    for (name, _) in &sources {
        let diagnostics = project.json_diagnostics(&[name.as_str()]);
        for diagnostic in &diagnostics {
            if diagnostic["source"] != "fpc" {
                continue;
            }
            let code = diagnostic["code"].as_str().unwrap_or_default();
            assert!(
                code.strip_prefix('F').is_some_and(
                    |digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
                ),
                "{name}: compiler diagnostic without a numeric code: {diagnostic:#?}"
            );
            let raw = diagnostic["fpc_raw"].as_str().unwrap_or_default();
            let Some(text) = message_text(raw) else {
                continue;
            };
            checked += 1;
            if catalog.describe(code, &text).is_none() {
                unmatched.push(format!("{code} :: {text}"));
            }
        }
    }

    assert!(
        checked >= 10,
        "expected to check real messages, only saw {checked}"
    );
    assert!(
        unmatched.is_empty(),
        "{} of {checked} real compiler messages did not match their catalogue entry:\n{}",
        unmatched.len(),
        unmatched.join("\n")
    );
}

/// Pull the message text out of a raw FPC line.
///
/// With `-vq` the shape is `<loc> <Severity>: (<id>) <text>`, so the text is
/// everything after the first `: (` ... `)`. Splitting on the *first* `: (`
/// matters: the location itself contains parentheses, and so does the id.
fn message_text(raw: &str) -> Option<String> {
    let (_, tail) = raw.split_once(": (")?;
    let (_, text) = tail.split_once(')')?;
    Some(text.trim().to_string())
}

#[test]
fn explain_works_for_a_real_message_number() {
    let fpc = require_fpc!();
    let project = Project::new(fpc, &[("clean.pas", CLEAN_PROGRAM)]);
    let (code, out) = project.run(&["explain", "F5000"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Identifier not found \"{p1}\""), "{out}");
    assert!(out.contains("undeclared identifier"), "{out}");
}

#[test]
fn allow_flag_silences_a_real_compiler_code() {
    let fpc = require_fpc!();
    let project = Project::new(
        fpc,
        &[(
            "warn.pas",
            "{$mode objfpc}{$H+}\nprogram warn;\nvar x: Integer;\nbegin\n  Writeln(x);\nend.\n",
        )],
    );
    // 5037 ("Variable ... does not seem to be initialized") is a warning, so the
    // build passes; silencing it must remove it from the output.
    let (_, before) = project.run(&["check", "--message-format=llm", "warn.pas"]);
    assert!(before.contains("F5037"), "{before}");

    let (code, after) =
        project.run(&["check", "--message-format=llm", "--allow=F5037", "warn.pas"]);
    assert_eq!(code, 0, "{after}");
    assert!(!after.contains("F5037"), "{after}");
}

#[test]
fn deny_warnings_fails_a_real_warning() {
    let fpc = require_fpc!();
    let project = Project::new(
        fpc,
        &[(
            "warn.pas",
            "{$mode objfpc}{$H+}\nprogram warn;\nvar x: Integer;\nbegin\n  Writeln(x);\nend.\n",
        )],
    );
    let (code, _) = project.run(&["check", "warn.pas"]);
    assert_eq!(code, 0, "a warning alone must not fail");

    let (code, _) = project.run(&["check", "--deny-warnings", "warn.pas"]);
    assert_eq!(code, 1, "--deny-warnings must fail the build");
}

#[test]
fn analyzer_and_compiler_findings_arrive_in_one_round_trip() {
    // The spec's whole point: analyzer lints and compiler errors together.
    let fpc = require_fpc!();
    let project = Project::new(
        fpc,
        &[(
            "both.pas",
            "{$mode objfpc}{$H+}\nprogram both;\nbegin\n  with Foo do Bar;\n  Writeln(nope);\nend.\n",
        )],
    );
    let (code, out) = project.run(&["check", "--message-format=json", "both.pas"]);
    assert_eq!(code, 1);
    assert!(out.contains("P0702"), "analyzer lint missing:\n{out}");
    assert!(out.contains("F5000"), "compiler error missing:\n{out}");
}

#[test]
fn an_unsafe_region_makes_the_compiler_findings_disappear() {
    // End-to-end proof that `fpcheck fix` plus a re-check converges: removing
    // the directive that P0304 reports leaves a clean compile.
    let fpc = require_fpc!();
    let project = Project::new(
        fpc,
        &[(
            "directive.pas",
            "{$mode objfpc}{$H+}\n{$R-}\nprogram directive;\nvar i: Integer;\nbegin\n  i := 1;\n  Writeln(i);\nend.\n",
        )],
    );
    let (code, out) = project.run(&["check", "--message-format=llm", "directive.pas"]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("P0304"), "{out}");

    let (_, fixed) = project.run(&["fix", "directive.pas"]);
    assert!(fixed.contains("applied"), "{fixed}");

    let (code, after) = project.run(&["check", "directive.pas"]);
    assert_eq!(code, 0, "after the fix the project must be clean:\n{after}");
}
