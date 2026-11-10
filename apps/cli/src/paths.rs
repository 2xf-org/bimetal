//! Paths as `bimetal.toml` and the records in `.bimetal/` write them: relative,
//! with `/` between components, and climbing with `..` where they leave the
//! directory they are relative to; and the canonical paths that Bimetal
//! compares them by.

use std::io;
use std::path::{Component, MAIN_SEPARATOR_STR, Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Return the canonical path of `path`, as [`Path::canonicalize`] does, but
/// in the form that Cargo reports and people write: on Windows without the
/// verbatim prefix, so `\\?\C:\app` is `C:\app`.
pub(crate) fn canonical(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize().map(without_verbatim_prefix)
}

/// Remove the verbatim prefix of a Windows path: `\\?\C:\app` becomes
/// `C:\app`, and `\\?\UNC\server\share` becomes `\\server\share`. Other
/// paths, such as every Unix path, stay as they are.
#[must_use]
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(drive) if drive.as_bytes().get(1) == Some(&b':') => PathBuf::from(drive),
        _ => path,
    }
}

/// Return `path` relative to `base`, climbing out of `base` with `..` where
/// `path` is outside it.
#[must_use]
pub(crate) fn relative(base: &Path, path: &Path) -> PathBuf {
    let base: Vec<Component<'_>> = base.components().collect();
    let path: Vec<Component<'_>> = path.components().collect();
    let shared = base
        .iter()
        .zip(&path)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in shared..base.len() {
        relative.push(Component::ParentDir);
    }
    for component in &path[shared..] {
        relative.push(component);
    }
    relative
}

/// Report whether `path` can be written relative to `base`: on Windows, not
/// when the two are on different drives.
#[must_use]
pub(crate) fn share_root(base: &Path, path: &Path) -> bool {
    let is_root =
        |component: &Component<'_>| matches!(component, Component::Prefix(_) | Component::RootDir);
    base.components()
        .take_while(is_root)
        .eq(path.components().take_while(is_root))
}

/// Write `path` relative to `base`, as [`portable`] does.
///
/// # Errors
///
/// Returns an error when the path is not UTF-8.
pub(crate) fn portable_relative(base: &Path, path: &Path) -> Result<String> {
    portable(&relative(base, path))
}

/// Resolve `.` and `..` components without touching the file system, as Cargo
/// does for dependency paths.
#[must_use]
pub(crate) fn normalized(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component),
        }
    }
    normalized
}

/// Write a relative path with `/` separators.
///
/// # Errors
///
/// Returns an error when the path is absolute or is not UTF-8, which no
/// record can hold.
pub(crate) fn portable(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => parts.push(".."),
            Component::Normal(part) => parts.push(
                part.to_str()
                    .with_context(|| format!("{} is not valid UTF-8", path.display()))?,
            ),
            _ => bail!("{} is not a relative path", path.display()),
        }
    }
    Ok(parts.join("/"))
}

/// Read a relative path that `bimetal.toml` records.
///
/// # Errors
///
/// Returns an error when the path is empty or absolute.
pub(crate) fn parse(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value.replace('/', MAIN_SEPARATOR_STR));
    let relative = path
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::ParentDir));
    if value.is_empty() || !relative {
        bail!(
            "invalid path in bimetal.toml: `{value}`; delete the [generated] tables and run `bimetal build`"
        );
    }
    Ok(path)
}

