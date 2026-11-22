//! The fingerprints that let build and check skip work: of the published
//! files, of the local inputs, and the hashing that the source fingerprint,
//! which `project::checkout` computes, shares with them.
//!
//! The source fingerprint, which `bimetal.toml` records, covers the inputs that
//! every checkout of a commit has on every host. The rest are this machine's
//! [`LocalInputs`], which the record of the installed native artifacts in
//! `.bimetal/` keeps. "Keep `[generated]` the same on every host" in
//! `docs/internals/reproducible.md` says which inputs are which.

use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::paths;

use super::publish::{self, OwnedFile};
use super::scratch;

/// The offset basis and the prime of 128-bit FNV-1a.
const FNV_OFFSET: u128 = 144_066_263_297_769_815_596_495_629_667_062_367_629;
const FNV_PRIME: u128 = 309_485_009_821_345_068_724_781_371;

/// A running 128-bit FNV-1a hash, spelled as 32 lowercase hex digits.
/// `bimetal.toml` records these, so changing what one folds in makes them stale.
#[derive(Debug, Clone)]
pub(crate) struct Fingerprint(u128);

impl Fingerprint {
    /// Start an empty fingerprint.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self(FNV_OFFSET)
    }

    /// Fold `bytes` in.
    pub(crate) fn add(&mut self, bytes: impl IntoIterator<Item = u8>) {
        for byte in bytes {
            self.0 ^= u128::from(byte);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    /// Fold a byte slice in without iterator overhead.
    pub(crate) fn add_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u128::from(byte);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    /// Stream the contents of `path` into this fingerprint using a fixed buffer
    /// so large binaries (`.wasm`, `.so`, `.node`) never allocate a whole-file
    /// buffer.
    pub(crate) fn add_file(&mut self, path: &Path) -> Result<()> {
        let mut file =
            File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if count == 0 {
                return Ok(());
            }
            self.add_bytes(&buffer[..count]);
        }
    }

    /// Fold one input in under its name: a file by its contents, a directory
    /// by the files in it that git would track, except hidden ones, as Cargo
    /// watches a directory by its files, and a missing path by its absence.
    pub(crate) fn add_input(&mut self, kind: &str, name: &str, path: &Path) -> Result<()> {
        self.add_bytes(kind.as_bytes());
        self.add_bytes(name.as_bytes());
        self.add_bytes(&[0]);
        let metadata = scratch::optional(fs::metadata(path))
            .with_context(|| format!("failed to read {}", path.display()))?;
        let Some(metadata) = metadata else {
            self.add_bytes(&[0]);
            return Ok(());
        };
        if !metadata.is_dir() {
            self.add_bytes(&[1]);
            self.add_file(path)?;
            self.add_bytes(&[0]);
            return Ok(());
        }
        self.add_bytes(&[2]);
        for file in directory_files(path)? {
            let relative = paths::portable(file.strip_prefix(path)?)?;
            self.add_bytes(relative.as_bytes());
            self.add_bytes(&[0]);
            self.add_file(&file)?;
            self.add_bytes(&[0]);
        }
        Ok(())
    }

    /// Return the hash as 32 lowercase hex digits.
    #[must_use]
    pub(crate) fn finish(self) -> String {
        format!("{:032x}", self.0)
    }
}

impl Default for Fingerprint {
    fn default() -> Self {
        Self::new()
    }
}

/// The source fingerprint that a build records, with the inputs it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sources {
    /// What `bimetal.toml` records as `source_fingerprint`.
    pub(crate) fingerprint: String,
    /// The files of the checkout that the WebAssembly build, or contract
    /// discovery without one, reads, relative to the Cargo workspace, as
    /// `bimetal.toml` records them.
    pub(crate) inputs: Vec<String>,
}

/// What a build read that only this machine has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LocalInputs {
    /// The files, and the directories that build scripts watch, by
    /// canonical path.
    pub(crate) files: Vec<PathBuf>,
    /// The environment variables, by name.
    pub(crate) environment: Vec<String>,
}

impl LocalInputs {
    /// Add the inputs of `other` that these lack, keeping both lists sorted.
    pub(crate) fn extend(&mut self, other: Self) {
        self.files.extend(other.files);
        self.files.sort();
        self.files.dedup();
        self.environment.extend(other.environment);
        self.environment.sort();
        self.environment.dedup();
    }
}

/// Return the fingerprint of a file's bytes.
#[cfg(test)]
#[must_use]
pub(super) fn content_fingerprint(bytes: &[u8]) -> String {
    let mut fingerprint = Fingerprint::new();
    fingerprint.add_bytes(bytes);
    fingerprint.finish()
}

/// Return the fingerprint of a file on disk without reading it into memory at
/// once.
pub(super) fn file_fingerprint(path: &Path) -> Result<String> {
    let mut fingerprint = Fingerprint::new();
    fingerprint.add_file(path)?;
    Ok(fingerprint.finish())
}

/// Return the fingerprint of the local inputs as they are now.
pub(crate) fn local_fingerprint(local: &LocalInputs) -> Result<String> {
    let mut fingerprint = Fingerprint::new();
    for path in &local.files {
        fingerprint.add_input("file:", &path.to_string_lossy(), path)?;
    }
    for name in &local.environment {
        let value = env::var_os(name);
        fingerprint.add(
            "environment:"
                .bytes()
                .chain(name.bytes())
                .chain([0, u8::from(value.is_some())])
                .chain(value.unwrap_or_default().into_encoded_bytes())
                .chain([0]),
        );
    }
    Ok(fingerprint.finish())
}

