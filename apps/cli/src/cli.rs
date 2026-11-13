//! Command-line syntax and argument types.
//!
//! These types describe accepted input only. Execution and reporting remain in
//! `commands`, which keeps parsing independently testable and free of effects.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "bimetal",
    version,
    about = "Build one Rust API for Python, TypeScript, and C"
)]
pub(crate) struct Cli {
    /// Print the result as JSON for scripts.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Create a new application.
    New(NewArgs),
    /// Build the packages that bimetal.toml turns on.
    Build(BuildArgs),
    /// Check that the generated files match the Rust sources.
    Check(CheckArgs),
}

/// Arguments for commands that operate on an existing application.
#[derive(Debug, Args)]
pub(crate) struct ProjectArgs {
    /// The application's `bimetal.toml`, or its directory [default: the
    /// `bimetal.toml` of the current directory or its nearest parent].
    #[arg(long, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,
    /// Fail rather than update Cargo.lock.
    #[arg(long)]
    pub(crate) locked: bool,
    /// Build without the network.
    #[arg(long)]
    pub(crate) offline: bool,
    /// Both `--locked` and `--offline`.
    #[arg(long)]
    pub(crate) frozen: bool,
}

/// Arguments for `bimetal build`.
#[derive(Debug, Args)]
pub(crate) struct BuildArgs {
    #[command(flatten)]
    pub(crate) project: ProjectArgs,
    /// Compile everything even when the fingerprints match.
    #[arg(long)]
    pub(crate) force: bool,
}

/// Arguments for `bimetal check`.
#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    #[command(flatten)]
    pub(crate) project: ProjectArgs,
    /// Compile even when the fingerprints match, and compare the result with
    /// every committed generated file.
    #[arg(long)]
    pub(crate) rebuild: bool,
}

/// Arguments for `bimetal new`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("bimetal").args(["bimetal_path", "bimetal_git"])))]
#[command(group(ArgGroup::new("reference").args(["tag", "branch", "rev"])))]
pub(crate) struct NewArgs {
    /// Directory to create. Its name becomes the application name.
    pub(crate) directory: PathBuf,
    /// Depend on the Bimetal checkout in this directory, not on crates.io.
    #[arg(long, value_name = "DIRECTORY")]
    pub(crate) bimetal_path: Option<PathBuf>,
    /// Depend on Bimetal from this git repository, not on crates.io.
    #[arg(long, value_name = "URL")]
    pub(crate) bimetal_git: Option<String>,
    /// Use this git tag of Bimetal.
    #[arg(long, requires = "bimetal_git", conflicts_with = "bimetal_path")]
    pub(crate) tag: Option<String>,
    /// Use this git branch of Bimetal.
    #[arg(long, requires = "bimetal_git", conflicts_with = "bimetal_path")]
    pub(crate) branch: Option<String>,
    /// Use this git commit of Bimetal.
    #[arg(long, requires = "bimetal_git", conflicts_with = "bimetal_path")]
    pub(crate) rev: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_takes_one_bimetal_source_at_most() {
        assert!(Cli::try_parse_from(["bimetal", "new", "app"]).is_ok());
        assert!(Cli::try_parse_from(["bimetal", "new", "app", "--tag", "v1"]).is_err());
        assert!(
            Cli::try_parse_from([
                "bimetal",
                "new",
                "app",
                "--bimetal-path",
                "../bimetal",
                "--bimetal-git",
                "https://example.com/bimetal"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "bimetal",
                "new",
                "app",
                "--bimetal-path",
                "../bimetal",
                "--tag",
                "v1"
            ])
            .is_err()
        );

        let parsed = Cli::try_parse_from([
            "bimetal",
            "new",
            "app",
            "--bimetal-git",
            "https://example.com/bimetal",
            "--tag",
            "v1.0.0",
        ])
        .expect("parse new");
        let Command::New(args) = parsed.command else {
            panic!("expected new command");
        };
        assert_eq!(args.tag.as_deref(), Some("v1.0.0"));
    }
}
