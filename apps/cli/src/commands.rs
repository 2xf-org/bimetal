//! Command execution and user-visible reporting.
//!
//! This module is the orchestration boundary between parsed arguments and the
//! build pipeline. A command prints one result: readable text by default, or
//! JSON with `--json`, which reports a failure as JSON too.

use std::env;
use std::fmt;
use std::io::Write;
use std::path::Path;

use anyhow::{Error, Result};
use serde::Serialize;

use crate::cargo::Resolution;
use crate::cli::{Cli, Command, ProjectArgs};
use crate::config::{Config, Host};
use crate::output::OutOfSync;
use crate::project::{self, BuildReport, BuildStatus, CheckReport, HostReport, Project};
use crate::scaffold::{self, NewReport};

/// Run one parsed command, and with `--json` also print a failure on
/// `stdout` as `{"status": "error", "error": "..."}`. The error is returned
/// either way, so the process reports it on stderr and exits with status 1.
pub(crate) fn run(cli: Cli, stdout: &mut dyn Write) -> Result<()> {
    let json = cli.json;
    let result = execute(cli, stdout);
    if json && let Err(error) = &result {
        write_json(stdout, &ErrorReport::new(error))?;
    }
    result
}

/// Execute one parsed command using an explicit output stream.
fn execute(cli: Cli, stdout: &mut dyn Write) -> Result<()> {
    let json = cli.json;
    match cli.command {
        Command::New(args) => report(stdout, json, &scaffold::create(&args)?),
        Command::Build(args) => {
            let application = read_project(&args.project, resolution(&args.project))?;
            report(stdout, json, &project::build(&application, args.force)?)
        }
        Command::Check(args) => {
            // A check writes nothing, Cargo.lock included.
            let resolution = Resolution {
                locked: true,
                ..resolution(&args.project)
            };
            let application = read_project(&args.project, resolution)?;
            report(stdout, json, &project::check(&application, args.rebuild)?)
        }
    }
}

/// The result of a command that failed.
#[derive(Debug, Serialize)]
struct ErrorReport<'error> {
    status: &'static str,
    /// The error and its causes, as the process prints them on stderr.
    error: String,
    /// The generated files that a failed check found out of sync.
    #[serde(flatten)]
    out_of_sync: Option<&'error OutOfSync>,
}

impl<'error> ErrorReport<'error> {
    #[must_use]
    fn new(error: &'error Error) -> Self {
        Self {
            status: "error",
            error: format!("{error:#}"),
            out_of_sync: error.downcast_ref(),
        }
    }
}

impl fmt::Display for NewReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let directory = display_path(&self.directory);
        writeln!(formatter, "Created {} in {directory}:", self.application)?;
        for file in &self.files {
            writeln!(formatter, "  {}", file.display())?;
        }
        write!(
            formatter,
            "Next, run `bimetal build` in {directory} to generate the packages."
        )
    }
}

impl fmt::Display for BuildReport<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            BuildStatus::Unchanged => write!(
                formatter,
                "{} {} is up to date.",
                self.application, self.version
            ),
            BuildStatus::Ok => {
                write!(formatter, "Built {} {}.", self.application, self.version)?;
                write_hosts(formatter, &self.hosts)
            }
        }
    }
}

impl fmt::Display for CheckReport<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "The generated files of {} {} match the Rust sources.",
            self.application, self.version
        )?;
        write_hosts(formatter, &self.hosts)
    }
}

/// Write one line for each host's package.
fn write_hosts(formatter: &mut fmt::Formatter<'_>, hosts: &[HostReport<'_>]) -> fmt::Result {
    for host in hosts {
        let kind = match host.host {
            Host::Python => "Python package",
            Host::TypeScript => "npm package",
        };
        write!(
            formatter,
            "\n  {kind} {} in {}",
            host.package,
            display_path(host.directory)
        )?;
    }
    Ok(())
}

/// Resolve a project from a command's configuration path.
fn read_project(args: &ProjectArgs, resolution: Resolution) -> Result<Project> {
    Project::read(&Config::find(args.config.as_deref())?, resolution)
}

/// Return how Cargo may resolve the application's dependencies, from the
/// command's `--locked`, `--offline`, and `--frozen` flags.
#[must_use]
fn resolution(args: &ProjectArgs) -> Resolution {
    Resolution {
        locked: args.locked || args.frozen,
        offline: args.offline || args.frozen,
    }
}

/// Print one result as text, or as pretty JSON with `--json`.
fn report(
    output: &mut dyn Write,
    json: bool,
    value: &(impl Serialize + fmt::Display),
) -> Result<()> {
    if json {
        return write_json(output, value);
    }
    writeln!(output, "{value}")?;
    Ok(())
}

