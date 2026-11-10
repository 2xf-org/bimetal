//! The `.bimetal` scratch directory, the build lock inside it, and the staged
//! files that the generators and the native builds write there; and the
//! file operations that publication shares.

use std::fs::{self, File, OpenOptions, Permissions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tempfile::TempDir;

/// The scratch directory, beside `bimetal.toml`.
pub(super) const SCRATCH_DIRECTORY: &str = ".bimetal";

/// What the name of every staging directory starts with.
const STAGING_PREFIX: &str = "stage-";

/// What the name of the temporary file that [`replace`] writes starts with.
const TEMPORARY_PREFIX: &str = ".bimetal-";

/// Exclusive process lock held for the duration of a build or check.
#[must_use = "dropping the lock releases it"]
pub(crate) struct ProjectLock {
    /// The locked file, which the operating system unlocks when it closes.
    _file: File,
}

/// Create and return the single scratch directory holding all Bimetal working
/// state: the build lock, staged output, and the record of the installed
/// native artifacts. The directory ignores itself, so host repositories need
/// no Bimetal ignore rules.
pub(super) fn scratch_directory(project_root: &Path) -> Result<PathBuf> {
    let scratch = project_root.join(SCRATCH_DIRECTORY);
    fs::create_dir_all(&scratch)
        .with_context(|| format!("failed to create {}", scratch.display()))?;
    let gitignore = scratch.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, "*\n")
            .with_context(|| format!("failed to write {}", gitignore.display()))?;
    }
    Ok(scratch)
}

/// Create a staging directory in the scratch directory, which is removed
/// when the returned value drops, or by the next command if this one dies.
pub(crate) fn staging_directory(project_root: &Path) -> Result<TempDir> {
    tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(scratch_directory(project_root)?)
        .context("failed to create a staging directory")
}

/// Acquire the build lock of the application `application`, whose
/// `bimetal.toml` is in `project_root`, saying so when another command holds
/// it, and then delete the staging directories of commands that died.
pub(crate) fn project_lock(project_root: &Path, application: &str) -> Result<ProjectLock> {
    let scratch = scratch_directory(project_root)?;
    let path = scratch.join("build.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            eprintln!("Waiting for another bimetal command to finish with {application}");
            file.lock()
                .with_context(|| format!("failed to lock {}", path.display()))?;
        }
        Err(TryLockError::Error(error)) => {
            return Err(error).with_context(|| format!("failed to lock {}", path.display()));
        }
    }
    let lock = ProjectLock { _file: file };
    remove_leftovers(&scratch)?;
    Ok(lock)
}

/// Delete the staging directories of commands that died, which only the lock
/// holder uses.
fn remove_leftovers(scratch: &Path) -> Result<()> {
    let entries =
        fs::read_dir(scratch).with_context(|| format!("failed to read {}", scratch.display()))?;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(STAGING_PREFIX) && entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())
                .with_context(|| format!("failed to remove {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Create a new UTF-8 file, rejecting any destination collision.
pub(crate) fn write(path: &Path, source: &str) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("generated file collision at {}", path.display()))?;
    file.write_all(source.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Replace the file at `destination` with a copy of `staged` that has its
/// permissions; see [`replace`].
pub(crate) fn install(staged: &Path, destination: &Path) -> Result<()> {
    let contents =
        fs::read(staged).with_context(|| format!("failed to read {}", staged.display()))?;
    let permissions = fs::metadata(staged)
        .with_context(|| format!("failed to read {}", staged.display()))?
        .permissions();
    replace(destination, &contents, Some(permissions))
}

/// Replace the file at `destination` with `contents`, keeping the
/// permissions of the file it replaces; see [`replace`].
pub(crate) fn persist(destination: &Path, contents: &[u8]) -> Result<()> {
    replace(destination, contents, None)
}

/// Replace the file at `destination` with `contents` at once, unless it
/// holds those bytes already, so that its modification time changes only
/// with its contents. The file gets `permissions`, or else keeps those of the
/// file it replaces, and a new file gets the mode that [`fs::write`] gives
/// one. The bytes go through a temporary file beside `destination`, which a
/// rename can replace it with on any file system.
fn replace(destination: &Path, contents: &[u8], permissions: Option<Permissions>) -> Result<()> {
    use std::io::Write;

    let current = optional(fs::read(destination))
        .with_context(|| format!("failed to read {}", destination.display()))?;
    if current.is_some_and(|current| current == contents) {
        return Ok(());
    }
    let parent = destination
        .parent()
        .with_context(|| format!("{} has no parent directory", destination.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let permissions = match permissions {
        Some(permissions) => Some(permissions),
        None => optional(fs::metadata(destination))?.map(|metadata| metadata.permissions()),
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(TEMPORARY_PREFIX);
    // Less the umask, as `fs::write` creates a file, rather than 0o600.
    #[cfg(unix)]
    if permissions.is_none() {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(Permissions::from_mode(0o666));
    }
    let mut temporary = builder
        .tempfile_in(parent)
        .with_context(|| format!("failed to create a file in {}", parent.display()))?;
    temporary.write_all(contents)?;
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    temporary
        .persist(destination)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to write {}", destination.display()))?;
    Ok(())
}

/// Return the result of a file operation, or `None` when the file is not
/// there.
pub(super) fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    #[test]
    fn installing_replaces_only_a_changed_file_and_leaves_nothing_beside_it() {
        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("staged");
        let destination = directory.path().join("project/pkg/api.py");
        let names = || -> Vec<String> {
            fs::read_dir(destination.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        };
        fs::write(&staged, "first").unwrap();

        install(&staged, &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "first");
        let earlier = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::options()
            .write(true)
            .open(&destination)
            .unwrap()
            .set_modified(earlier)
            .unwrap();
        install(&staged, &destination).unwrap();
        assert_eq!(
            fs::metadata(&destination).unwrap().modified().unwrap(),
            earlier,
            "an unchanged file keeps its time, so builds that watch it do not rerun"
        );

        fs::write(&staged, "second").unwrap();
        install(&staged, &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "second");
        assert_eq!(names(), ["api.py"]);
    }

    #[cfg(unix)]
    #[test]
    fn installing_keeps_the_permissions_of_the_staged_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let staged = directory.path().join("staged");
        let destination = directory.path().join("lib/libapp.so");
        fs::write(&staged, "library").unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();

        install(&staged, &destination).unwrap();

        let mode = fs::metadata(&destination).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_gets_the_mode_that_writing_it_gives() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let written = directory.path().join("written");
        let persisted = directory.path().join("persisted");
        fs::write(&written, "text").unwrap();

        persist(&persisted, b"text").unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode(&persisted), mode(&written));
    }
}
