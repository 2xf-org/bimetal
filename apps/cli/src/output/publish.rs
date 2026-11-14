//! Publication of a staged tree, the comparison that `check` makes against
//! it, and the paths Bimetal owns.
//!
//! Each generator stages its host's files in `<stage>/<host>/`, as they go
//! in the host's project. Publication checks every destination before it
//! changes one, copies each staged file whose bytes changed into place,
//! deletes the stale files that are Bimetal's, and records the result in
//! `bimetal.toml` last. A command
//! that dies midway leaves a mix of both builds' files, all of them Bimetal's.
//! `check` reports them, and the next build adopts and replaces them.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, DirEntry, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{Config, GeneratedState, Host, HostPackage, HostRecord};
use crate::dependencies::HostDependencies;
use crate::paths;
use crate::problems;
use crate::typescript;

use super::GENERATED_BANNER;
use super::artifacts::{self, Platform};
use super::fingerprint::{self, Sources};
use super::scratch;

/// The generated files that differ from what the Rust sources generate, by
/// their paths relative to `bimetal.toml`.
#[derive(Debug, Default, Serialize)]
pub(crate) struct OutOfSync {
    /// Files that the sources generate, and that are not there.
    pub(crate) missing: Vec<String>,
    /// Files whose contents differ.
    pub(crate) changed: Vec<String>,
    /// Files that Bimetal generated before, and that the sources no longer do.
    pub(crate) stale: Vec<String>,
}

impl OutOfSync {
    /// Return whether every generated file is in sync.
    #[must_use]
    fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.changed.is_empty() && self.stale.is_empty()
    }
}

