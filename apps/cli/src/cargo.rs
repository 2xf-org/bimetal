//! Cargo and rustc: their subprocesses, configuration, and output files.
//!
//! - `metadata` reads `cargo metadata` into typed fields;
//! - `config_files` reads the `.cargo/config.toml` files that a build sees;
//! - `remap` names local directories portably in artifacts, and back;
//! - `rustc_wrapper` adds those names to the compilations of a build;
//! - `toolchain` asks rustc for its host, sysroot, and commit;
//! - `messages` reads what a build reports, and `dep_info` the files that
//!   list what it read.

mod config_files;
pub(crate) mod dep_info;
pub(crate) mod messages;
mod metadata;
mod remap;
pub(crate) mod rustc_wrapper;
mod toolchain;

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) use config_files::ConfigFiles;
#[cfg(test)]
pub(crate) use metadata::{Dependency, DependencyKind, Node, Resolve, Target};
pub(crate) use metadata::{Metadata, Package, Resolution, metadata};
pub(crate) use remap::PathRemaps;
pub(crate) use toolchain::Toolchain;

/// The environment variable that makes pyo3 build a Python extension module,
/// which leaves libpython's symbols to the interpreter that loads it.
pub(crate) const PYTHON_EXTENSION_VARIABLE: &str = "PYO3_BUILD_EXTENSION_MODULE";

/// Return Cargo's executable, honoring Cargo's subprocess environment.
#[must_use]
pub(crate) fn executable() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

/// Return Cargo's home directory: `CARGO_HOME`, which Cargo running in
/// `directory` reads relative to it, or `.cargo` in the user's home.
#[must_use]
pub(crate) fn home(directory: &Path) -> Option<PathBuf> {
    env::var_os("CARGO_HOME")
        .map(|home| directory.join(home))
        .or_else(|| env::home_dir().map(|home| home.join(".cargo")))
}
