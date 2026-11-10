//! The source fingerprint, which `bimetal.toml` records: what every checkout of
//! a commit has that can change the build, and the inputs of a build that
//! only this machine has.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::IncrementalIgnore;

use crate::cargo::dep_info::Reads;
use crate::config::Config;
use crate::output::{self, Fingerprint, LocalInputs, Sources};
use crate::paths;

use super::{Project, SourceRoot};

/// Hash what every checkout of the commit has that can change this
/// application's build: the manifests of the workspace and of the local
/// packages, the lockfile, the toolchain, the checkout's Cargo
/// configuration, `inputs`, the generator, the user-owned tables of
/// `bimetal.toml`, and the folders that name the namespaces when
/// `namespaces = "folders"`.
///
/// A fresh clone computes the same fingerprint on any host, without
/// building.
pub(super) fn source_fingerprint(
    project: &Project,
    config: &Config,
    inputs: &[String],
) -> Result<Sources> {
    let mut checkout = Checkout::new(project);
    let mut fingerprint = Fingerprint::new();
    // The lockfile pins git dependencies.
    for name in ["Cargo.toml", "Cargo.lock"] {
        let path = project.workspace_root.join(name);
        fingerprint.add_input("manifest:", name, &path)?;
    }
    // Cargo builds a package's `build.rs` when there is one, so creating it
    // changes the build before any dep-info file lists it.
    for source_root in &project.source_roots {
        for file in ["Cargo.toml", "build.rs"] {
            let name = format!("{}/{file}", source_root.identity);
            fingerprint.add_input("manifest:", &name, &source_root.path.join(file))?;
        }
    }
    fingerprint.add(
        "toolchain:"
            .bytes()
            .chain(project.toolchain.identity().bytes()),
    );
    // Cargo configuration changes flags, features, and patches.
    for path in project.configuration.paths() {
        let path = resolved(path)?;
        if let Some(name) = checkout.name(&path)? {
            fingerprint.add_input("configuration:", &name, &path)?;
        }
    }
    let unique: BTreeSet<&String> = inputs.iter().collect();
    let inputs: Vec<String> = unique.into_iter().cloned().collect();
    for input in &inputs {
        let path = project.workspace_root.join(paths::parse(input)?);
        fingerprint.add_input("input:", input, &path)?;
    }
    fingerprint.add(env!("BIMETAL_GENERATOR_FINGERPRINT").bytes());
    fingerprint.add(config.settings_fingerprint_source()?.bytes());
    if let Some(folders) = &project.folders {
        fingerprint.add(
            "namespaces:"
                .bytes()
                .chain(folders.fingerprint_source().bytes()),
        );
    }
    Ok(Sources {
        fingerprint: fingerprint.finish(),
        inputs,
    })
}

/// Return the inputs that the source fingerprint covers: the files of the
/// checkout that `reads`, those of the WebAssembly build or of contract
/// discovery without one, name, by their names in `bimetal.toml`, sorted.
pub(super) fn committed_inputs(project: &Project, reads: &Reads) -> Result<Vec<String>> {
    Checkout::new(project).committed_inputs(reads)
}

/// Return what the builds read that the source fingerprint, with these
/// `committed` inputs, does not cover.
pub(super) fn local_inputs(
    project: &Project,
    reads: &[&Reads],
    committed: &[String],
) -> Result<LocalInputs> {
    Checkout::new(project).local_inputs(reads, project.configuration.paths(), committed)
}

/// The files that every checkout of a commit has: those that git would
/// track, hidden ones included, in the git repository that holds the
/// workspace, or in the workspace outside a repository, and in the local
/// packages.
struct Checkout {
    workspace_root: PathBuf,
    /// The Cargo target directory, whose files Cargo writes.
    target: Option<PathBuf>,
    /// One matcher for the repository and one for each local package.
    matchers: Vec<IncrementalIgnore>,
}

impl Checkout {
    #[must_use]
    fn new(project: &Project) -> Self {
        Self::with_roots(
            &project.workspace_root,
            &project.source_roots,
            &project.target_directory,
        )
    }

