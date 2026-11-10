//! Selecting the application's Cargo packages, the sources they depend on,
//! the `bimetal` runtime they use, and the folders that can name them.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cargo::{Metadata as CargoMetadata, Package as CargoPackage};
use crate::literal;
use crate::namespaces::Folders;
use crate::paths;

/// Where the `bimetal` runtime of an application comes from.
///
/// The link crate must declare the same source that the application resolves, or
/// Cargo would link a second copy of the runtime with its own export registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BimetalDependency {
    /// The release of this command's version on crates.io.
    Registry,
    /// A local checkout, as the directory that Cargo reports for it. Symlinks
    /// on the way stay, because Cargo identifies a path package by the path
    /// it reached the package through.
    Path(PathBuf),
    /// A git repository. Cargo.lock pins the commit.
    Git {
        /// Repository URL as Cargo resolved it.
        url: String,
        /// The `branch`, `tag`, or `rev` requirement, if any.
        reference: Option<GitReference>,
    },
}

/// The `branch`, `tag`, or `rev` key of a git dependency, with its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitReference {
    /// `branch`, `tag`, or `rev`.
    pub(crate) key: String,
    /// The branch name, tag name, or commit.
    pub(crate) value: String,
}

impl BimetalDependency {
    /// Render the dependency as the value of a Cargo.toml `bimetal = ...` entry.
    #[must_use]
    pub(crate) fn manifest_value(&self) -> String {
        match self {
            // The command refuses a runtime of another version, so the
            // requirement is exact, and `cargo update` cannot move it.
            Self::Registry => literal::quoted(concat!("=", env!("CARGO_PKG_VERSION"))),
            Self::Path(path) => {
                format!("{{ path = {} }}", literal::quoted(&path.to_string_lossy()))
            }
            Self::Git { url, reference } => match reference {
                Some(GitReference { key, value }) => format!(
                    "{{ git = {}, {key} = {} }}",
                    literal::quoted(url),
                    literal::quoted(value)
                ),
                None => format!("{{ git = {} }}", literal::quoted(url)),
            },
        }
    }
}

/// Stable identity and source directory for one fingerprinted local package.
#[derive(Debug, Clone)]
pub(crate) struct SourceRoot {
    /// The workspace-relative directory, or `name@version` outside the workspace.
    pub(crate) identity: String,
    /// The canonical package directory.
    pub(crate) path: PathBuf,
}

/// A package that Cargo builds from a local directory.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LocalPackage {
    /// The package name.
    pub(crate) name: String,
    /// The package directory as Cargo reports it, which is the path that
    /// rustc sees, symlinks included.
    pub(crate) directory: PathBuf,
}

/// Select and validate the primary and configured application packages.
pub(super) fn select_application_packages<'a>(
    metadata: &'a CargoMetadata,
    requested_manifest: &Path,
    additional_packages: &[String],
) -> Result<Vec<&'a CargoPackage>> {
    let primary = metadata
        .packages
        .iter()
        .find(|package| {
            paths::canonical(&package.manifest_path).is_ok_and(|path| path == requested_manifest)
        })
        .context("bimetal.toml must be beside a Cargo package manifest")?;
    if !metadata.workspace_members.contains(&primary.id) {
        bail!("the Cargo package beside bimetal.toml must be a workspace member");
    }

    let mut selected = vec![primary];
    for name in additional_packages {
        if selected.iter().any(|package| &package.name == name) {
            bail!("bimetal application package `{name}` is listed more than once");
        }
        let mut matches = metadata.packages.iter().filter(|package| {
            &package.name == name && metadata.workspace_members.contains(&package.id)
        });
        let package = matches.next().with_context(|| {
            format!("additional Bimetal package `{name}` is not a workspace member")
        })?;
        if matches.next().is_some() {
            bail!("additional Bimetal package name `{name}` is ambiguous");
        }
        selected.push(package);
    }
    for package in &selected {
        check_library_target(package)?;
    }
    Ok(selected)
}

/// Name the selected packages after their folders below `workspace_root`,
/// as Cargo reports both, symlinks included. The first is the application
/// package, whose declarations take the root namespace.
pub(super) fn folders(workspace_root: &Path, selected: &[&CargoPackage]) -> Result<Folders> {
    let mut packages = Vec::new();
    for package in selected {
        let folder = package
            .manifest_path
            .parent()
            .context("a Cargo manifest has no parent folder")?;
        packages.push((package.name.as_str(), folder));
    }
    let (application, additional) = packages
        .split_first()
        .context("a Bimetal application must select a primary Cargo package")?;
    Folders::new(workspace_root, *application, additional)
}

