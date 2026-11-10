//! Reading a user-owned file, such as `pyproject.toml`, and checking the
//! version that it declares.

use std::error::Error as StdError;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

/// Read the user-owned file at `path`, such as `pyproject.toml`, and parse it
/// with `parse`.
pub(super) fn read_user_file<T, E>(
    path: &Path,
    parse: impl FnOnce(&str) -> Result<T, E>,
) -> Result<T>
where
    E: StdError + Send + Sync + 'static,
{
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read user-owned {}", path.display()))?;
    parse(&source).with_context(|| format!("failed to parse user-owned {}", path.display()))
}

/// Return what is wrong with `declared`, the version that a user-owned file
/// declares at `key`, which must be `expected`, Cargo's.
#[must_use]
pub(super) fn version_problem(declared: Option<&str>, key: &str, expected: &str) -> Option<String> {
    match declared {
        None => Some(format!("must define a string {key}")),
        Some(version) if version != expected => Some(format!(
            "declares version `{version}`, but Cargo.toml declares `{expected}`"
        )),
        Some(_) => None,
    }
}
