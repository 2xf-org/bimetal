//! Command-line entry points for the Bimetal build orchestrator.
//!
//! The crate is organized as a small compiler pipeline:
//!
//! - `config` and `cargo` read the user's settings and the workspace;
//! - `project` builds the synthetic link crate and validates the discovered IR;
//! - `contract` and `documentation` index that IR and its Rustdoc for the
//!   generators, and `namespaces` says which namespace each declaration
//!   goes in;
//! - `python` and `typescript` lower the IR into host packages, quoting
//!   strings with `literal`;
//! - `output` owns collision checks and publication, and `paths` writes
//!   paths the way `bimetal.toml` records them;
//! - `scaffold` writes the user-owned files of a new application;
//! - `cli` and `commands` provide the process-facing interface.
//!
//! This module owns the process boundary, where the executable is either the
//! `bimetal` command or, inside Bimetal's builds, Cargo's rustc wrapper.

#![deny(missing_docs, rustdoc::broken_intra_doc_links)]
#![forbid(unsafe_op_in_unsafe_fn)]

use std::io;

use anyhow::Result;
use clap::Parser;

mod cargo;
mod cli;
mod commands;
mod config;
mod contract;
mod dependencies;
mod documentation;
#[cfg(test)]
mod fixtures;
mod literal;
mod namespaces;
mod output;
mod paths;
mod problems;
mod project;
mod python;
mod scaffold;
mod typescript;

use cli::Cli;

/// Parse the current process arguments and run the selected command.
///
/// The result is written to stdout, as text or with `--json` as JSON; with
/// `--json`, a failure is written there too. Progress goes to stderr, and
/// fatal errors are returned to the binary entry point.
///
/// # Errors
///
/// Returns an error when command input is invalid or a requested operation
/// fails.
pub fn run() -> Result<()> {
    let stdout = io::stdout();
    commands::run(Cli::parse(), &mut stdout.lock())
}

/// Run rustc, and return its exit status, when one of Bimetal's builds started
/// this process as Cargo's `RUSTC_WRAPPER`; otherwise return `None`.
#[must_use]
pub fn run_as_rustc_wrapper() -> Option<i32> {
    cargo::rustc_wrapper::run()
}
