//! Typed `cargo metadata`.
//!
//! Cargo's JSON document is deserialized at this boundary so the rest of the
//! CLI operates on named fields instead of string-indexed values. Only the
//! subset required by project discovery and link crate construction is modeled;
//! Serde ignores the other fields that Cargo reports.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Typed subset of a Cargo metadata document used by bimetal.
#[derive(Deserialize)]
pub(crate) struct Metadata {
    /// Packages included in the metadata query.
    pub(crate) packages: Vec<Package>,
    /// Package IDs belonging to the active workspace.
    pub(crate) workspace_members: Vec<String>,
    /// Canonical Cargo workspace root.
    pub(crate) workspace_root: PathBuf,
    /// Effective Cargo target directory.
    pub(crate) target_directory: PathBuf,
    /// Resolved dependency graph. Cargo omits it only for `--no-deps`, which
    /// Bimetal never passes.
    pub(crate) resolve: Resolve,
}

/// Cargo package fields required by bimetal.
#[derive(Deserialize)]
pub(crate) struct Package {
    /// Opaque Cargo package ID.
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    /// The manifest as Cargo reached it, which keeps any symlink on the way.
    pub(crate) manifest_path: PathBuf,
    /// Registry/source identity, or `None` for a local package.
    pub(crate) source: Option<String>,
    /// Compilation targets declared by the package.
    pub(crate) targets: Vec<Target>,
}

/// Cargo target fields used to identify linkable libraries.
#[derive(Deserialize)]
pub(crate) struct Target {
    /// Cargo target kinds such as `lib` or `bin`.
    pub(crate) kind: Vec<String>,
    /// rustc crate types emitted by the target.
    pub(crate) crate_types: Vec<String>,
}

/// Resolved Cargo dependency graph.
#[derive(Deserialize)]
pub(crate) struct Resolve {
    /// One dependency node per resolved package.
    pub(crate) nodes: Vec<Node>,
}

/// Dependencies resolved for one Cargo package.
#[derive(Deserialize)]
pub(crate) struct Node {
    /// Opaque Cargo package ID for this node.
    pub(crate) id: String,
    /// Named outgoing dependency edges.
    #[serde(rename = "deps")]
    pub(crate) dependencies: Vec<Dependency>,
}

/// One named edge in Cargo's resolved dependency graph.
#[derive(Deserialize)]
pub(crate) struct Dependency {
    /// Opaque package ID of the dependency target.
    #[serde(rename = "pkg")]
    pub(crate) package: String,
    /// Build contexts in which this dependency applies.
    #[serde(rename = "dep_kinds")]
    pub(crate) kinds: Vec<DependencyKind>,
}

/// Build context for one Cargo dependency edge.
#[derive(Deserialize)]
pub(crate) struct DependencyKind {
    /// `None` for a normal dependency; otherwise `dev` or `build`.
    pub(crate) kind: Option<String>,
}

/// Whether Cargo may update `Cargo.lock` and use the network, as its
/// `--locked`, `--offline`, and `--frozen` flags decide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resolution {
    /// Fail rather than update `Cargo.lock`.
    pub(crate) locked: bool,
    /// Resolve and build without the network.
    pub(crate) offline: bool,
}

impl Resolution {
    /// Return the Cargo flags that ask for this resolution.
    pub(crate) fn flags(self) -> impl Iterator<Item = &'static str> {
        let locked = self.locked.then_some("--locked");
        locked
            .into_iter()
            .chain(self.offline.then_some("--offline"))
    }
}

/// Read Cargo metadata, with the resolved dependency graph, for one manifest.
///
/// Cargo runs in `directory`, which decides the `.cargo/config.toml` files
/// that it reads, so every Cargo command of one application sees the same
/// configuration wherever the `bimetal` command started.
///
/// # Errors
///
/// Returns an error when Cargo cannot run, rejects the project, or emits a
/// metadata document that does not satisfy the modeled schema.
pub(crate) fn metadata(
    manifest: &Path,
    directory: &Path,
    resolution: Resolution,
) -> Result<Metadata> {
    let output = Command::new(super::executable())
        .args(["metadata", "--format-version", "1"])
        .args(resolution.flags())
        .arg("--manifest-path")
        .arg(manifest)
        .current_dir(directory)
        .output()
        .context("failed to run cargo metadata")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(problem) = lockfile_problem(&stderr) {
            bail!(
                "{problem}: run `bimetal build`, or `cargo update`, and commit Cargo.lock with the sources"
            );
        }
        bail!("cargo metadata failed\n{stderr}");
    }
    serde_json::from_slice(&output.stdout).context("Cargo returned invalid metadata")
}

/// Return what is wrong with the lockfile when Cargo refused to write it
/// because of `--locked`, which it reports as `cannot create the lock file
/// <path> because --locked was passed`, or `cannot update ...`.
#[must_use]
fn lockfile_problem(stderr: &str) -> Option<String> {
    let line = stderr
        .lines()
        .find(|line| line.contains("--locked was passed"))?;
    let path = line
        .split_once("lock file ")
        .and_then(|(_, rest)| rest.split_once(" because"))
        .map_or("Cargo.lock", |(path, _)| path);
    Some(if line.contains("cannot create") {
        format!("{path} is missing")
    } else {
        format!("{path} is out of date with Cargo.toml")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_failures_say_what_is_wrong_with_the_lockfile() {
        assert_eq!(
            lockfile_problem(
                "error: cannot create the lock file /app/Cargo.lock because --locked was passed to prevent this\nhelp: ..."
            )
            .as_deref(),
            Some("/app/Cargo.lock is missing")
        );
        assert_eq!(
            lockfile_problem(
                "error: cannot update the lock file /app/Cargo.lock because --locked was passed to prevent this"
            )
            .as_deref(),
            Some("/app/Cargo.lock is out of date with Cargo.toml")
        );
        assert_eq!(lockfile_problem("error: failed to parse manifest"), None);
    }
}
