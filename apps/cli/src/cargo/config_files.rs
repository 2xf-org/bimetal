//! The Cargo configuration files that a Cargo command reads, in Cargo's
//! order of precedence.
//!
//! Cargo reads `.cargo/config.toml`, or the older `.cargo/config`, in the
//! directory it runs in and in each ancestor, and then the one in Cargo's
//! home. A deeper file takes precedence over the files above it.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml::{Table, Value};

use crate::paths;

/// The configuration files of one directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConfigFiles {
    /// The parsed files, from the lowest precedence to the highest.
    files: Vec<ConfigFile>,
    /// Every file that Cargo reads, the deepest first.
    paths: Vec<PathBuf>,
}

/// One parsed configuration file.
#[derive(Debug, Clone)]
struct ConfigFile {
    table: Table,
    /// The directory that the file's relative paths start from: the one that
    /// holds its `.cargo` directory, as Cargo resolves them.
    root: PathBuf,
}

impl ConfigFiles {
    /// Find and parse the files that Cargo reads when it runs in `directory`.
    ///
    /// # Errors
    ///
    /// Returns an error when a file cannot be read or is not TOML.
    pub(crate) fn discover(directory: &Path, cargo_home: Option<&Path>) -> Result<Self> {
        let mut paths = Vec::new();
        let mut read_home = false;
        for ancestor in directory.ancestors() {
            let configuration = ancestor.join(".cargo");
            read_home |= cargo_home.is_some_and(|home| same_directory(home, &configuration));
            paths.extend(file_in(&configuration));
        }
        if !read_home && let Some(path) = cargo_home.and_then(file_in) {
            paths.push(path);
        }
        let mut files = Vec::new();
        for path in paths.iter().rev() {
            let source = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let table: Table = toml::from_str(&source)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            let root = path
                .parent()
                .and_then(Path::parent)
                .with_context(|| format!("{} is not in a directory", path.display()))?
                .to_path_buf();
            files.push(ConfigFile { table, root });
        }
        Ok(Self { files, paths })
    }

    /// Return every file that Cargo reads, the one in Cargo's home included,
    /// the deepest first.
    #[must_use]
    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Return the path at a dotted key, such as `build.dep-info-basedir`,
    /// from the file that takes precedence. A relative path starts from the
    /// directory that holds the file's `.cargo` directory, as in Cargo.
    #[must_use]
    pub(crate) fn path(&self, key: &[&str]) -> Option<PathBuf> {
        self.files
            .iter()
            .rev()
            .find_map(|file| Some(file.root.join(value_at(&file.table, key)?.as_str()?)))
    }

    /// Return whether a file sets the dotted key, such as `build.jobs`.
    #[must_use]
    pub(crate) fn sets(&self, key: &[&str]) -> bool {
        self.files
            .iter()
            .any(|file| value_at(&file.table, key).is_some())
    }

    /// Return the program that Cargo, running in `directory`, runs as one of
    /// its tools, such as `rustc` or `rustc-wrapper`: the one that the tool's
    /// variable names, such as `RUSTC`, or else the `build` setting, such as
    /// `build.rustc`, from the environment or the file that takes precedence.
    /// As in Cargo, a path with a separator is relative to `directory`, or to
    /// the directory that holds the file's `.cargo` directory, and a bare
    /// name is looked up on `PATH`.
    #[must_use]
    pub(crate) fn tool(&self, directory: &Path, tool: &str) -> Option<PathBuf> {
        let variable = tool.to_uppercase().replace('-', "_");
        let from_environment = [variable.clone(), format!("CARGO_BUILD_{variable}")]
            .into_iter()
            .find_map(env::var_os);
        if let Some(value) = from_environment {
            return Some(program(directory, value));
        }
        self.files.iter().rev().find_map(|file| {
            let value = value_at(&file.table, &["build", tool])?.as_str()?;
            Some(program(&file.root, value.into()))
        })
    }
}

/// Return one file's value at a dotted key.
#[must_use]
fn value_at<'table>(table: &'table Table, key: &[&str]) -> Option<&'table Value> {
    let (last, parents) = key.split_last()?;
    let mut current = table;
    for segment in parents {
        current = current.get(*segment)?.as_table()?;
    }
    current.get(*last)
}

/// Return the program that `value` names: a path with a separator relative
/// to `root`, or a bare name.
#[must_use]
fn program(root: &Path, value: OsString) -> PathBuf {
    let path = PathBuf::from(value);
    if path.components().count() > 1 {
        root.join(path)
    } else {
        path
    }
}

/// Return the configuration file in a `.cargo` directory: `config`, which
/// Cargo prefers, or `config.toml`.
fn file_in(directory: &Path) -> Option<PathBuf> {
    ["config", "config.toml"]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())
}

/// Return whether two paths name the same existing directory.
#[must_use]
fn same_directory(first: &Path, second: &Path) -> bool {
    match (paths::canonical(first), paths::canonical(second)) {
        (Ok(first), Ok(second)) => first == second,
        _ => first == second,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_reads_ancestors_then_the_cargo_home() {
        let root = tempfile::tempdir().expect("temporary directory");
        let root = paths::canonical(root.path()).expect("canonical directory");
        let project = root.join("workspace").join("app");
        let home = root.join("home");
        for (directory, name, wrapper) in [
            (root.join("workspace"), "config.toml", "workspace"),
            (project.clone(), "config.toml", "ignored"),
            (project.clone(), "config", "bin/app"),
            (home.clone(), "config.toml", "home"),
        ] {
            let directory = if directory == home {
                directory
            } else {
                directory.join(".cargo")
            };
            fs::create_dir_all(&directory).expect("configuration directory");
            fs::write(
                directory.join(name),
                format!("[build]\nbimetal-test-tool = \"{wrapper}\"\ntarget-dir = \"{wrapper}\"\n"),
            )
            .expect("configuration file");
        }

        let config = ConfigFiles::discover(&project, Some(&home)).expect("discover");

        // A deeper file takes precedence, and a path starts beside its
        // `.cargo` directory.
        assert_eq!(
            config.tool(&root, "bimetal-test-tool"),
            Some(project.join("bin/app"))
        );
        assert_eq!(
            config.path(&["build", "target-dir"]),
            Some(project.join("bin/app"))
        );
        assert_eq!(config.path(&["build", "dep-info-basedir"]), None);
        let app = project.join(".cargo").join("config");
        let workspace = root.join("workspace").join(".cargo").join("config.toml");
        assert_eq!(
            config.paths(),
            [app.clone(), workspace.clone(), home.join("config.toml")]
        );

        let config = ConfigFiles::discover(&project, Some(&root.join("workspace").join(".cargo")))
            .expect("discover with the home among the ancestors");
        assert_eq!(config.paths(), [app, workspace]);
    }

    #[test]
    fn programs_are_paths_only_with_a_separator() {
        let root = Path::new("/app");

        assert_eq!(program(root, "sccache".into()), Path::new("sccache"));
        assert_eq!(
            program(root, "tools/wrap".into()),
            Path::new("/app/tools/wrap")
        );
        assert_eq!(program(root, "/bin/wrap".into()), Path::new("/bin/wrap"));
    }
}
