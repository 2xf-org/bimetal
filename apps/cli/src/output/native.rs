//! Which sources built the installed native artifacts, and what else this
//! machine's last build read.
//!
//! The native artifacts, such as the Python extension, are build products
//! that git ignores, so after a checkout of another commit they can belong to
//! other sources while every committed file matches. Bimetal records, in
//! `.bimetal/native.json`, the source fingerprint and each artifact that it
//! installs, and a build compiles them again when either differs. The record
//! also keeps the inputs that no committed fingerprint can cover, with their
//! fingerprint, and a build starts again from the sources when they change.
//! Where the artifacts are follows from the tables of `bimetal.toml`, which the
//! source fingerprint covers.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

use super::fingerprint::{self, LocalInputs};
use super::scratch::{self, SCRATCH_DIRECTORY};

/// The record file's name in `.bimetal/`.
const RECORD_FILE: &str = "native.json";

/// How the installed artifacts were built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    /// The source fingerprint of the sources they were built from.
    source_fingerprint: String,
    /// What else the builds read.
    local_inputs: LocalInputs,
    /// The fingerprint of the local inputs when they were built.
    local_fingerprint: String,
    /// The installed files.
    artifacts: Vec<Artifact>,
}

/// One installed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Artifact {
    /// The path, relative to `bimetal.toml`.
    path: String,
    /// The size and the modification time after installation, which spare
    /// hashing an artifact that did not change.
    size: u64,
    mtime: SystemTime,
    /// The fingerprint of the contents.
    content_fingerprint: String,
}

impl Artifact {
    /// Describe the file that Bimetal installed at `path`.
    fn installed(project_root: &Path, path: &Path) -> Result<Self> {
        let contents =
            fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let metadata = fs::metadata(path)?;
        Ok(Self {
            path: paths::portable_relative(project_root, path)?,
            size: metadata.len(),
            mtime: metadata.modified()?,
            content_fingerprint: fingerprint::content_fingerprint(&contents),
        })
    }

    /// Return whether the file at `path` still has the contents that Bimetal
    /// installed, which it hashes only when its size or its modification
    /// time changed.
    fn is_current(&self, path: &Path) -> Result<bool> {
        let metadata = scratch::optional(fs::metadata(path))
            .with_context(|| format!("failed to read {}", path.display()))?;
        let Some(metadata) = metadata else {
            return Ok(false);
        };
        if metadata.len() == self.size && metadata.modified()? == self.mtime {
            return Ok(true);
        }
        let contents =
            fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        Ok(fingerprint::content_fingerprint(&contents) == self.content_fingerprint)
    }
}

/// What a build must do about the installed native artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstalledNative {
    /// Nothing: they were built from these sources and have not changed
    /// since, or the application has none.
    Current,
    /// Compile them again: one is missing or changed, or they came from
    /// other sources or from an older Bimetal.
    Outdated,
    /// Build everything again: something that only this machine reads, such
    /// as an environment variable, changed since the last build, and can
    /// change every output.
    LocalInputsChanged,
}

/// Return what a build must do about the native artifacts at `artifacts`,
/// given the current source fingerprint.
pub(crate) fn installed_native(
    project_root: &Path,
    artifacts: &[PathBuf],
    source_fingerprint: &str,
) -> Result<InstalledNative> {
    let Some(record) = read(project_root)? else {
        return Ok(if artifacts.is_empty() {
            InstalledNative::Current
        } else {
            InstalledNative::Outdated
        });
    };
    if fingerprint::local_fingerprint(&record.local_inputs)? != record.local_fingerprint {
        return Ok(InstalledNative::LocalInputsChanged);
    }
    if artifacts.is_empty() {
        return Ok(InstalledNative::Current);
    }
    if record.source_fingerprint != source_fingerprint || record.artifacts.len() != artifacts.len()
    {
        return Ok(InstalledNative::Outdated);
    }
    for (recorded, path) in record.artifacts.iter().zip(artifacts) {
        let relative = paths::portable_relative(project_root, path)?;
        if recorded.path != relative || !recorded.is_current(path)? {
            return Ok(InstalledNative::Outdated);
        }
    }
    Ok(InstalledNative::Current)
}