/// Require a package to have a Rust library target, which the link crate
/// links into the library of each host.
fn check_library_target(package: &CargoPackage) -> Result<()> {
    const NOT_LIBRARIES: [&str; 5] = ["bin", "example", "test", "bench", "custom-build"];

    let manifest = package.manifest_path.display();
    let Some(library) = package.targets.iter().find(|target| {
        !target
            .kind
            .iter()
            .any(|kind| NOT_LIBRARIES.contains(&kind.as_str()))
    }) else {
        bail!(
            "the Bimetal application package `{}` has no library target: add `src/lib.rs` beside {manifest}",
            package.name
        );
    };
    if let Some(kind) = library
        .crate_types
        .iter()
        .find(|kind| !matches!(kind.as_str(), "lib" | "rlib"))
    {
        bail!(
            "the Bimetal application package `{}` builds a `{kind}`, which Bimetal cannot link, because it builds the library of every host itself: set `[lib] crate-type` in {manifest} to `[\"lib\"]`, or remove it",
            package.name
        );
    }
    Ok(())
}

/// Resolve the local dependency closure that can affect generated output.
///
/// Application packages pull in the runtime, macros, and IR crates when they
/// are path dependencies. Unrelated workspace members, such as native
/// application shells, stay out of the freshness checks. The generator's own
/// sources are covered by `BIMETAL_GENERATOR_FINGERPRINT`.
pub(super) fn resolve_source_roots(
    metadata: &CargoMetadata,
    selected: &[&CargoPackage],
) -> Result<Vec<SourceRoot>> {
    let packages: BTreeMap<&str, &CargoPackage> = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect();
    let nodes: BTreeMap<&str, _> = metadata
        .resolve
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut pending: VecDeque<String> = selected.iter().map(|package| package.id.clone()).collect();
    let mut visited = BTreeSet::new();
    let mut roots = BTreeMap::new();
    while let Some(id) = pending.pop_front() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let package = packages
            .get(id.as_str())
            .with_context(|| format!("Cargo metadata omitted resolved package `{id}`"))?;
        if package.source.is_some() {
            continue;
        }
        let manifest = paths::canonical(&package.manifest_path)
            .with_context(|| format!("cannot resolve {}", package.manifest_path.display()))?;
        let path = manifest
            .parent()
            .context("local Cargo manifest has no parent")?
            .to_path_buf();
        let identity = match path.strip_prefix(&metadata.workspace_root) {
            Ok(relative) => paths::portable(relative)?,
            Err(_) => format!("{}@{}", package.name, package.version),
        };
        roots.insert(path.clone(), SourceRoot { identity, path });

        if let Some(node) = nodes.get(id.as_str()) {
            for dependency in &node.dependencies {
                let participates_in_build = dependency
                    .kinds
                    .iter()
                    .any(|kind| !matches!(kind.kind.as_deref(), Some("dev")));
                if participates_in_build
                    && packages
                        .get(dependency.package.as_str())
                        .is_some_and(|dependency| dependency.source.is_none())
                {
                    pending.push_back(dependency.package.clone());
                }
            }
        }
    }

    let mut roots: Vec<SourceRoot> = roots.into_values().collect();
    roots.sort_by(|left, right| left.identity.cmp(&right.identity));
    Ok(roots)
}

/// Return every local package of the workspace's dependency graph, sorted.
///
/// Decisions based on them are the same for every application of a
/// workspace, so the applications give shared dependencies the same flags.
#[must_use]
pub(super) fn local_packages(metadata: &CargoMetadata) -> Vec<LocalPackage> {
    let mut packages: Vec<LocalPackage> = metadata
        .packages
        .iter()
        .filter(|package| package.source.is_none())
        .filter_map(|package| {
            Some(LocalPackage {
                name: package.name.clone(),
                directory: package.manifest_path.parent()?.to_path_buf(),
            })
        })
        .collect();
    packages.sort();
    packages.dedup();
    packages
}