/// Print one result as pretty JSON.
fn write_json(output: &mut dyn Write, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer_pretty(&mut *output, value)?;
    writeln!(output)?;
    Ok(())
}

/// Show a path relative to the current directory when it is inside it.
#[must_use]
fn display_path(path: &Path) -> String {
    let relative = env::current_dir()
        .ok()
        .and_then(|current| path.strip_prefix(current).ok().map(Path::to_path_buf));
    match relative {
        Some(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Some(relative) => relative.display().to_string(),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn run_with(args: &[&str]) -> (Result<()>, String) {
        let cli = Cli::try_parse_from(args).expect("parse arguments");
        let mut stdout = Vec::new();
        let result = run(cli, &mut stdout);
        (result, String::from_utf8(stdout).expect("UTF-8 output"))
    }

    #[test]
    fn json_failures_print_an_error_object() {
        let missing = tempfile::tempdir().expect("temporary directory");
        let config = missing.path().join("absent").join("bimetal.toml");
        let config = config.to_str().expect("UTF-8 path");

        let (result, stdout) = run_with(&["bimetal", "--json", "check", "--config", config]);

        let error = result.expect_err("a missing configuration fails");
        let report: serde_json::Value = serde_json::from_str(&stdout).expect("JSON on stdout");
        assert_eq!(report["status"], "error");
        assert_eq!(report["error"], format!("{error:#}"));
        assert!(
            report["error"]
                .as_str()
                .is_some_and(|message| message.contains("cannot find Bimetal configuration")),
            "{stdout}"
        );
        assert!(report.get("missing").is_none(), "{stdout}");
    }

    #[test]
    fn json_check_failures_list_the_files() {
        let error = Error::new(OutOfSync {
            missing: vec!["python/app/api.py".to_owned()],
            changed: Vec::new(),
            stale: vec!["typescript/app/old.js".to_owned()],
        })
        .context("the check failed");

        let report = serde_json::to_value(ErrorReport::new(&error)).expect("JSON report");

        assert_eq!(report["missing"], serde_json::json!(["python/app/api.py"]));
        assert_eq!(report["changed"], serde_json::json!([]));
        assert_eq!(
            report["stale"],
            serde_json::json!(["typescript/app/old.js"])
        );
        assert!(
            report["error"]
                .as_str()
                .is_some_and(|message| message.contains("\n  stale: typescript/app/old.js")),
            "{report}"
        );
    }

    #[test]
    fn reports_list_every_enabled_host() {
        let hosts = || {
            vec![
                HostReport {
                    host: Host::TypeScript,
                    package: "@acme/app",
                    directory: Path::new("/work/web"),
                    files: vec!["@acme/app/index.js".to_owned()],
                },
                HostReport {
                    host: Host::Python,
                    package: "app",
                    directory: Path::new("/work/app/python"),
                    files: vec!["app/api.py".to_owned(), "app/_native.abi3.so".to_owned()],
                },
            ]
        };
        let build = BuildReport {
            status: BuildStatus::Ok,
            application: "app",
            version: "1.0.0",
            hosts: hosts(),
        };
        let check = CheckReport {
            application: "app",
            version: "1.0.0",
            hosts: hosts(),
        };
        let expected_hosts = serde_json::json!([
            {"host": "typescript", "package": "@acme/app", "directory": "/work/web", "files": ["@acme/app/index.js"]},
            {"host": "python", "package": "app", "directory": "/work/app/python", "files": ["app/api.py", "app/_native.abi3.so"]},
        ]);

        assert_eq!(
            serde_json::to_value(&build).unwrap(),
            serde_json::json!({"status": "ok", "application": "app", "version": "1.0.0", "hosts": expected_hosts})
        );
        assert_eq!(
            serde_json::to_value(&check).unwrap(),
            serde_json::json!({"status": "ok", "application": "app", "version": "1.0.0", "hosts": expected_hosts})
        );
        let lines =
            "\n  npm package @acme/app in /work/web\n  Python package app in /work/app/python";
        assert_eq!(build.to_string(), format!("Built app 1.0.0.{lines}"));
        assert_eq!(
            check.to_string(),
            format!("The generated files of app 1.0.0 match the Rust sources.{lines}")
        );
    }

    #[test]
    fn text_failures_leave_stdout_empty() {
        let missing = tempfile::tempdir().expect("temporary directory");
        let config = missing.path().join("bimetal.toml");
        let config = config.to_str().expect("UTF-8 path");

        let (result, stdout) = run_with(&["bimetal", "build", "--config", config]);

        assert!(result.is_err());
        assert_eq!(stdout, "");
    }
}
