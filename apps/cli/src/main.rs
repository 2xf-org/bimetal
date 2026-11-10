//! Process entry point for the `bimetal` executable.
//!
//! The library owns parsing and behavior. The binary owns only terminal error
//! presentation and the exit status.

use std::process;

fn main() {
    // Bimetal's builds run this executable as Cargo's rustc wrapper.
    if let Some(status) = bimetal_cli::run_as_rustc_wrapper() {
        process::exit(status);
    }
    if let Err(error) = bimetal_cli::run() {
        eprintln!("error: {error:#}");
        process::exit(1);
    }
}