/// Require every application package to use the same direct `bimetal` package,
/// at the version of this CLI.
pub(super) fn resolve_bimetal_dependency(
    metadata: &CargoMetadata,
    selected: &[&CargoPackage],
) -> Result<BimetalDependency> {
    let mut identities = Vec::new();
    for package in selected {
        identities.push(direct_bimetal_id(metadata, package)?);
    }
    let identity = identities.iter().flatten().next();
    let dependency = identity
        .map(|identity| {
            let package = metadata
                .packages
                .iter()
                .find(|package| &package.id == identity)
                .context("Cargo metadata omitted the resolved Bimetal package")?;
            let cli_version = env!("CARGO_PKG_VERSION");
            if package.version != cli_version {
                bail!(
                    "the application uses the `bimetal` runtime {}, but this `bimetal` command is {cli_version}; install the command from the same Bimetal version",
                    package.version
                );
            }
            resolved_dependency(package)
        })
        .transpose()?;
    for (package, candidate) in selected.iter().zip(&identities) {
        let Some(candidate) = candidate else {
            let line = dependency.as_ref().map_or_else(
                || format!("`bimetal`, at version {},", env!("CARGO_PKG_VERSION")),
                |dependency| format!("`bimetal = {}`", dependency.manifest_value()),
            );
            bail!(
                "the Bimetal application package `{}` must depend directly on `bimetal`: add {line} to `[dependencies]` in {}",
                package.name,
                package.manifest_path.display()
            );
        };
        if Some(candidate) != identity {
            bail!("all Bimetal application packages must resolve the same `bimetal` package");
        }
    }
    dependency.context("a Bimetal application must select at least one Cargo package")
}

/// Resolve the package ID of a package's direct normal `bimetal` dependency,
/// or `None` when it has none.
fn direct_bimetal_id(metadata: &CargoMetadata, package: &CargoPackage) -> Result<Option<String>> {
    let node = metadata
        .resolve
        .nodes
        .iter()
        .find(|node| node.id == package.id)
        .context("Cargo metadata omitted an application package from its dependency graph")?;
    let mut matches = node
        .dependencies
        .iter()
        .filter(|dependency| dependency.kinds.iter().any(|kind| kind.kind.is_none()))
        .filter(|dependency| {
            metadata
                .packages
                .iter()
                .any(|package| package.id == dependency.package && package.name == "bimetal")
        })
        .map(|dependency| dependency.package.clone());
    let dependency = matches.next();
    if matches.next().is_some() {
        bail!(
            "the Bimetal application package `{}` resolves more than one `bimetal` package",
            package.name
        );
    }
    Ok(dependency)
}