    #[must_use]
    fn with_roots(
        workspace_root: &Path,
        source_roots: &[SourceRoot],
        target_directory: &Path,
    ) -> Self {
        let workspace_root =
            paths::canonical(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        let repository = workspace_root
            .ancestors()
            .find(|directory| directory.join(".git").exists())
            .unwrap_or(&workspace_root);
        let mut roots = vec![repository];
        roots.extend(source_roots.iter().map(|root| root.path.as_path()));
        let matchers = output::tracked(&roots).hidden(false).build_matchers();
        Self {
            workspace_root,
            target: paths::canonical(target_directory).ok(),
            matchers,
        }
    }

    /// Return the name in `bimetal.toml` of a file, by canonical path, that
    /// every checkout has: its path relative to the workspace. `None` means
    /// that only this machine has it.
    fn name(&mut self, path: &Path) -> Result<Option<String>> {
        // The deepest directory that holds the file sees it as git does.
        let Some(matcher) = self
            .matchers
            .iter_mut()
            .filter(|matcher| path.starts_with(matcher.root()))
            .max_by_key(|matcher| matcher.root().components().count())
        else {
            return Ok(None);
        };
        let relative = path.strip_prefix(matcher.root())?;
        let in_git = relative
            .components()
            .any(|component| component.as_os_str() == ".git");
        // A file on another drive, such as a path dependency, has no name
        // relative to the workspace.
        let local = !paths::share_root(&self.workspace_root, path);
        if local || in_git || matcher.matched(relative, path.is_dir()).is_ignore() {
            return Ok(None);
        }
        paths::portable_relative(&self.workspace_root, path).map(Some)
    }

    /// Return whether Cargo wrote a file, as it writes the link crate and
    /// what build scripts generate, in its target directory.
    #[must_use]
    fn is_build_product(&self, path: &Path) -> bool {
        self.target
            .as_deref()
            .is_some_and(|target| path.starts_with(target))
    }

    /// See [`committed_inputs`].
    fn committed_inputs(&mut self, reads: &Reads) -> Result<Vec<String>> {
        let mut inputs = BTreeSet::new();
        for path in &reads.files {
            let path = resolved(path)?;
            if self.is_build_product(&path) {
                continue;
            }
            inputs.extend(self.name(&path)?);
        }
        Ok(inputs.into_iter().collect())
    }

    /// See [`local_inputs`]; `configuration` lists the Cargo configuration
    /// files that the builds read.
    fn local_inputs(
        &mut self,
        reads: &[&Reads],
        configuration: &[PathBuf],
        committed: &[String],
    ) -> Result<LocalInputs> {
        let committed: BTreeSet<&str> = committed.iter().map(String::as_str).collect();
        let mut files = BTreeSet::new();
        let mut environment = BTreeSet::new();
        for reads in reads {
            for path in &reads.files {
                let path = resolved(path)?;
                if self.is_build_product(&path) {
                    continue;
                }
                let name = self.name(&path)?;
                if !name.is_some_and(|name| committed.contains(name.as_str())) {
                    files.insert(path);
                }
            }
            environment.extend(reads.environment.iter().cloned());
        }
        for path in configuration {
            let path = resolved(path)?;
            if self.name(&path)?.is_none() {
                files.insert(path);
            }
        }
        Ok(LocalInputs {
            files: files.into_iter().collect(),
            environment: environment.into_iter().collect(),
        })
    }
}

/// Return the canonical path of a file that a build read, which may no
/// longer exist. Cargo reaches a package by the path that names it, which can
/// go through a symlink.
fn resolved(path: &Path) -> Result<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        let canonical = match paths::canonical(existing) {
            Ok(canonical) => Some(canonical),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| format!("cannot resolve {}", path.display()));
            }
        };
        if let Some(canonical) = canonical {
            return Ok(missing
                .into_iter()
                .rev()
                .fold(canonical, |path, name| path.join(name)));
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            bail!("cannot find {}", path.display());
        };
        missing.push(name);
        existing = parent;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::slice;

    use super::*;

    /// Write a repository with an application in `app`, which `.gitignore`
    /// files in the repository and in the application ignore parts of.
    fn repository() -> (tempfile::TempDir, PathBuf) {
        let repository = tempfile::tempdir().unwrap();
        let root = paths::canonical(repository.path()).unwrap();
        for (path, contents) in [
            (".git/HEAD", "ref: refs/heads/main\n"),
            (".gitignore", "*.local.rs\nscratch/\n"),
            ("docs/guide.md", "guide\n"),
            ("app/Cargo.toml", "[package]\n"),
            ("app/src/lib.rs", "lib\n"),
            ("app/src/macos.txt", "macos\n"),
            ("app/src/notes.local.rs", "machine-local\n"),
            ("app/src/data.txt", "data\n"),
            ("app/.sqlx/query.json", "{}\n"),
            ("app/assets/logo.svg", "logo\n"),
            ("app/assets/.DS_Store", "finder\n"),
            ("app/assets/draft.secret", "draft\n"),
            ("app/scratch/data.txt", "scratch\n"),
            ("app/target/out.txt", "built\n"),
            ("app/.gitignore", "*.secret\n"),
            ("app/src/key.secret", "secret\n"),
        ] {
            output::write(&root.join(path), contents).unwrap();
        }
        (repository, root)
    }

    /// Return the checkout of a workspace in `root`, with the application in
    /// `root/app` as its one local package.
    fn checkout(root: &Path, workspace: &str) -> Checkout {
        let source_roots = [SourceRoot {
            identity: "app".to_owned(),
            path: root.join("app"),
        }];
        Checkout::with_roots(
            &root.join(workspace),
            &source_roots,
            &root.join("app/target"),
        )
    }

    /// Return the reads of the files at these paths in `root`.
    fn reads(root: &Path, paths: &[&str], environment: &[&str]) -> Reads {
        Reads {
            files: paths.iter().map(|path| root.join(path)).collect(),
            environment: environment.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    #[test]
    fn committed_inputs_are_the_tracked_files_that_the_wasm_build_reads() {
        let (_repository, root) = repository();
        let wasm = reads(
            &root,
            &[
                "app/src/lib.rs",
                "app/src/data.txt",
                "app/.sqlx/query.json",
                "app/assets",
                "docs/guide.md",
                "app/src/notes.local.rs",
                "app/scratch/data.txt",
                "app/src/key.secret",
                "app/target/out.txt",
                ".git/HEAD",
                "app/missing.txt",
            ],
            &[],
        );

        let inputs = checkout(&root, "app").committed_inputs(&wasm).unwrap();
        assert_eq!(
            inputs,
            [
                "../docs/guide.md",
                ".sqlx/query.json",
                "assets",
                "missing.txt",
                "src/data.txt",
                "src/lib.rs"
            ]
        );

        let inputs = checkout(&root, "").committed_inputs(&wasm).unwrap();
        assert_eq!(
            inputs,
            [
                "app/.sqlx/query.json",
                "app/assets",
                "app/missing.txt",
                "app/src/data.txt",
                "app/src/lib.rs",
                "docs/guide.md"
            ]
        );
    }

    #[test]
    fn local_inputs_are_what_only_this_machine_reads() {
        let (_repository, root) = repository();
        let outside = tempfile::tempdir().unwrap();
        let outside = paths::canonical(outside.path())
            .unwrap()
            .join("config.toml");
        fs::write(&outside, "[build]\n").unwrap();
        let mut checkout = checkout(&root, "app");
        let wasm = reads(
            &root,
            &["app/src/lib.rs", "app/src/key.secret"],
            &["API_URL"],
        );
        let host = reads(
            &root,
            &[
                "app/src/lib.rs",
                "app/src/macos.txt",
                "app/target/out.txt",
                ".git/HEAD",
            ],
            &["PYO3_PYTHON"],
        );
        let committed = checkout.committed_inputs(&wasm).unwrap();

        let local = checkout
            .local_inputs(&[&host, &wasm], slice::from_ref(&outside), &committed)
            .unwrap();

        let mut files = vec![
            root.join(".git/HEAD"),
            root.join("app/src/key.secret"),
            root.join("app/src/macos.txt"),
            outside,
        ];
        files.sort();
        assert_eq!(
            local,
            LocalInputs {
                files,
                environment: vec!["API_URL".to_owned(), "PYO3_PYTHON".to_owned()],
            }
        );
    }

    #[test]
    fn a_directory_counts_with_its_tracked_files() {
        let (_repository, root) = repository();
        let assets = root.join("app/assets");
        let fingerprint = || {
            let mut fingerprint = Fingerprint::new();
            fingerprint.add_input("input:", "assets", &assets).unwrap();
            fingerprint.finish()
        };
        let first = fingerprint();

        fs::write(assets.join(".DS_Store"), "moved icons\n").unwrap();
        fs::write(assets.join("draft.secret"), "edited\n").unwrap();
        assert_eq!(fingerprint(), first);

        fs::write(assets.join("logo.svg"), "new logo\n").unwrap();
        let edited = fingerprint();
        assert_ne!(edited, first);
        fs::write(assets.join("icon.svg"), "icon\n").unwrap();
        assert_ne!(fingerprint(), edited);
    }

    #[test]
    fn resolved_paths_keep_their_missing_part() {
        let directory = tempfile::tempdir().unwrap();
        let canonical = paths::canonical(directory.path()).unwrap();

        assert_eq!(
            resolved(&directory.path().join("missing/data.txt")).unwrap(),
            canonical.join("missing/data.txt")
        );
    }
}