/// Return the directory of a host's project, which `value` names relative
/// to `root`, the canonical directory of `bimetal.toml`.
///
/// # Errors
///
/// Returns an error when `value` is not a relative path, or when the
/// directory holds `root` or lies inside `target`, Cargo's target directory.
pub(crate) fn project(root: &Path, target: &Path, value: &str) -> Result<PathBuf> {
    let target = canonical(target).unwrap_or_else(|_| target.to_path_buf());
    let relative = Path::new(value);
    let is_relative = relative.components().all(|component| {
        matches!(
            component,
            Component::Normal(_) | Component::ParentDir | Component::CurDir
        )
    });
    if value.is_empty() || !is_relative {
        bail!("`{value}` is not a relative path");
    }
    let directory = normalized(&root.join(relative));
    if root.starts_with(&directory) {
        bail!("`{value}` holds bimetal.toml; give the package a folder of its own");
    }
    if directory.starts_with(&target) {
        bail!("`{value}` is inside Cargo's target directory");
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_climb_out_of_their_base() {
        assert_eq!(
            relative(Path::new("/a/app"), Path::new("/a/app/src/data.txt")),
            Path::new("src/data.txt")
        );
        assert_eq!(
            relative(Path::new("/a/app"), Path::new("/a/bimetal/README.md")),
            Path::new("../bimetal/README.md")
        );
    }

    #[test]
    fn only_paths_on_one_drive_share_a_root() {
        assert!(share_root(Path::new("/a/app"), Path::new("/b/lib.rs")));
        if cfg!(windows) {
            assert!(share_root(
                Path::new(r"C:\app"),
                Path::new(r"C:\bimetal\lib.rs")
            ));
            assert!(!share_root(
                Path::new(r"C:\app"),
                Path::new(r"D:\bimetal\lib.rs")
            ));
        }
    }

    #[test]
    fn canonical_paths_drop_the_verbatim_prefix() {
        for (verbatim, plain) in [
            (r"\\?\C:\work\app", r"C:\work\app"),
            (r"\\?\UNC\server\share\app", r"\\server\share\app"),
            // A verbatim path that names no drive has no plain form.
            (r"\\?\Volume{0}\app", r"\\?\Volume{0}\app"),
            ("/work/app", "/work/app"),
        ] {
            assert_eq!(
                without_verbatim_prefix(PathBuf::from(verbatim)),
                Path::new(plain)
            );
        }
        let directory = tempfile::tempdir().unwrap();
        let path = canonical(directory.path()).unwrap();
        assert!(path.is_absolute() && path.is_dir(), "{}", path.display());
        assert!(!path.to_string_lossy().starts_with(r"\\?\"));
    }

    #[test]
    fn dependency_paths_resolve_dots_as_cargo_does() {
        // Cargo resolves `..` in a dependency path without following symlinks.
        assert_eq!(
            normalized(Path::new("/repo/examples/app/../../crates/./bimetal")),
            Path::new("/repo/crates/bimetal")
        );
    }

    #[test]
    fn portable_paths_round_trip() {
        for value in ["../bimetal/README.md", "python/app/api.py"] {
            assert_eq!(portable(&parse(value).unwrap()).unwrap(), value);
        }
        assert!(portable(Path::new("/a")).is_err());
        assert_eq!(
            parse("src/./data.txt").unwrap(),
            parse("src/data.txt").unwrap()
        );
        assert!(parse("/etc/passwd").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn projects_are_folders_beside_or_above_bimetal_toml() {
        let root = Path::new("/repo/crates/app");
        let target = Path::new("/repo/target");
        let project = |value| project(root, target, value);

        assert_eq!(project("python").unwrap(), root.join("python"));
        assert_eq!(project("./python/").unwrap(), root.join("python"));
        assert_eq!(project("../../python").unwrap(), Path::new("/repo/python"));
        assert_eq!(
            project("generated/../typescript").unwrap(),
            root.join("typescript")
        );
        for (value, problem) in [
            ("", "not a relative path"),
            ("/repo/python", "not a relative path"),
            (".", "holds bimetal.toml"),
            ("src/..", "holds bimetal.toml"),
            ("..", "holds bimetal.toml"),
            ("../../..", "holds bimetal.toml"),
            ("../../target/python", "inside Cargo's target directory"),
        ] {
            let error = project(value).unwrap_err().to_string();
            assert!(error.contains(problem), "{value}: {error}");
        }
    }
}