/// The source that Cargo reports for a package from crates.io, whichever
/// protocol fetched it.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// Resolve where the selected `bimetal` package comes from.
fn resolved_dependency(package: &CargoPackage) -> Result<BimetalDependency> {
    let Some(source) = package.source.as_deref() else {
        // Canonicalizing would resolve a symlink that the application reaches
        // Bimetal through, and Cargo would see two `bimetal` packages at one version.
        return package
            .manifest_path
            .parent()
            .context("resolved Bimetal manifest has no parent")
            .map(|directory| BimetalDependency::Path(directory.to_path_buf()));
    };
    if source == CRATES_IO {
        return Ok(BimetalDependency::Registry);
    }
    // Cargo writes a git source as `git+<url>[?<kind>=<value>]#<commit>`.
    let location = source
        .strip_prefix("git+")
        .and_then(|location| location.split_once('#'))
        .map(|(location, _commit)| location)
        .with_context(|| {
            format!(
                "the `bimetal` runtime must come from crates.io, a path, or a git repository, not `{source}`"
            )
        })?;
    let (url, reference) = match location.split_once('?') {
        Some((url, reference)) => {
            let (key, value) = reference
                .split_once('=')
                .filter(|(key, _)| matches!(*key, "branch" | "tag" | "rev"))
                .with_context(|| format!("unsupported git reference in `{source}`"))?;
            let reference = GitReference {
                key: key.to_owned(),
                value: value.to_owned(),
            };
            (url, Some(reference))
        }
        None => (location, None),
    };
    Ok(BimetalDependency::Git {
        url: url.to_owned(),
        reference,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::cargo::{Dependency, DependencyKind, Node, Resolve, Target};

    fn package(name: &str, library: &[&str]) -> CargoPackage {
        let target = |kinds: &[&str]| Target {
            kind: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
            crate_types: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
        };
        CargoPackage {
            id: name.to_owned(),
            name: name.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            manifest_path: PathBuf::from(format!("/work/{name}/Cargo.toml")),
            source: None,
            targets: [target(&["bin"]), target(library)]
                .into_iter()
                .filter(|target| !target.kind.is_empty())
                .collect(),
        }
    }

    #[test]
    fn application_packages_are_rust_libraries() {
        check_library_target(&package("app", &["lib"])).expect("a library");
        check_library_target(&package("app", &["rlib"])).expect("an rlib");

        let message = |library: &[&str]| {
            check_library_target(&package("app", library))
                .expect_err("not a Rust library")
                .to_string()
        };
        let cdylib = message(&["cdylib", "rlib"]);
        assert!(
            cdylib.contains("`app` builds a `cdylib`, which Bimetal cannot link, because it builds the library of every host itself: set `[lib] crate-type` in /work/app/Cargo.toml"),
            "{cdylib}"
        );
        assert!(message(&["dylib"]).contains("builds a `dylib`"));
        assert!(message(&[]).contains("`app` has no library target"));
    }

    #[test]
    fn every_application_package_names_bimetal() {
        let packages = vec![package("app", &["lib"]), package("extra", &["lib"]), {
            let mut bimetal = package("bimetal", &["lib"]);
            bimetal.id = "path+file:///work/bimetal#1".to_owned();
            bimetal
        }];
        let node = |id: &str, dependencies: &[&str]| Node {
            id: id.to_owned(),
            dependencies: dependencies
                .iter()
                .map(|package| Dependency {
                    package: (*package).to_owned(),
                    kinds: vec![DependencyKind { kind: None }],
                })
                .collect(),
        };
        let metadata = CargoMetadata {
            workspace_members: vec!["app".to_owned(), "extra".to_owned()],
            workspace_root: PathBuf::from("/work"),
            target_directory: PathBuf::from("/work/target"),
            resolve: Resolve {
                nodes: vec![
                    node("app", &["path+file:///work/bimetal#1"]),
                    node("extra", &[]),
                ],
            },
            packages,
        };
        let selected = [&metadata.packages[0], &metadata.packages[1]];

        let error = resolve_bimetal_dependency(&metadata, &selected)
            .expect_err("`extra` lacks bimetal")
            .to_string();

        assert_eq!(
            error,
            "the Bimetal application package `extra` must depend directly on `bimetal`: add `bimetal = { path = \"/work/bimetal\" }` to `[dependencies]` in /work/extra/Cargo.toml"
        );
        assert_eq!(
            resolve_bimetal_dependency(&metadata, &selected[..1]).expect("`app` has bimetal"),
            BimetalDependency::Path(PathBuf::from("/work/bimetal"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_dependency_keeps_the_path_cargo_reports() {
        use std::os::unix::fs::symlink;

        let checkout =
            paths::canonical(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages"))
                .expect("canonical packages directory");
        let application = tempfile::tempdir().expect("temporary application");
        let linked = application.path().join("vendor");
        symlink(&checkout, &linked).expect("symlink to the Bimetal packages");
        let manifest = linked.join("runtime").join("Cargo.toml");
        let package: CargoPackage = serde_json::from_value(serde_json::json!({
            "id": "path+file:///workspace/packages/runtime#3.0.1",
            "name": "bimetal",
            "manifest_path": manifest,
            "source": null,
            "version": "3.0.1",
            "targets": [],
        }))
        .expect("deserialize package metadata");

        let dependency = resolved_dependency(&package).expect("resolve path dependency");

        // The link crate must name the package by the same path as the
        // application, or Cargo sees two `bimetal` packages at one version.
        assert_eq!(dependency, BimetalDependency::Path(linked.join("runtime")));
    }

    #[cfg(unix)]
    #[test]
    fn folders_keep_the_names_of_the_symlinks_that_cargo_reports() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("temporary workspace");
        let root = paths::canonical(workspace.path()).expect("canonical temporary workspace");
        let outside = tempfile::tempdir().expect("a package folder outside the workspace");
        fs::create_dir(root.join("packages")).expect("packages folder");
        let mut application = package("app", &["lib"]);
        application.manifest_path = root.join("app/Cargo.toml");
        // A glob member, such as one of `packages/*`, reached through a
        // symlink whose target is outside the workspace.
        let folders_through = |link: &str| {
            let linked = root.join("packages").join(link);
            symlink(outside.path(), &linked).expect("symlink to the package");
            let mut feeds = package("app-feeds", &["lib"]);
            feeds.manifest_path = linked.join("Cargo.toml");
            let folders = folders(&root, &[&application, &feeds]).expect("name the packages");
            fs::remove_file(&linked).expect("remove the symlink");
            folders.fingerprint_source()
        };

        let feeds = folders_through("feeds");
        let quotes = folders_through("quotes");

        assert!(feeds.contains("app-feeds\0packages/feeds\0"), "{feeds:?}");
        // Renaming the symlink changes no file, but renames the namespace.
        assert_ne!(feeds, quotes);
    }

    #[test]
    fn the_runtime_comes_from_crates_io_or_a_git_repository() {
        let package = |source: &str| CargoPackage {
            id: "bimetal".to_owned(),
            name: "bimetal".to_owned(),
            version: "1.0.0".to_owned(),
            manifest_path: PathBuf::from(
                "/cargo/git/checkouts/bimetal/packages/runtime/Cargo.toml",
            ),
            source: Some(source.to_owned()),
            targets: Vec::new(),
        };

        let tagged = resolved_dependency(&package(
            "git+ssh://git@example.com/team/bimetal.git?tag=v1.0.0#0123abcd",
        ))
        .expect("resolve tagged git dependency");
        let default_branch =
            resolved_dependency(&package("git+https://example.com/team/bimetal#0123abcd"))
                .expect("resolve default-branch git dependency");
        let registry = resolved_dependency(&package(CRATES_IO)).expect("resolve crates.io");
        let other_registry = resolved_dependency(&package("registry+https://example.com/index"))
            .expect_err("another registry")
            .to_string();

        assert_eq!(
            tagged.manifest_value(),
            r#"{ git = "ssh://git@example.com/team/bimetal.git", tag = "v1.0.0" }"#
        );
        assert_eq!(
            default_branch.manifest_value(),
            r#"{ git = "https://example.com/team/bimetal" }"#
        );
        assert_eq!(registry, BimetalDependency::Registry);
        assert_eq!(
            registry.manifest_value(),
            concat!("\"=", env!("CARGO_PKG_VERSION"), "\"")
        );
        assert!(
            other_registry.contains("must come from crates.io"),
            "{other_registry}"
        );
    }

    #[test]
    fn source_roots_exclude_unrelated_workspace_members() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let workspace_root =
            paths::canonical(workspace.path()).expect("canonical temporary workspace");
        let package = |name: &str| {
            let root = workspace_root.join(name);
            fs::create_dir_all(&root).expect("package directory");
            let manifest = root.join("Cargo.toml");
            fs::write(&manifest, format!("[package]\nname = \"{name}\"\n"))
                .expect("package manifest");
            CargoPackage {
                id: name.to_owned(),
                name: name.to_owned(),
                version: "1.0.0".to_owned(),
                manifest_path: manifest,
                source: None,
                targets: Vec::new(),
            }
        };
        let packages = vec![
            package("application"),
            package("bimetal"),
            package(env!("CARGO_PKG_NAME")),
            package("unrelated-native-app"),
        ];
        let dependency = |package: &str| Dependency {
            package: package.to_owned(),
            kinds: vec![DependencyKind { kind: None }],
        };
        let metadata = CargoMetadata {
            workspace_members: packages.iter().map(|package| package.id.clone()).collect(),
            workspace_root: workspace_root.clone(),
            target_directory: workspace_root.join("target"),
            resolve: Resolve {
                nodes: vec![
                    Node {
                        id: "application".to_owned(),
                        dependencies: vec![dependency("bimetal")],
                    },
                    Node {
                        id: "bimetal".to_owned(),
                        dependencies: Vec::new(),
                    },
                    Node {
                        id: env!("CARGO_PKG_NAME").to_owned(),
                        dependencies: vec![dependency("bimetal")],
                    },
                    Node {
                        id: "unrelated-native-app".to_owned(),
                        dependencies: Vec::new(),
                    },
                ],
            },
            packages,
        };

        let roots = resolve_source_roots(&metadata, &[&metadata.packages[0]])
            .expect("resolve fingerprint roots");
        let identities: Vec<&str> = roots.iter().map(|root| root.identity.as_str()).collect();

        assert_eq!(identities, ["application", "bimetal"]);

        // Every application of the workspace sees the same local packages.
        let local = local_packages(&metadata);
        let names: Vec<&str> = local.iter().map(|package| package.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "application",
                "bimetal",
                env!("CARGO_PKG_NAME"),
                "unrelated-native-app"
            ]
        );
        assert_eq!(local[1].directory, workspace_root.join("bimetal"));
    }
}
