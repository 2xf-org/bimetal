//! The resolved inputs of one command: `bimetal.toml` and Cargo metadata.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cargo::{self, ConfigFiles, Resolution, Toolchain};
use crate::config::{Backend, Config, Host, HostPackage, Namespaces};
use crate::namespaces::Folders;
use crate::output::{self, Platform};
use crate::paths;
use crate::python;

use super::compile::CompileKind;
use super::packages::{self, BimetalDependency, LocalPackage, SourceRoot};
use super::validation;

/// Fully resolved inputs for one CLI invocation.
///
/// The snapshot is constructed once from `bimetal.toml` and Cargo metadata.
/// Commands that perform a long-running build re-read the configuration before
/// publication so a concurrent edit cannot produce output from mixed inputs.
#[derive(Debug, Clone)]
pub(crate) struct Project {
    /// Directory containing `bimetal.toml` and the primary Cargo package.
    pub(crate) root: PathBuf,
    /// Cargo workspace root used for source fingerprinting and path remapping.
    pub(crate) workspace_root: PathBuf,
    /// Cargo target directory shared by the link crate builds.
    pub(crate) target_directory: PathBuf,
    /// Ordered application packages linked into the link crate.
    pub(super) linked_packages: Vec<LinkedPackage>,
    /// The folders that name the application packages, when
    /// `[application] namespaces` is `"folders"`.
    pub(super) folders: Option<Folders>,
    /// Local packages whose sources can affect generated contracts or runtimes.
    pub(crate) source_roots: Vec<SourceRoot>,
    /// Every local package of the workspace's dependency graph.
    pub(super) local_packages: Vec<LocalPackage>,
    /// The `bimetal` runtime that every linked package uses.
    pub(super) bimetal_dependency: BimetalDependency,
    /// Public application/package name.
    pub(crate) package_name: String,
    /// Version inherited from the primary Cargo package.
    pub(crate) package_version: String,
    /// The Python package, when `[python]` turns it on.
    pub(crate) python: Option<HostPackage>,
    /// The npm package, when `[typescript]` turns it on.
    pub(crate) typescript: Option<HostPackage>,
    /// How the npm package runs the Rust code.
    pub(crate) backend: Backend,
    /// Whether Cargo may update `Cargo.lock` and use the network.
    pub(super) resolution: Resolution,
    /// The Cargo configuration files that the builds read.
    pub(crate) configuration: ConfigFiles,
    /// The rustc that the builds run.
    pub(crate) toolchain: Toolchain,
    /// Configuration snapshot used to construct this project.
    pub(super) config: Config,
}

/// The library that a native build produces, and where Bimetal installs it.
#[derive(Debug, Clone)]
pub(super) struct NativeArtifact {
    /// The build that produces it.
    pub(super) kind: CompileKind,
    /// Where it goes.
    pub(super) destination: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct LinkedPackage {
    pub(super) name: String,
    pub(super) manifest: PathBuf,
}

impl Project {
    /// Resolve configuration and Cargo metadata for the package at `path`.
    pub(crate) fn read(path: &Path, resolution: Resolution) -> Result<Self> {
        let config = Config::read(path)?;
        let requested_manifest = config.root().join("Cargo.toml");
        let requested_manifest = paths::canonical(&requested_manifest)
            .with_context(|| format!("cannot find Cargo.toml beside {}", config.path.display()))?;
        let metadata = cargo::metadata(&requested_manifest, config.root(), resolution)?;
        let settings = &config.settings;
        let selected = packages::select_application_packages(
            &metadata,
            &requested_manifest,
            &settings.application.additional_packages,
        )?;
        let primary = selected
            .first()
            .copied()
            .context("a Bimetal application must select a primary Cargo package")?;
        let bimetal_dependency = packages::resolve_bimetal_dependency(&metadata, &selected)?;
        let folders = match settings.application.namespaces {
            Namespaces::Packages => None,
            Namespaces::Folders => Some(packages::folders(&metadata.workspace_root, &selected)?),
        };
        let source_roots = packages::resolve_source_roots(&metadata, &selected)?;
        let local_packages = packages::local_packages(&metadata);

        let package_name = settings
            .application
            .name
            .clone()
            .unwrap_or_else(|| primary.name.clone());
        validation::validate_application_name(&package_name)?;
        let package_version = primary.version.clone();
        let root = config.root().to_path_buf();
        let target_directory = metadata.target_directory.clone();
        // A host's package defaults to a spelling of the application name.
        let underscored = python::python_package_name(&package_name);
        let host = |host, package: Option<&str>, project: Option<&str>, default: &str| {
            host_package(
                host,
                package.unwrap_or(default),
                project,
                &root,
                &target_directory,
            )
        };
        let python = settings
            .python
            .as_ref()
            .map(|python| {
                let package = python.package.as_deref();
                let project = python.project.as_deref();
                host(Host::Python, package, project, &underscored)
            })
            .transpose()?;
        let typescript = settings
            .typescript
            .as_ref()
            .map(|typescript| {
                let project = typescript.project.as_deref();
                let mut package = host(Host::TypeScript, None, project, &package_name)?;
                if let Some(name) = npm_name(&package.project) {
                    validation::validate_typescript_package(&name)
                        .context("invalid `name` in the npm project's package.json")?;
                    package.package = name;
                }
                Ok::<_, anyhow::Error>(package)
            })
            .transpose()?;
        let enabled: Vec<&HostPackage> = [&python, &typescript].into_iter().flatten().collect();
        check_separate_projects(&enabled)?;
        let backend = settings
            .typescript
            .as_ref()
            .map(|typescript| typescript.backend)
            .unwrap_or_default();

        let configuration = ConfigFiles::discover(&root, cargo::home(&root).as_deref())?;
        let toolchain = Toolchain::query(&root, &configuration)?;
        let workspace_root = metadata.workspace_root.clone();
        let linked_packages: Vec<LinkedPackage> = selected
            .iter()
            .map(|package| LinkedPackage {
                name: package.name.clone(),
                manifest: package.manifest_path.clone(),
            })
            .collect();

        Ok(Self {
            root,
            workspace_root,
            target_directory,
            linked_packages,
            folders,
            source_roots,
            local_packages,
            bimetal_dependency,
            package_name,
            package_version,
            python,
            typescript,
            backend,
            resolution,
            configuration,
            toolchain,
            config,
        })
    }