/// Return the local inputs that the record keeps for the sources with this
/// fingerprint, if none changed since it was written, and none otherwise. A
/// build of the native artifacts alone keeps them for the builds that it
/// skips.
pub(crate) fn recorded_local_inputs(
    project_root: &Path,
    source_fingerprint: &str,
) -> Result<LocalInputs> {
    let Some(record) = read(project_root)? else {
        return Ok(LocalInputs::default());
    };
    if record.source_fingerprint != source_fingerprint
        || fingerprint::local_fingerprint(&record.local_inputs)? != record.local_fingerprint
    {
        return Ok(LocalInputs::default());
    }
    Ok(record.local_inputs)
}

/// Forget how the installed artifacts were built, before they are replaced,
/// so that an interrupted installation never leaves a record that claims
/// them.
pub(crate) fn forget_native(project_root: &Path) -> Result<()> {
    let path = project_root.join(SCRATCH_DIRECTORY).join(RECORD_FILE);
    scratch::optional(fs::remove_file(&path))
        .with_context(|| format!("failed to remove {}", path.display()))?;
    Ok(())
}

/// Record that the artifacts now at `artifacts` were built from the sources
/// with this fingerprint, and from `local_inputs` as they are now.
pub(crate) fn record_native(
    project_root: &Path,
    artifacts: &[PathBuf],
    source_fingerprint: &str,
    local_inputs: &LocalInputs,
) -> Result<()> {
    let record = Record {
        source_fingerprint: source_fingerprint.to_owned(),
        local_inputs: local_inputs.clone(),
        local_fingerprint: fingerprint::local_fingerprint(local_inputs)?,
        artifacts: artifacts
            .iter()
            .map(|path| Artifact::installed(project_root, path))
            .collect::<Result<_>>()?,
    };
    let path = scratch::scratch_directory(project_root)?.join(RECORD_FILE);
    scratch::persist(&path, &serde_json::to_vec_pretty(&record)?)
        .context("failed to record the installed native artifacts")
}