impl fmt::Display for OutOfSync {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the generated files are not in sync with the Rust sources; run `bimetal build`"
        )?;
        for (state, paths) in [
            ("missing", &self.missing),
            ("changed", &self.changed),
            ("stale", &self.stale),
        ] {
            for path in paths {
                write!(formatter, "\n  {state}: {path}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for OutOfSync {}

/// A file in a host's project that Bimetal generates, or generated before.
#[derive(Clone, Debug)]
pub(super) struct OwnedFile {
    pub(super) host: Host,
    /// The host's project.
    pub(super) project: PathBuf,
    /// The file, relative to the project.
    pub(super) relative: PathBuf,
}

impl OwnedFile {
    /// Return where the file is.
    #[must_use]
    fn path(&self) -> PathBuf {
        self.project.join(&self.relative)
    }
}

/// One file of a staged tree.
struct StagedFile {
    /// Where the generator wrote it.
    staged: PathBuf,
    /// Where it goes.
    owned: OwnedFile,
}

/// The files of a staged tree, and the state that publishing them records.
struct GeneratedPlan {
    ownership: GeneratedState,
    /// The staged files, by where they go.
    files: BTreeMap<PathBuf, StagedFile>,
}

impl GeneratedPlan {
    /// List the staged tree of each enabled host, and derive the state
    /// published with it: the ownership of its files, and the build's
    /// sources and dependencies. `root` is the directory of `bimetal.toml`.
    fn read(
        stage: &Path,
        root: &Path,
        hosts: &[&HostPackage],
        sources: &Sources,
        dependencies: &HostDependencies,
    ) -> Result<Self> {
        let mut ownership = GeneratedState {
            source_fingerprint: sources.fingerprint.clone(),
            inputs: sources.inputs.clone(),
            ..GeneratedState::default()
        };
        let mut files = BTreeMap::new();
        for package in hosts {
            let directory = stage.join(package.host.name());
            let mut names = Vec::new();
            for relative in staged_files(&directory)? {
                names.push(paths::portable(&relative)?);
                let owned = OwnedFile {
                    host: package.host,
                    project: package.project.clone(),
                    relative: relative.clone(),
                };
                let staged = StagedFile {
                    staged: directory.join(relative),
                    owned,
                };
                files.insert(staged.owned.path(), staged);
            }
            *ownership.record_mut(package.host) = Some(HostRecord {
                project: paths::portable_relative(root, &package.project)?,
                files: names,
                dependencies: HostDependencies::names(dependencies.of(package.host)),
            });
        }
        Ok(Self { ownership, files })
    }
}

/// Publish the staged tree in `stage` into the projects of `hosts`, record
/// it in `bimetal.toml`, and return the state recorded. Every user-owned path
/// stays as it is: a collision stops the publication before it changes
/// anything. The files of a host that `bimetal.toml` no longer turns on are
/// stale.
pub(crate) fn reconcile_generated_files(
    stage: &Path,
    hosts: &[&HostPackage],
    sources: &Sources,
    dependencies: &HostDependencies,
    config: &Config,
) -> Result<GeneratedState> {
    let root = config.root();
    let plan = GeneratedPlan::read(stage, root, hosts, sources, dependencies)?;
    // Previous files are where their records say, so a project that moved
    // leaves its old files behind as stale.
    let previous = owned_files(root, &config.generated)?;
    let installed = implied_native_artifacts(plan.files.values().map(|file| &file.owned));
    // A stale path is deleted only when it is Bimetal's: a generated file by
    // its banner, a native artifact by its name.
    let mut stale = BTreeMap::new();
    let current = |path: &Path| plan.files.contains_key(path) || installed.contains_key(path);
    for (path, file) in &previous {
        if !current(path) && is_regular_file(path)? && is_generated(path)? {
            stale.insert(path.clone(), file.clone());
        }
    }
    for (path, file) in implied_native_artifacts(previous.values()) {
        if !current(&path) && is_regular_file(&path)? {
            stale.insert(path, file);
        }
    }
    validate_destinations(&plan, &previous, &stale)?;

    for (destination, file) in &plan.files {
        scratch::install(&file.staged, destination)?;
    }
    for path in stale.keys() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    cleanup_empty_directories(stale.values())?;

    let mut published = plan.ownership;
    published.output_fingerprint =
        fingerprint::output_fingerprint(&owned_files(root, &published)?)?;
    write_ownership(config, &published)?;
    Ok(published)
}

/// Reject all user-owned collisions before any project path is mutated.
///
/// A file that `[generated]` does not list, but that Bimetal generated, is
/// adopted, so a merge that lost entries or the whole table, or a build that
/// died before it wrote `bimetal.toml`, needs no repair.
fn validate_destinations(
    plan: &GeneratedPlan,
    previous: &BTreeMap<PathBuf, OwnedFile>,
    stale: &BTreeMap<PathBuf, OwnedFile>,
) -> Result<()> {
    let expected = plan.files.iter().map(|(path, file)| (path, &file.owned));
    for (destination, file) in expected.chain(stale) {
        validate_parent_directories(file)?;
        let new = plan.files.contains_key(destination) && !previous.contains_key(destination);
        match scratch::optional(fs::symlink_metadata(destination))? {
            Some(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if new && !is_generated(destination)? {
                    bail!(
                        "generated path collides with a user-owned file: {}. Bimetal replaces only the files that it generated, which start with its AUTO-GENERATED banner; move this one away",
                        destination.display()
                    );
                }
            }
            Some(_) if new => bail!(
                "generated path collides with a user-owned path: {}",
                destination.display()
            ),
            Some(_) => bail!(
                "generated path is not a normal file: {}",
                destination.display()
            ),
            None => {}
        }
    }
    Ok(())
}

/// Reject a project that is not a directory, and symlink or file collisions
/// in every existing directory between the project and the file.
fn validate_parent_directories(file: &OwnedFile) -> Result<()> {
    // The project is the user's, who may make it a symlink.
    match scratch::optional(fs::metadata(&file.project))? {
        Some(metadata) if metadata.is_dir() => {}
        Some(_) => bail!(
            "generated path collides with a user-owned path: {}",
            file.project.display()
        ),
        None => return Ok(()),
    }
    let mut parent = file.project.clone();
    if let Some(relative_parent) = file.relative.parent() {
        for component in relative_parent.components() {
            parent.push(component);
            match scratch::optional(fs::symlink_metadata(&parent))? {
                Some(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Some(_) => {
                    bail!(
                        "generated path collides with a user-owned path: {}",
                        parent.display()
                    );
                }
                None => break,
            }
        }
    }
    Ok(())
}

/// Compare the staged tree with the files that the projects of `hosts` hold,
/// and with the files that `bimetal.toml` records. Native artifacts, which
/// each platform builds for itself, are no part of either.
pub(crate) fn check_generated_files(
    stage: &Path,
    hosts: &[&HostPackage],
    sources: &Sources,
    dependencies: &HostDependencies,
    config: &Config,
) -> Result<()> {
    let root = config.root();
    let plan = GeneratedPlan::read(stage, root, hosts, sources, dependencies)?;
    let name = |path: &Path| paths::portable_relative(root, path);
    let mut out_of_sync = OutOfSync::default();
    for (destination, file) in &plan.files {
        if !destination.is_file() {
            out_of_sync.missing.push(name(destination)?);
        } else if fs::read(destination)? != fs::read(&file.staged)? {
            out_of_sync.changed.push(name(destination)?);
        }
    }
    for path in owned_files(root, &config.generated)?.keys() {
        if !plan.files.contains_key(path) && path.is_file() {
            out_of_sync.stale.push(name(path)?);
        }
    }
    if !out_of_sync.is_empty() {
        return Err(out_of_sync.into());
    }
    if config.generated != plan.ownership {
        bail!(
            "generated files match the sources, but the recorded state in bimetal.toml is stale; run `bimetal build` to refresh it"
        );
    }
    Ok(())
}

/// Atomically replace the generated ownership subtree in `bimetal.toml`.
fn write_ownership(config: &Config, ownership: &GeneratedState) -> Result<()> {
    let source = config.render_generated(ownership)?;
    scratch::persist(&config.path, source.as_bytes())
}

/// Return the files that the records of a `[generated]` state list, by
/// where they are. `root` is the directory of `bimetal.toml`.
pub(super) fn owned_files(
    root: &Path,
    ownership: &GeneratedState,
) -> Result<BTreeMap<PathBuf, OwnedFile>> {
    let mut owned = BTreeMap::new();
    for (host, record) in ownership.records() {
        let project = paths::normalized(&root.join(paths::parse(&record.project)?));
        for value in &record.files {
            let file = OwnedFile {
                host,
                project: project.clone(),
                relative: paths::parse(value)?,
            };
            owned.insert(file.path(), file);
        }
    }
    Ok(owned)
}

/// Return the native artifacts of every platform that generated files among
/// `files` imply, which no record lists, because each platform builds its
/// own: the Python extension beside each stub, under the stub's name, and the
/// Node.js addon beside each TypeScript runtime that loads no Wasm module.
fn implied_native_artifacts<'file>(
    files: impl Iterator<Item = &'file OwnedFile>,
) -> BTreeMap<PathBuf, OwnedFile> {
    let files: Vec<&OwnedFile> = files.collect();
    let paths: BTreeSet<PathBuf> = files.iter().map(|file| file.path()).collect();
    let runtime = format!("{}.js", typescript::RUNTIME_MODULE);
    let mut artifacts = BTreeMap::new();
    for file in files {
        let relative = &file.relative;
        let name = relative.file_name();
        let directory = || relative.parent().and_then(Path::to_str);
        let package = match file.host {
            Host::Python if relative.extension() == Some("pyi".as_ref()) => {
                for platform in Platform::ALL {
                    let artifact = OwnedFile {
                        relative: artifacts::python_extension(relative, platform),
                        ..file.clone()
                    };
                    artifacts.insert(artifact.path(), artifact);
                }
                None
            }
            Host::TypeScript if name == Some(runtime.as_ref()) => {
                let native = relative.with_file_name(typescript::NATIVE_DIRECTORY);
                let wasm = file.project.join(native.join(typescript::WASM_MODULE));
                if paths.contains(&wasm) {
                    None
                } else {
                    directory()
                }
            }
            _ => None,
        };
        let Some(package) = package else {
            continue;
        };
        for platform in Platform::ALL {
            let artifact = OwnedFile {
                relative: artifacts::native_file(file.host, package, platform),
                ..file.clone()
            };
            artifacts.insert(artifact.path(), artifact);
        }
    }
    artifacts
}

/// Reject every `[generated]` record whose project no host can have, or
/// that lists a path outside its project, before any file is replaced or
/// removed. `root` is the directory of `bimetal.toml`, and `target` Cargo's
/// target directory. A listed path that the sources no longer generate,
/// such as one of the old package after a rename, is stale, and publication
/// deletes it only if it is Bimetal's file, so an entry that a bad merge or a
/// hand edit added never deletes another file.
pub(crate) fn check_records(root: &Path, target: &Path, ownership: &GeneratedState) -> Result<()> {
    let mut problems = Vec::new();
    for (host, record) in ownership.records() {
        let table = host.name();
        if let Err(error) = paths::project(root, target, &record.project) {
            problems.push(format!(
                "`project` of `[generated.{table}]` cannot be a host's project: {error}"
            ));
        }
        for value in &record.files {
            let inside = paths::parse(value)
                .is_ok_and(|path| !path.components().any(|part| part == Component::ParentDir));
            if !inside {
                problems.push(format!(
                    "`{value}` in `files` of `[generated.{table}]` is outside the project"
                ));
            }
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    bail!(
        "the [generated] tables of bimetal.toml name paths where Bimetal generates no files, so it will not replace or delete them. Remove these entries, or delete the `[generated]` tables and run `bimetal build`:{}",
        problems::list(&problems)
    )
}

/// Return whether a path is a file, and not a symlink.
fn is_regular_file(path: &Path) -> Result<bool> {
    let metadata = scratch::optional(fs::symlink_metadata(path))?;
    Ok(metadata.is_some_and(|metadata| metadata.is_file()))
}

/// Return whether Bimetal generated a file: a text file with its banner near
/// the top, where a merge conflict can push it, or the Wasm module, which
/// has no banner, by its name.
fn is_generated(path: &Path) -> Result<bool> {
    use std::io::Read;

    if path
        .file_name()
        .is_some_and(|name| name == typescript::WASM_MODULE)
    {
        return Ok(true);
    }
    let mut head = Vec::new();
    File::open(path)
        .and_then(|file| file.take(1024).read_to_end(&mut head))
        .with_context(|| format!("failed to read {}", path.display()))?;
    let banner = GENERATED_BANNER.as_bytes();
    Ok(head.windows(banner.len()).any(|window| window == banner))
}

/// Remove the generated directories that deleting stale files left empty,
/// such as the old package after a rename, and their parents inside the
/// hosts' projects, but not the projects themselves.
fn cleanup_empty_directories<'file>(stale: impl Iterator<Item = &'file OwnedFile>) -> Result<()> {
    let mut directories = BTreeSet::new();
    for file in stale {
        let inside = file
            .relative
            .ancestors()
            .skip(1)
            .filter(|directory| !directory.as_os_str().is_empty());
        directories.extend(inside.map(|directory| file.project.join(directory)));
    }
    let mut directories: Vec<PathBuf> = directories.into_iter().collect();
    directories.sort_by_key(|path| Reverse(path.components().count()));
    for path in directories {
        if path.is_dir() {
            remove_if_empty(&path)?;
        }
    }
    Ok(())
}

/// Remove a directory that holds nothing, or nothing but the bytecode that
/// Python cached for the modules that were in it.
fn remove_if_empty(directory: &Path) -> Result<()> {
    let entries: Vec<DirEntry> = fs::read_dir(directory)?.collect::<io::Result<_>>()?;
    for entry in &entries {
        if entry.file_name() != "__pycache__" || !entry.file_type()?.is_dir() {
            return Ok(());
        }
    }
    for entry in entries {
        fs::remove_dir_all(entry.path())?;
    }
    fs::remove_dir(directory)?;
    Ok(())
}

/// Return the files of a staged tree, relative to it, rejecting symlinks.
fn staged_files(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut files = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .with_context(|| format!("failed to read {}", directory.display()))?;
        for entry in entries {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!("generated output contains a symlink: {}", path.display());
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                files.insert(path.strip_prefix(root)?.to_path_buf());
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::config::CONFIG_FILE;
    use crate::scaffold;

    fn sources() -> Sources {
        Sources {
            fingerprint: "one".to_owned(),
            inputs: vec!["src/data.txt".to_owned()],
        }
    }

    /// A new application directory in a temporary one, which holds the
    /// projects outside it, by its canonical path, as `bimetal.toml` resolves
    /// it.
    fn application() -> (TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = paths::canonical(directory.path()).unwrap().join("app");
        fs::create_dir(&root).unwrap();
        (directory, root)
    }

    /// Return the package of `host` in `project`, relative to `root`.
    fn package(root: &Path, host: Host, project: &str) -> HostPackage {
        HostPackage {
            host,
            package: "pkg".to_owned(),
            project: paths::normalized(&root.join(project)),
        }
    }

    /// The hosts of a new application: Python and TypeScript in their
    /// default projects.
    fn hosts(root: &Path) -> Vec<HostPackage> {
        vec![
            package(root, Host::Python, "python"),
            package(root, Host::TypeScript, "typescript"),
        ]
    }

    /// Return the configuration in `root`, which starts as `bimetal new`
    /// writes it.
    fn config(root: &Path) -> Config {
        let path = root.join(CONFIG_FILE);
        if !path.exists() {
            fs::write(&path, scaffold::configuration("my-app", "my_app")).unwrap();
        }
        Config::read(&path).unwrap()
    }

    /// Return the text of a generated file, with Bimetal's banner.
    fn with_banner(text: &str) -> String {
        format!("# {GENERATED_BANNER}\n{text}\n")
    }

    /// Write these files, by their paths in `<host>/`, into a new staging
    /// tree.
    fn stage(files: &[(&str, &str)]) -> TempDir {
        let staged = tempfile::tempdir().unwrap();
        for host in [Host::Python, Host::TypeScript] {
            fs::create_dir_all(staged.path().join(host.name())).unwrap();
        }
        for (path, source) in files {
            scratch::write(&staged.path().join(path), source).unwrap();
        }
        staged
    }

    /// Write files into `root`, as the user does.
    fn write(root: &Path, files: &[(&str, &str)]) {
        for (path, source) in files {
            scratch::write(&root.join(path), source).unwrap();
        }
    }

    /// Stage these files, and publish them into the projects of `hosts`.
    fn publish_to(root: &Path, hosts: &[HostPackage], files: &[(&str, &str)]) -> Result<()> {
        let staged = stage(files);
        let hosts: Vec<&HostPackage> = hosts.iter().collect();
        reconcile_generated_files(
            staged.path(),
            &hosts,
            &sources(),
            &HostDependencies::default(),
            &config(root),
        )
        .map(drop)
    }

    /// Stage these files, and publish them into the default projects.
    fn publish(root: &Path, files: &[(&str, &str)]) -> Result<()> {
        publish_to(root, &hosts(root), files)
    }

    /// Compare the projects of `hosts` with what a build that generates
    /// these files would publish.
    fn check_in(root: &Path, hosts: &[HostPackage], files: &[(&str, &str)]) -> Result<()> {
        let staged = stage(files);
        let hosts: Vec<&HostPackage> = hosts.iter().collect();
        check_generated_files(
            staged.path(),
            &hosts,
            &sources(),
            &HostDependencies::default(),
            &config(root),
        )
    }

    /// Compare the default projects with what a build that generates these
    /// files would publish.
    fn check(root: &Path, files: &[(&str, &str)]) -> Result<()> {
        check_in(root, &hosts(root), files)
    }

    fn read(root: &Path, path: &str) -> String {
        fs::read_to_string(root.join(path)).unwrap()
    }

    #[test]
    fn a_rename_removes_the_old_generated_directories() {
        let (_directory, root) = application();
        publish(
            &root,
            &[
                ("python/old/__init__.py", &with_banner("init")),
                ("python/old/stats/api.py", &with_banner("stats")),
                ("python/old/native.pyi", &with_banner("stub")),
                ("typescript/old/api.js", &with_banner("api")),
                ("typescript/old/native/native_bg.wasm", "wasm"),
            ],
        )
        .unwrap();
        write(
            &root,
            &[
                // The extension that the build installed beside its stub.
                ("python/old/native.abi3.so", "native"),
                ("python/old/__pycache__/api.pyc", "cache"),
                ("python/pyproject.toml", "manifest"),
            ],
        );

        publish(
            &root,
            &[
                ("python/new/__init__.py", &with_banner("init")),
                ("typescript/new/api.js", &with_banner("api")),
            ],
        )
        .unwrap();

        assert!(!root.join("python/old").exists());
        assert!(!root.join("typescript/old").exists());
        assert!(root.join("python/new/__init__.py").is_file());
        assert!(root.join("python/pyproject.toml").is_file());
    }

    #[test]
    fn the_installed_extension_beside_the_stub_stays() {
        let (_directory, root) = application();
        let stub = with_banner("stub");
        let generated = [("python/pkg/native.pyi", stub.as_str())];
        publish(&root, &generated).unwrap();
        write(&root, &[("python/pkg/native.abi3.so", "native")]);

        publish(&root, &generated).unwrap();

        assert_eq!(read(&root, "python/pkg/native.abi3.so"), "native");
        check(&root, &generated).unwrap();
        fs::remove_file(root.join("python/pkg/native.abi3.so")).unwrap();
        check(&root, &generated).unwrap();
    }

    #[test]
    fn the_installed_addon_stays_until_the_package_loads_wasm() {
        let (_directory, root) = application();
        let runtime = with_banner("runtime");
        let node = [("typescript/pkg/runtime.js", runtime.as_str())];
        publish(&root, &node).unwrap();
        write(&root, &[("typescript/pkg/native/native.node", "addon")]);

        publish(&root, &node).unwrap();
        assert_eq!(read(&root, "typescript/pkg/native/native.node"), "addon");
        check(&root, &node).unwrap();

        // The Wasm backend replaces the addon with its module.
        let wasm = [
            ("typescript/pkg/runtime.js", runtime.as_str()),
            ("typescript/pkg/native/native_bg.wasm", "wasm"),
        ];
        publish(&root, &wasm).unwrap();
        assert!(!root.join("typescript/pkg/native/native.node").exists());
        assert_eq!(read(&root, "typescript/pkg/native/native_bg.wasm"), "wasm");
    }

    #[test]
    fn stale_files_without_the_banner_stay() {
        let (_directory, root) = application();
        publish(
            &root,
            &[
                ("python/pkg/api.py", &with_banner("api")),
                ("python/pkg/notes.py", &with_banner("notes")),
                ("typescript/pkg/native/native_bg.wasm", "wasm"),
            ],
        )
        .unwrap();
        // The user took the file over, and the user's own Wasm module took
        // the place of the generated one.
        fs::write(root.join("python/pkg/notes.py"), "mine\n").unwrap();
        fs::remove_file(root.join("typescript/pkg/native/native_bg.wasm")).unwrap();
        write(&root, &[("typescript/pkg/native/other.wasm", "mine")]);
        let mut published = config(&root).generated;
        published
            .typescript
            .as_mut()
            .unwrap()
            .files
            .push("pkg/native/other.wasm".to_owned());
        write_ownership(&config(&root), &published).unwrap();

        publish(&root, &[("python/pkg/api.py", &with_banner("api"))]).unwrap();

        assert_eq!(read(&root, "python/pkg/notes.py"), "mine\n");
        assert!(root.join("typescript/pkg/native/other.wasm").is_file());
        let published = config(&root).generated;
        assert_eq!(published.python.unwrap().files, ["pkg/api.py"]);
        assert!(published.typescript.unwrap().files.is_empty());
    }

    #[test]
    fn records_list_files_relative_to_their_projects() {
        let (_directory, root) = application();
        let staged = stage(&[
            ("python/pkg/api.py", "python\n"),
            ("python/pkg/native.pyi", "stub\n"),
            ("typescript/pkg/api.ts", "typescript\n"),
            ("typescript/pkg/native/native_bg.wasm", "wasm\n"),
        ]);
        let python = package(&root, Host::Python, "python");
        let typescript = package(&root, Host::TypeScript, "../web");

        let plan = GeneratedPlan::read(
            staged.path(),
            &root,
            &[&python, &typescript],
            &sources(),
            &HostDependencies::default(),
        )
        .unwrap();

        let python = plan.ownership.python.unwrap();
        assert_eq!(python.project, "python");
        assert_eq!(python.files, ["pkg/api.py", "pkg/native.pyi"]);
        let typescript = plan.ownership.typescript.unwrap();
        assert_eq!(typescript.project, "../web");
        assert_eq!(
            typescript.files,
            ["pkg/api.ts", "pkg/native/native_bg.wasm"]
        );
        assert!(
            plan.files
                .contains_key(&root.parent().unwrap().join("web/pkg/api.ts"))
        );
    }

    #[test]
    fn reconciliation_preserves_user_files_and_removes_stale_generated_files() {
        let (_directory, root) = application();
        let user = [
            ("python/pyproject.toml", "python manifest\n"),
            ("python/pkg/__init__.py", "python entrypoint\n"),
            ("typescript/package.json", "typescript manifest\n"),
            ("typescript/tsconfig.json", "typescript config\n"),
            ("typescript/pkg/index.ts", "typescript entrypoint\n"),
        ];
        write(&root, &user);
        publish(&root, &[("python/pkg/api.py", &with_banner("first"))]).unwrap();
        write(&root, &[("python/pkg/custom.py", "user\n")]);

        let second = [("python/pkg/models.py", "second\n")];
        publish(&root, &second).unwrap();

        assert!(!root.join("python/pkg/api.py").exists());
        assert_eq!(read(&root, "python/pkg/models.py"), "second\n");
        assert_eq!(read(&root, "python/pkg/custom.py"), "user\n");
        for (path, source) in user {
            assert_eq!(read(&root, path), source);
        }
        check(&root, &second).unwrap();
        let generated = config(&root).generated;
        assert_eq!(generated.inputs, ["src/data.txt"]);
        assert_eq!(generated.source_fingerprint, "one");
        assert_eq!(generated.python.unwrap().files, ["pkg/models.py"]);
    }

    #[test]
    fn reconciliation_rejects_user_owned_collisions_before_mutating() {
        for (user, generated, collision) in [
            (
                "typescript/pkg/api.ts",
                "typescript/pkg/api.ts",
                "user-owned file",
            ),
            ("typescript/pkg", "typescript/pkg/api.ts", "user-owned path"),
            ("typescript", "typescript/pkg/api.ts", "user-owned path"),
        ] {
            let (_directory, root) = application();
            write(&root, &[(user, "user\n")]);
            config(&root);
            let before = fs::read(root.join(CONFIG_FILE)).unwrap();

            let error = publish(
                &root,
                &[
                    ("python/pkg/api.py", "generated\n"),
                    (generated, "generated\n"),
                ],
            )
            .unwrap_err()
            .to_string();

            assert!(error.contains(collision), "{error}");
            assert_eq!(read(&root, user), "user\n");
            assert!(!root.join("python/pkg/api.py").exists());
            assert_eq!(fs::read(root.join(CONFIG_FILE)).unwrap(), before);
        }
    }

    #[test]
    fn reconciliation_adopts_generated_files_that_bimetal_toml_lost() {
        let (_directory, root) = application();
        let banner = format!("// {GENERATED_BANNER}\n");
        let conflicted = format!("<<<<<<< HEAD\n{banner}old\n=======\n{banner}other\n>>>>>>> b\n");
        write(
            &root,
            &[
                ("typescript/pkg/api.js", &conflicted),
                ("typescript/pkg/native/native_bg.wasm", "old\n"),
            ],
        );

        publish(
            &root,
            &[
                ("typescript/pkg/api.js", "new\n"),
                ("typescript/pkg/native/native_bg.wasm", "new\n"),
            ],
        )
        .unwrap();

        for path in [
            "typescript/pkg/api.js",
            "typescript/pkg/native/native_bg.wasm",
        ] {
            assert_eq!(read(&root, path), "new\n");
        }
    }

    #[test]
    fn a_moved_project_leaves_no_old_files_and_keeps_the_users_new_ones() {
        let (_directory, root) = application();
        publish(
            &root,
            &[
                ("python/pkg/api.py", &with_banner("api")),
                ("python/pkg/native.pyi", &with_banner("stub")),
            ],
        )
        .unwrap();
        write(
            &root,
            &[
                ("python/pkg/native.abi3.so", "native"),
                ("python/pyproject.toml", "manifest"),
                ("moved/pkg/api.py", "the user's own\n"),
            ],
        );
        let moved = [
            package(&root, Host::Python, "moved"),
            package(&root, Host::TypeScript, "typescript"),
        ];
        let generated = [
            ("python/pkg/api.py", "api\n"),
            ("python/pkg/native.pyi", "stub\n"),
        ];

        // The record lists `pkg/api.py`, but in the old project.
        let error = publish_to(&root, &moved, &generated)
            .unwrap_err()
            .to_string();
        assert!(error.contains("collides with a user-owned file"), "{error}");
        assert_eq!(read(&root, "moved/pkg/api.py"), "the user's own\n");

        fs::remove_file(root.join("moved/pkg/api.py")).unwrap();
        publish_to(&root, &moved, &generated).unwrap();

        assert!(!root.join("python/pkg").exists());
        assert!(root.join("python/pyproject.toml").is_file());
        assert_eq!(read(&root, "moved/pkg/api.py"), "api\n");
        let record = config(&root).generated.python.unwrap();
        assert_eq!(record.project, "moved");
        check_in(&root, &moved, &generated).unwrap();
    }

    #[test]
    fn a_disabled_host_loses_its_files_and_its_record() {
        let (_directory, root) = application();
        publish(
            &root,
            &[
                ("python/pkg/api.py", &with_banner("api")),
                ("typescript/pkg/api.js", &with_banner("api")),
                ("typescript/pkg/native/native_bg.wasm", "wasm"),
            ],
        )
        .unwrap();
        write(&root, &[("typescript/package.json", "manifest")]);
        let python = [package(&root, Host::Python, "python")];
        let generated = [("python/pkg/api.py", "api\n")];

        let error = check_in(&root, &python, &generated)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("\n  stale: typescript/pkg/api.js"),
            "{error}"
        );

        publish_to(&root, &python, &generated).unwrap();

        assert!(!root.join("typescript/pkg").exists());
        assert!(root.join("typescript/package.json").is_file());
        let generated_state = config(&root).generated;
        assert_eq!(generated_state.typescript, None);
        check_in(&root, &python, &generated).unwrap();
    }

    #[test]
    fn an_interrupted_publication_is_reported_and_then_finished() {
        let (_directory, root) = application();
        publish(
            &root,
            &[
                ("python/pkg/api.py", &with_banner("first")),
                ("python/pkg/stale.py", &with_banner("stale")),
            ],
        )
        .unwrap();
        let api = with_banner("second");
        let index = with_banner("index");
        let second = [
            ("python/pkg/api.py", api.as_str()),
            ("typescript/pkg/index.js", index.as_str()),
        ];
        // A build that died after it installed one of the two files, before
        // it deleted the stale one and wrote bimetal.toml.
        write(&root, &[("typescript/pkg/index.js", &index)]);

        let error = check(&root, &second).unwrap_err().to_string();
        assert!(error.contains("\n  changed: python/pkg/api.py"), "{error}");
        assert!(error.contains("\n  stale: python/pkg/stale.py"), "{error}");

        publish(&root, &second).unwrap();
        check(&root, &second).unwrap();
        assert!(!root.join("python/pkg/stale.py").exists());
    }

    #[test]
    fn records_stay_in_the_hosts_projects() {
        let root = Path::new("/work/app");
        let target = Path::new("/work/app/target");
        let record = |project: &str, files: &[&str]| {
            Some(HostRecord {
                project: project.to_owned(),
                files: files.iter().map(|file| (*file).to_owned()).collect(),
                dependencies: Vec::new(),
            })
        };
        let mut ownership = GeneratedState {
            // A stale entry, such as one of the old package after a rename.
            python: record("python", &["pkg/api.py", "old/api.py"]),
            typescript: record("../web", &["pkg/api.js"]),
            ..GeneratedState::default()
        };
        check_records(root, target, &ownership).unwrap();

        ownership.python = record("python", &["pkg/api.py", "../notes.txt", ""]);
        ownership.typescript = record("", &["typescript/pkg/api.js"]);
        let error = check_records(root, target, &ownership)
            .unwrap_err()
            .to_string();

        for problem in [
            "`../notes.txt` in `files` of `[generated.python]` is outside the project",
            "`` in `files` of `[generated.python]` is outside the project",
            "`project` of `[generated.typescript]` cannot be a host's project: `` is not a relative path",
        ] {
            assert!(error.contains(&format!("\n  - {problem}")), "{error}");
        }
        assert!(!error.contains("pkg/api.py"), "{error}");

        ownership.python = record("target/python", &["pkg/api.py"]);
        let error = check_records(root, target, &ownership)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "`project` of `[generated.python]` cannot be a host's project: `target/python` is inside Cargo's target directory"
            ),
            "{error}"
        );
    }

    #[test]
    fn check_detects_changed_missing_and_stale_files_but_ignores_user_files() {
        let (_directory, root) = application();
        let generated = [("python/pkg/api.py", "generated\n")];
        publish(&root, &generated).unwrap();
        write(&root, &[("python/pkg/custom.py", "user\n")]);
        check(&root, &generated).unwrap();

        fs::write(root.join("python/pkg/api.py"), "changed\n").unwrap();
        let error = check(&root, &generated).unwrap_err().to_string();
        assert!(error.contains("\n  changed: python/pkg/api.py"), "{error}");

        fs::remove_file(root.join("python/pkg/api.py")).unwrap();
        let error = check(&root, &generated).unwrap_err().to_string();
        assert!(error.contains("\n  missing: python/pkg/api.py"), "{error}");

        publish(
            &root,
            &[
                ("python/pkg/api.py", "generated\n"),
                ("python/pkg/stale.py", "stale\n"),
            ],
        )
        .unwrap();
        let error = check(&root, &generated).unwrap_err().to_string();
        assert!(error.contains("\n  stale: python/pkg/stale.py"), "{error}");
    }

    #[test]
    fn check_names_files_relative_to_bimetal_toml() {
        let (_directory, root) = application();
        let hosts = [package(&root, Host::TypeScript, "../web")];
        let generated = [("typescript/pkg/api.js", "generated\n")];
        publish_to(&root, &hosts, &generated).unwrap();

        fs::write(root.parent().unwrap().join("web/pkg/api.js"), "changed\n").unwrap();
        let error = check_in(&root, &hosts, &generated).unwrap_err().to_string();

        assert!(error.contains("\n  changed: ../web/pkg/api.js"), "{error}");
    }
}