    /// Return the canonical path of `bimetal.toml`.
    #[must_use]
    pub(crate) fn config_path(&self) -> &Path {
        &self.config.path
    }

    /// Return the packages of the enabled hosts.
    pub(crate) fn hosts(&self) -> impl Iterator<Item = &HostPackage> {
        [&self.python, &self.typescript].into_iter().flatten()
    }

    /// Return whether a host runs the application as a WebAssembly module.
    #[must_use]
    pub(super) fn builds_wasm(&self) -> bool {
        self.typescript.is_some() && self.backend == Backend::Wasm
    }

    /// Return the native builds that the enabled hosts need: the Python
    /// extension and the Node.js addon.
    #[must_use]
    pub(super) fn native_kinds(&self) -> Vec<CompileKind> {
        self.native_artifacts()
            .iter()
            .map(|artifact| artifact.kind)
            .collect()
    }

    /// Return the file name that the library of a native build is installed
    /// as, which macOS records in it.
    #[must_use]
    pub(super) fn shared_library_name(&self, kind: CompileKind) -> Option<String> {
        let artifacts = self.native_artifacts();
        let library = artifacts.iter().find(|artifact| artifact.kind == kind)?;
        Some(library.destination.file_name()?.to_str()?.to_owned())
    }

    /// Return every native file that the enabled hosts install for this
    /// platform. Git ignores them, so each platform builds its own.
    #[must_use]
    pub(super) fn native_artifacts(&self) -> Vec<NativeArtifact> {
        let artifact = |package: &HostPackage, kind, directory: &str| {
            let relative = output::native_file(package.host, directory, Platform::CURRENT);
            NativeArtifact {
                kind,
                destination: package.project.join(relative),
            }
        };
        let python = self.python.as_ref().map(|python| {
            let directory = python::python_package_directory(&python.package);
            artifact(python, CompileKind::Python, &directory)
        });
        let addon = self
            .typescript
            .as_ref()
            .filter(|_| self.backend == Backend::Node)
            .map(|typescript| artifact(typescript, CompileKind::Native, &self.package_name));
        python.into_iter().chain(addon).collect()
    }
}

/// Return the package of `host`, after checking its name, in the project
/// that its table names or in the host's default one, relative to `root`,
/// the directory of `bimetal.toml`, and outside `target`, Cargo's target
/// directory.
fn host_package(
    host: Host,
    package: &str,
    project: Option<&str>,
    root: &Path,
    target: &Path,
) -> Result<HostPackage> {
    match host {
        Host::Python => validation::validate_python_package(package)?,
        Host::TypeScript => validation::validate_typescript_package(package)?,
    }
    let value = project.unwrap_or(host.default_project());
    let project = paths::project(root, target, value).with_context(|| {
        format!(
            "invalid `project` in the [{}] table of bimetal.toml",
            host.name()
        )
    })?;
    Ok(HostPackage {
        host,
        package: package.to_owned(),
        project,
    })
}

/// The npm package name that the `package.json` of `project` declares, if
/// any. The validation of `package.json` reports a missing or unreadable file.
fn npm_name(project: &Path) -> Option<String> {
    let source = std::fs::read_to_string(project.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&source).ok()?;
    manifest["name"].as_str().map(str::to_owned)
}

/// Reject hosts whose projects are the same folder or one inside the other,
/// where one host's generated files would mix with the other's.
fn check_separate_projects(hosts: &[&HostPackage]) -> Result<()> {
    for (index, first) in hosts.iter().enumerate() {
        for second in &hosts[index + 1..] {
            if first.project.starts_with(&second.project)
                || second.project.starts_with(&first.project)
            {
                bail!(
                    "the projects of [{}] and [{}] in bimetal.toml overlap, at {} and {}; give each host a folder of its own",
                    first.host.name(),
                    second.host.name(),
                    first.project.display(),
                    second.project.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(host: Host, project: &str) -> HostPackage {
        HostPackage {
            host,
            package: "app".to_owned(),
            project: PathBuf::from(project),
        }
    }

    #[test]
    fn each_host_has_a_folder_of_its_own() {
        let python = package(Host::Python, "/app/python");
        let typescript = package(Host::TypeScript, "/app/typescript");
        check_separate_projects(&[&python, &typescript]).unwrap();
        check_separate_projects(&[&python]).unwrap();
        // A shared prefix of the name is not a shared folder.
        let near = package(Host::TypeScript, "/app/python-ts");
        check_separate_projects(&[&python, &near]).unwrap();

        for (first, second) in [
            ("/app/packages", "/app/packages"),
            ("/app/packages", "/app/packages/web"),
            ("/app/packages/web", "/app/packages"),
        ] {
            let error = check_separate_projects(&[
                &package(Host::Python, first),
                &package(Host::TypeScript, second),
            ])
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("the projects of [python] and [typescript] in bimetal.toml overlap"),
                "{error}"
            );
        }
    }
}