/// Hash the published files exactly as `check` compares them, each by its
/// host and its path in the host's project, so that the fingerprint is the
/// same wherever the checkout and the projects are.
pub(super) fn output_fingerprint(owned: &BTreeMap<PathBuf, OwnedFile>) -> Result<Option<String>> {
    let mut names = BTreeMap::new();
    for (path, file) in owned {
        let name = format!("{}:{}", file.host.name(), paths::portable(&file.relative)?);
        names.insert(name, path);
    }
    let mut fingerprint = Fingerprint::new();
    for (name, path) in names {
        if path.is_file() {
            fingerprint.add_bytes(name.as_bytes());
            fingerprint.add_bytes(&[0]);
            fingerprint.add_file(path)?;
            fingerprint.add_bytes(&[0]);
        } else {
            // A missing owned file can never fingerprint as up to date.
            return Ok(None);
        }
    }
    Ok(Some(fingerprint.finish()))
}

/// Report whether the published outputs match the recorded fingerprints, so
/// build and check can skip compilation entirely for an unchanged project.
pub(crate) fn generated_up_to_date(config: &Config, current: &Sources) -> Result<bool> {
    let generated = &config.generated;
    let Some(recorded) = generated.output_fingerprint.as_deref() else {
        return Ok(false);
    };
    if generated.source_fingerprint != current.fingerprint {
        return Ok(false);
    }
    let outputs = output_fingerprint(&publish::owned_files(config.root(), generated)?)?;
    Ok(outputs.as_deref() == Some(recorded))
}

/// Return the files below a directory that git would track, except hidden
/// ones, which can differ between machines, sorted.
fn directory_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in tracked(&[directory]).build() {
        let entry = entry?;
        if entry.file_type().is_some_and(|kind| kind.is_file()) {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

/// Return a walk of `roots` that sees the files as git does in every checkout:
/// it skips what the `.gitignore` files of each root and of its parents in
/// the repository ignore, and does not consult the machine's own ignore
/// sources (the global gitignore, `.git/info/exclude`, and `.ignore` files).
pub(crate) fn tracked(roots: &[&Path]) -> WalkBuilder {
    let (first, rest) = roots.split_first().expect("a walk has at least one root");
    let mut builder = WalkBuilder::new(first);
    for root in rest {
        builder.add(root);
    }
    builder
        .git_global(false)
        .git_exclude(false)
        .ignore(false)
        .parents(true);
    builder
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Host;

    #[test]
    fn fingerprints_are_128_bit_fnv_1a() {
        assert_eq!(content_fingerprint(b""), "6c62272e07bb014262b821756295c58d");
        assert_eq!(
            content_fingerprint(b"a"),
            "d228cb696f1a8caf78912b704e4a8964"
        );
    }

    #[test]
    fn output_fingerprints_name_files_by_host_and_project_path() {
        let directory = tempfile::tempdir().unwrap();
        let fingerprint = |host: Host, project: &str| {
            let project = directory.path().join(project);
            let file = OwnedFile {
                host,
                project: project.clone(),
                relative: PathBuf::from("pkg/api.py"),
            };
            let path = file.project.join(&file.relative);
            if !path.exists() {
                scratch::write(&path, "generated\n").unwrap();
            }
            output_fingerprint(&BTreeMap::from([(path, file)])).unwrap()
        };
        let first = fingerprint(Host::Python, "python");

        assert!(first.is_some());
        assert_eq!(fingerprint(Host::Python, "moved"), first);
        assert_ne!(fingerprint(Host::TypeScript, "typescript"), first);
        fs::write(directory.path().join("python/pkg/api.py"), "changed\n").unwrap();
        assert_ne!(fingerprint(Host::Python, "python"), first);
        fs::remove_file(directory.path().join("python/pkg/api.py")).unwrap();
        let missing = OwnedFile {
            host: Host::Python,
            project: directory.path().join("python"),
            relative: PathBuf::from("pkg/api.py"),
        };
        assert_eq!(
            output_fingerprint(&BTreeMap::from([(PathBuf::from("missing"), missing)])).unwrap(),
            None
        );
    }

    #[test]
    fn local_fingerprints_follow_the_files() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("config.toml");
        let local = LocalInputs {
            files: vec![file.clone()],
            environment: vec!["BIMETAL_TEST_UNSET_VARIABLE".to_owned()],
        };
        let missing = local_fingerprint(&local).unwrap();

        fs::write(&file, "[build]\n").unwrap();
        let present = local_fingerprint(&local).unwrap();
        assert_ne!(present, missing);
        assert_eq!(local_fingerprint(&local).unwrap(), present);
        fs::write(&file, "[build]\njobs = 1\n").unwrap();
        assert_ne!(local_fingerprint(&local).unwrap(), present);
    }

    #[test]
    fn file_fingerprint_matches_content_fingerprint_across_chunks() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("binary.bin");
        let bytes: Vec<u8> = (0..150_000_usize)
            .map(|index| (index.wrapping_mul(31) & 0xff) as u8)
            .collect();
        fs::write(&file, &bytes).unwrap();
        assert_eq!(
            file_fingerprint(&file).unwrap(),
            content_fingerprint(&bytes)
        );
    }
}