/// Read the record. A missing or damaged file records nothing, which only
/// costs a rebuild of the artifacts.
fn read(project_root: &Path) -> Result<Option<Record>> {
    let path = project_root.join(SCRATCH_DIRECTORY).join(RECORD_FILE);
    let source = scratch::optional(fs::read_to_string(&path))
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(source.and_then(|source| serde_json::from_str(&source).ok()))
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::slice;

    use super::*;

    /// Return a project with a directory for an extension, and the
    /// extension's path.
    fn project() -> (tempfile::TempDir, PathBuf) {
        let project = tempfile::tempdir().unwrap();
        let extension = project.path().join("python/app/_native.abi3.so");
        fs::create_dir_all(extension.parent().unwrap()).unwrap();
        (project, extension)
    }

    #[test]
    fn artifacts_are_current_only_for_their_sources_and_contents() {
        let (project, extension) = project();
        let artifacts = [extension.clone()];
        let state = |fingerprint| installed_native(project.path(), &artifacts, fingerprint);
        let local = LocalInputs::default();
        assert_eq!(state("one").unwrap(), InstalledNative::Outdated);

        fs::write(&extension, "built from one").unwrap();
        record_native(project.path(), &artifacts, "one", &local).unwrap();
        assert_eq!(state("one").unwrap(), InstalledNative::Current);
        // Another branch's sources.
        assert_eq!(state("two").unwrap(), InstalledNative::Outdated);

        // Another branch's extension, left behind because git ignores it.
        fs::write(&extension, "built from two!").unwrap();
        assert_eq!(state("one").unwrap(), InstalledNative::Outdated);

        record_native(project.path(), &artifacts, "two", &local).unwrap();
        forget_native(project.path()).unwrap();
        assert_eq!(state("two").unwrap(), InstalledNative::Outdated);

        forget_native(project.path()).unwrap();
        fs::write(
            project.path().join(SCRATCH_DIRECTORY).join(RECORD_FILE),
            "damaged",
        )
        .unwrap();
        assert_eq!(state("two").unwrap(), InstalledNative::Outdated);
    }

    #[test]
    fn only_a_changed_size_or_time_hashes_an_artifact() {
        let (project, extension) = project();
        let artifacts = [extension.clone()];
        fs::write(&extension, "first").unwrap();
        record_native(project.path(), &artifacts, "one", &LocalInputs::default()).unwrap();
        let installed = fs::metadata(&extension).unwrap().modified().unwrap();
        let state = || installed_native(project.path(), &artifacts, "one").unwrap();

        // The same size and time pass without a read.
        fs::write(&extension, "other").unwrap();
        File::options()
            .write(true)
            .open(&extension)
            .unwrap()
            .set_modified(installed)
            .unwrap();
        assert_eq!(state(), InstalledNative::Current);

        // A new time with the recorded contents passes after a read.
        fs::write(&extension, "first").unwrap();
        assert_eq!(state(), InstalledNative::Current);
        fs::write(&extension, "second").unwrap();
        assert_eq!(state(), InstalledNative::Outdated);
    }

    #[test]
    fn every_artifact_counts() {
        let (project, extension) = project();
        let addon = project.path().join("typescript/app/native/native.node");
        fs::create_dir_all(addon.parent().unwrap()).unwrap();
        fs::write(&extension, "extension").unwrap();
        fs::write(&addon, "addon").unwrap();
        let both = [extension.clone(), addon.clone()];
        record_native(project.path(), &both, "one", &LocalInputs::default()).unwrap();
        assert_eq!(
            installed_native(project.path(), &both, "one").unwrap(),
            InstalledNative::Current
        );

        assert_eq!(
            installed_native(project.path(), &[extension], "one").unwrap(),
            InstalledNative::Outdated
        );
        fs::remove_file(&addon).unwrap();
        assert_eq!(
            installed_native(project.path(), &both, "one").unwrap(),
            InstalledNative::Outdated
        );
    }

    #[test]
    fn an_application_without_artifacts_is_current() {
        let project = tempfile::tempdir().unwrap();
        let state = || installed_native(project.path(), &[], "one").unwrap();
        assert_eq!(state(), InstalledNative::Current);

        record_native(project.path(), &[], "other", &LocalInputs::default()).unwrap();

        assert_eq!(state(), InstalledNative::Current);
    }

    #[test]
    fn a_change_to_a_local_input_rebuilds_everything() {
        let (project, extension) = project();
        let macos = project.path().join("src/macos.txt");
        fs::create_dir_all(macos.parent().unwrap()).unwrap();
        fs::write(&extension, "built").unwrap();
        fs::write(&macos, "first").unwrap();
        let local = LocalInputs {
            files: vec![macos.clone()],
            environment: Vec::new(),
        };
        for artifacts in [vec![extension.clone()], Vec::new()] {
            record_native(project.path(), &artifacts, "one", &local).unwrap();
            let state = || installed_native(project.path(), &artifacts, "one").unwrap();
            assert_eq!(state(), InstalledNative::Current);

            fs::write(&macos, "edited").unwrap();
            assert_eq!(state(), InstalledNative::LocalInputsChanged);
            fs::write(&macos, "first").unwrap();
        }
    }

    #[test]
    fn a_native_build_keeps_what_the_skipped_builds_read() {
        let (project, extension) = project();
        let wasm_only = project.path().join("src/wasm.txt");
        fs::create_dir_all(wasm_only.parent().unwrap()).unwrap();
        fs::write(&extension, "built").unwrap();
        fs::write(&wasm_only, "first").unwrap();
        let full = LocalInputs {
            files: vec![wasm_only.clone()],
            environment: vec!["API_URL".to_owned()],
        };
        record_native(project.path(), slice::from_ref(&extension), "one", &full).unwrap();
        let recorded = |fingerprint| recorded_local_inputs(project.path(), fingerprint).unwrap();

        assert_eq!(recorded("two"), LocalInputs::default());
        let mut native = LocalInputs {
            files: vec![extension.clone()],
            environment: vec!["PYO3_PYTHON".to_owned()],
        };
        native.extend(recorded("one"));
        let mut files = vec![extension.clone(), wasm_only.clone()];
        files.sort();
        assert_eq!(
            native,
            LocalInputs {
                files,
                environment: vec!["API_URL".to_owned(), "PYO3_PYTHON".to_owned()],
            }
        );

        // An input that changed since must take a full build, not hide in a
        // new record.
        fs::write(&wasm_only, "edited").unwrap();
        assert_eq!(recorded("one"), LocalInputs::default());
    }
}
