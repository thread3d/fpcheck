//! Invoking `fpc` and capturing its output.
//!
//! FPC writes diagnostics to stderr and progress to stdout (and has changed
//! which over time). Suppressing cascades depends on message *order*, so the
//! two streams are merged at the OS level into one file rather than captured
//! separately and concatenated. If no temporary file can be created the runner
//! falls back to separate pipes, which loses interleaving but still works.

use crate::error::{Error, Result};
use crate::fpc::profile::{FpcMode, FpcProfile};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct FpcOutput {
    /// The exact command line, program first.
    pub argv: Vec<String>,
    /// Process exit status, if it exited normally.
    pub status: Option<i32>,
    pub success: bool,
    /// Merged stdout + stderr, in emission order where possible.
    pub output: String,
}

pub fn run_fpc(
    profile: &FpcProfile,
    source: &Path,
    mode: FpcMode,
    extras: &[String],
    cwd: &Path,
) -> Result<FpcOutput> {
    let args = profile.argv(source, mode, extras);
    let mut cmd = Command::new(&profile.program);
    cmd.args(&args).current_dir(cwd);

    let (status, output) = match run_merged(&mut cmd) {
        Ok(v) => v,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(Error::FpcNotFound(format!(
                "`{}` was not found; install Free Pascal or set `[fpc] binary` in fpcheck.toml",
                profile.program
            )));
        }
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            return Err(Error::FpcNotFound(format!(
                "`{}` is not executable",
                profile.program
            )));
        }
        Err(e) => return Err(Error::io(profile.program.clone(), e)),
    };

    let mut argv = vec![profile.program.clone()];
    argv.extend(args);

    Ok(FpcOutput { argv, status, success: status == Some(0), output })
}

/// Run, capturing both streams into one file so their order is preserved.
fn run_merged(cmd: &mut Command) -> std::io::Result<(Option<i32>, String)> {
    let tmp = unique_temp_path();
    match std::fs::File::create(&tmp) {
        Ok(file) => {
            let clone = file.try_clone()?;
            let status = cmd
                .stdout(Stdio::from(file))
                .stderr(Stdio::from(clone))
                .status()?;
            let output = std::fs::read_to_string(&tmp).unwrap_or_default();
            let _ = std::fs::remove_file(&tmp);
            Ok((status.code(), output))
        }
        Err(_) => {
            // No writable temporary directory: fall back to separate pipes.
            let out = cmd.output()?;
            let mut merged = String::from_utf8_lossy(&out.stdout).into_owned();
            merged.push_str(&String::from_utf8_lossy(&out.stderr));
            Ok((out.status.code(), merged))
        }
    }
}

fn unique_temp_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("fpcheck-{}-{}.log", std::process::id(), nanos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ProfileName};

    #[cfg(unix)]
    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn merges_stdout_and_stderr_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "fakefpc",
            "#!/bin/sh\necho 'Free Pascal Compiler version 3.2.2'\necho 'hello.pas(5,11) Error: Identifier not found \"y\"' >&2\necho 'Fatal: Compilation aborted'\nexit 1\n",
        );
        let cfg = Config::defaults();
        let mut profile = crate::fpc::profile::build_profile(&cfg, ProfileName::Dev);
        profile.program = script.display().to_string();

        let out = run_fpc(&profile, Path::new("hello.pas"), FpcMode::CheckOnly, &[], dir.path()).unwrap();
        assert!(!out.success);
        assert_eq!(out.status, Some(1));
        let banner = out.output.find("Free Pascal Compiler").expect("banner");
        let error = out.output.find("Identifier not found").expect("error");
        let fatal = out.output.find("Fatal: Compilation aborted").expect("fatal");
        assert!(banner < error && error < fatal, "order lost:\n{}", out.output);
    }

    #[cfg(unix)]
    #[test]
    fn argv_records_the_program_first() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(dir.path(), "fakefpc", "#!/bin/sh\nexit 0\n");
        let cfg = Config::defaults();
        let mut profile = crate::fpc::profile::build_profile(&cfg, ProfileName::Dev);
        profile.program = script.display().to_string();
        let out = run_fpc(&profile, Path::new("a.pas"), FpcMode::Build, &[], dir.path()).unwrap();
        assert_eq!(out.argv[0], profile.program);
        assert_eq!(out.argv.last().unwrap(), "a.pas");
        assert!(out.success);
    }

    #[cfg(unix)]
    #[test]
    fn the_source_path_is_passed_through_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(dir.path(), "fakefpc", "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");
        let cfg = Config::defaults();
        let mut profile = crate::fpc::profile::build_profile(&cfg, ProfileName::Dev);
        profile.program = script.display().to_string();
        let out = run_fpc(&profile, Path::new("src/weird name.pas"), FpcMode::Build, &[], dir.path())
            .unwrap();
        assert!(out.output.contains("src/weird name.pas"), "{}", out.output);
    }

    #[test]
    fn missing_compiler_is_a_clear_error_not_a_panic() {
        let cfg = Config::defaults();
        let mut profile = crate::fpc::profile::build_profile(&cfg, ProfileName::Dev);
        profile.program = "fpcheck-no-such-compiler-binary".to_string();
        let err = run_fpc(&profile, Path::new("a.pas"), FpcMode::Build, &[], Path::new("."))
            .unwrap_err();
        assert!(matches!(err, Error::FpcNotFound(_)), "{err:?}");
        assert!(format!("{err}").contains("not found"), "{err}");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn temp_paths_are_unique() {
        let a = unique_temp_path();
        let b = unique_temp_path();
        assert_ne!(a, b);
    }
}
