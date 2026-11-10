//! How the declarations of an application get their namespaces: from the
//! names of their Cargo packages, or from the packages' folders, as
//! `[application] namespaces` in `bimetal.toml` chooses.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use bimetal_ir::{CargoPackageId, Manifest, Namespace};

use crate::paths;
use crate::problems::{self, Subject};

/// Derives the namespace of each declaration of one contract.
#[derive(Debug, Clone)]
pub(crate) enum NamespaceRule {
    /// Each package's name, without the prefix that all of them share.
    Packages(bimetal_ir::NamespaceRule),
    /// Each package's folder below the deepest folder that holds all of them.
    Folders(Folders),
}

impl NamespaceRule {
    /// Return the rule that names the declarations of `manifest`: after
    /// `folders`, which `namespaces = "folders"` gives, or else after the
    /// names of the Cargo packages.
    ///
    /// # Errors
    ///
    /// Returns an error when the folders name the packages and one that
    /// declares something is not a package of the application.
    pub(crate) fn new(manifest: &Manifest, folders: Option<&Folders>) -> Result<Self> {
        match folders {
            Some(folders) => folders.rule(manifest),
            None => Ok(Self::Packages(manifest.namespace_rule())),
        }
    }

    /// Return the namespace of a declaration of `owner` in `rust_module`.
    ///
    /// # Panics
    ///
    /// Under the folder rule, when `owner` has no folder, which
    /// [`Folders::rule`] rules out for the declarations of its manifest.
    #[must_use]
    pub(crate) fn namespace(&self, owner: &CargoPackageId, rust_module: &str) -> Namespace {
        match self {
            Self::Packages(rule) => rule.namespace(owner, rust_module),
            Self::Folders(folders) => {
                let package = folders
                    .packages
                    .get(owner)
                    .expect("the folder rule names every package that declares something");
                Namespace::new(package.clone(), rust_module)
            }
        }
    }
}

/// The folders that name the packages of an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Folders {
    /// The deepest folder that holds every package, as the folders from the
    /// workspace root down to it.
    base: Vec<String>,
    /// The folders from `base` to each package, by Cargo package name; none
    /// for the application package.
    packages: BTreeMap<CargoPackageId, Vec<String>>,
}

impl Folders {
    /// Name each package after its folder below the deepest folder that
    /// holds them all, which lies in `workspace`, the folder of the Cargo
    /// workspace. `application` is the name and the folder of the
    /// application package, whose declarations take the root namespace, and
    /// `additional` those of the packages that `additional_packages` links.
    ///
    /// # Errors
    ///
    /// Returns an error when a package is outside `workspace`, when a folder
    /// is not UTF-8, or when the deepest folder is the folder of an
    /// additional package, which would then share the root namespace.
    pub(crate) fn new(
        workspace: &Path,
        application: (&str, &Path),
        additional: &[(&str, &Path)],
    ) -> Result<Self> {
        let workspace = paths::normalized(workspace);
        let (application, application_folder) = application;
        let application_segments = segments(&workspace, application, application_folder)?;
        let mut linked = Vec::new();
        for (name, folder) in additional {
            linked.push((*name, segments(&workspace, name, folder)?));
        }
        let shared = linked
            .iter()
            .fold(application_segments.len(), |shared, (_, segments)| {
                application_segments
                    .iter()
                    .zip(segments)
                    .take(shared)
                    .take_while(|(left, right)| left == right)
                    .count()
            });
        let base = application_segments[..shared].to_vec();
        let mut packages = BTreeMap::from([(CargoPackageId::new(application), Vec::new())]);
        for (name, segments) in linked {
            if segments.len() == shared {
                let (folder, fix) = if base.is_empty() {
                    ("the workspace root".to_owned(), String::new())
                } else {
                    let folder = format!("`{}`", base.join("/"));
                    let fix = format!("move the other packages out of {folder}, or ");
                    (folder, fix)
                };
                bail!(
                    "`namespaces = \"folders\"` would give Cargo package `{name}` the root namespace of `{application}`, because its folder, {folder}, holds every other package of the application; {fix}set `namespaces = \"packages\"` in bimetal.toml"
                );
            }
            packages.insert(CargoPackageId::new(name), segments[shared..].to_vec());
        }
        Ok(Self { base, packages })
    }

    /// Return the rule that names the declarations of `manifest` after these
    /// folders.
    ///
    /// # Errors
    ///
    /// Returns an error that lists every package that declares something but
    /// is not a package of the application, which has no folder here.
    pub(crate) fn rule(&self, manifest: &Manifest) -> Result<NamespaceRule> {
        let owners = manifest
            .types
            .iter()
            .map(|item| &item.owner)
            .chain(manifest.errors.iter().map(|item| &item.owner))
            .chain(manifest.functions.iter().map(|item| &item.owner))
            .chain(manifest.resources.iter().map(|item| &item.owner))
            .chain(manifest.constants.iter().map(|item| &item.owner));
        let unknown: BTreeSet<&CargoPackageId> = owners
            .filter(|owner| !self.packages.contains_key(*owner))
            .collect();
        let problems: Vec<String> = unknown
            .into_iter()
            .map(|owner| {
                format!(
                    "Cargo package `{owner}` declares exports, but `namespaces = \"folders\"` can name only the application package and those of `additional_packages`, which must be members of this Cargo workspace; add `{owner}` to `additional_packages`, or set `namespaces = \"packages\"` in bimetal.toml"
                )
            })
            .collect();
        problems::report(Subject::RustApi, &problems)?;
        Ok(NamespaceRule::Folders(self.clone()))
    }

    /// Return the folder that the segments `package` of a namespace name,
    /// relative to the workspace root, such as `packages/sources/venues`.
    #[must_use]
    pub(crate) fn folder(&self, package: &[String]) -> String {
        let segments: Vec<&str> = self
            .base
            .iter()
            .chain(package)
            .map(String::as_str)
            .collect();
        segments.join("/")
    }

    /// Return what the source fingerprint covers of these folders: each
    /// package's segments. The name of a symlink on the way to a package
    /// sets them, and no manifest records it.
    #[must_use]
    pub(crate) fn fingerprint_source(&self) -> String {
        let mut source = String::new();
        for (package, segments) in &self.packages {
            let _ = write!(source, "{package}\0{}\0", segments.join("/"));
        }
        source
    }
}

/// Return the folders from `workspace`, which is normalized, down to
/// `folder`, the folder of the Cargo package `name`.
fn segments(workspace: &Path, name: &str, folder: &Path) -> Result<Vec<String>> {
    let folder = paths::normalized(folder);
    let Ok(relative) = folder.strip_prefix(workspace) else {
        // A folder on another drive has no path relative to the workspace.
        let shown = paths::portable_relative(workspace, &folder).map_or_else(
            |_| folder.display().to_string(),
            |relative| format!("`{relative}`"),
        );
        bail!(
            "`namespaces = \"folders\"` names packages after their folders in the Cargo workspace, but Cargo package `{name}` is in {shown}, outside the workspace root; move it into the workspace, or set `namespaces = \"packages\"` in bimetal.toml"
        );
    };
    let relative = paths::portable(relative).with_context(|| {
        format!("`namespaces = \"folders\"` cannot name Cargo package `{name}` after its folder")
    })?;
    Ok(relative
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    /// The layout of a workspace whose bindings link four packages, two of
    /// them nested two folders deeper than the others.
    fn workspace() -> Folders {
        Folders::new(
            Path::new("/work"),
            ("xf-bindings", Path::new("/work/packages/bindings")),
            &[
                ("xf-core", Path::new("/work/packages/core")),
                ("xf-trading", Path::new("/work/packages/trading")),
                (
                    "xf-polymarket",
                    Path::new("/work/packages/sources/venues/polymarket"),
                ),
                ("xf-sports", Path::new("/work/packages/sources/data/sports")),
            ],
        )
        .unwrap()
    }

    /// Return the Python segments of the namespace that `folders` give a
    /// declaration of `owner` in `rust_module`.
    fn python(folders: &Folders, owner: &str, rust_module: &str) -> Vec<String> {
        NamespaceRule::Folders(folders.clone())
            .namespace(&CargoPackageId::new(owner), rust_module)
            .python_segments()
    }

    #[test]
    fn packages_are_named_after_their_folders_below_the_deepest_common_one() {
        let folders = workspace();

        assert_eq!(folders.base, ["packages"]);
        assert_eq!(
            python(&folders, "xf-core", "xf_core::data"),
            ["core", "data"]
        );
        assert_eq!(python(&folders, "xf-trading", "xf_trading"), ["trading"]);
        assert_eq!(
            python(&folders, "xf-polymarket", "xf_polymarket::markets::fees"),
            ["sources", "venues", "polymarket", "markets", "fees"]
        );
        assert_eq!(
            python(&folders, "xf-sports", "xf_sports"),
            ["sources", "data", "sports"]
        );
        assert_eq!(
            folders.folder(&["sources".to_owned(), "venues".to_owned()]),
            "packages/sources/venues"
        );
    }

    #[test]
    fn the_application_package_takes_the_root_wherever_its_folder_is() {
        let folders = workspace();
        assert!(python(&folders, "xf-bindings", "xf_bindings").is_empty());
        assert_eq!(
            python(&folders, "xf-bindings", "xf_bindings::fees"),
            ["fees"]
        );

        // The application's folder can hold the others, or be the only one.
        let outer = Folders::new(
            Path::new("/work"),
            ("showcase", Path::new("/work/showcase")),
            &[("showcase-geometry", Path::new("/work/showcase/geometry"))],
        )
        .unwrap();
        assert!(python(&outer, "showcase", "showcase").is_empty());
        assert_eq!(
            python(&outer, "showcase-geometry", "showcase_geometry"),
            ["geometry"]
        );
        let alone = Folders::new(Path::new("/work"), ("app", Path::new("/work/app")), &[]).unwrap();
        assert!(python(&alone, "app", "app").is_empty());
    }

    #[test]
    fn folder_segments_keep_their_hyphens_until_python() {
        let folders = Folders::new(
            Path::new("/work"),
            ("app", Path::new("/work/app")),
            &[("app-feeds", Path::new("/work/live-feeds"))],
        )
        .unwrap();
        let namespace = NamespaceRule::Folders(folders)
            .namespace(&CargoPackageId::new("app-feeds"), "app_feeds::quotes");

        assert_eq!(namespace.python_segments(), ["live_feeds", "quotes"]);
        assert_eq!(namespace.typescript_segments(), ["live-feeds", "quotes"]);
    }

    #[test]
    fn only_the_application_package_takes_the_root() {
        let error = |workspace: &str| {
            Folders::new(
                Path::new(workspace),
                ("geometry", Path::new("/work/showcase/geometry")),
                &[("showcase", Path::new("/work/showcase"))],
            )
            .unwrap_err()
            .to_string()
        };

        assert_eq!(
            error("/work"),
            "`namespaces = \"folders\"` would give Cargo package `showcase` the root namespace of `geometry`, because its folder, `showcase`, holds every other package of the application; move the other packages out of `showcase`, or set `namespaces = \"packages\"` in bimetal.toml"
        );
        // Nothing can move out of the workspace root.
        assert_eq!(
            error("/work/showcase"),
            "`namespaces = \"folders\"` would give Cargo package `showcase` the root namespace of `geometry`, because its folder, the workspace root, holds every other package of the application; set `namespaces = \"packages\"` in bimetal.toml"
        );
    }

    #[test]
    fn every_package_is_named_below_the_workspace_root() {
        // The packages share only the workspace root, so no folder above it
        // names a namespace.
        let folders = Folders::new(
            Path::new("/work/ws"),
            ("app", Path::new("/work/ws/app")),
            &[("app-core", Path::new("/work/ws/./core/../core"))],
        )
        .unwrap();
        assert!(folders.base.is_empty());
        assert_eq!(python(&folders, "app-core", "app_core"), ["core"]);

        // Cargo allows a member outside the workspace root, such as
        // `members = ["../outside/far"]`.
        let outside = |application: &str, additional: &str| {
            Folders::new(
                Path::new("/work/ws"),
                ("app", Path::new(application)),
                &[("far", Path::new(additional))],
            )
            .unwrap_err()
            .to_string()
        };
        let far = "`namespaces = \"folders\"` names packages after their folders in the Cargo workspace, but Cargo package `far` is in `../outside/far`, outside the workspace root; move it into the workspace, or set `namespaces = \"packages\"` in bimetal.toml";
        assert_eq!(outside("/work/ws/app", "/work/outside/far"), far);
        assert_eq!(outside("/work/ws/app", "/work/ws/../outside/far"), far);
        assert!(
            outside("/work/app", "/work/ws/far").contains("Cargo package `app` is in `../app`"),
            "the application package must be in the workspace too"
        );
        // A folder whose name only starts with the root's is outside it.
        assert!(outside("/work/ws/app", "/work/ws-other/far").contains("is in `../ws-other/far`"));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_is_not_utf8_names_no_namespace() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let folder = Path::new("/work/packages").join(OsStr::from_bytes(b"feeds-\xff"));
        let error = Folders::new(
            Path::new("/work"),
            ("app", Path::new("/work/packages/app")),
            &[("app-feeds", &folder)],
        )
        .unwrap_err();

        assert_eq!(
            format!("{error:#}"),
            "`namespaces = \"folders\"` cannot name Cargo package `app-feeds` after its folder: packages/feeds-\u{fffd} is not valid UTF-8"
        );
    }

    #[test]
    fn every_package_that_declares_something_needs_a_folder() {
        let folders = workspace();
        let mut manifest = fixtures::manifest();
        manifest
            .functions
            .push(fixtures::declared("xf-core", "xf_core", "price"));
        assert!(folders.rule(&manifest).is_ok());

        manifest.types.push(fixtures::model("Point", Vec::new()));
        let error = folders.rule(&manifest).unwrap_err().to_string();
        assert_eq!(
            error,
            "Cargo package `test` declares exports, but `namespaces = \"folders\"` can name only the application package and those of `additional_packages`, which must be members of this Cargo workspace; add `test` to `additional_packages`, or set `namespaces = \"packages\"` in bimetal.toml"
        );
    }

    #[test]
    fn the_rule_follows_the_setting() {
        let mut manifest = fixtures::manifest();
        manifest.package_name = "xf-bindings".to_owned();
        manifest.functions.extend([
            fixtures::declared("xf-bindings", "xf_bindings", "greet"),
            fixtures::declared("xf-sports", "xf_sports", "score"),
        ]);
        let folders = workspace();
        let namespace = |folders: Option<&Folders>, owner: &str, rust_module: &str| {
            NamespaceRule::new(&manifest, folders)
                .unwrap()
                .namespace(&CargoPackageId::new(owner), rust_module)
                .python_segments()
        };

        // Without folders, the application package keeps a segment of its
        // own unless its name is the prefix that every package shares.
        assert_eq!(namespace(None, "xf-bindings", "xf_bindings"), ["bindings"]);
        assert_eq!(namespace(None, "xf-sports", "xf_sports"), ["sports"]);
        assert!(namespace(Some(&folders), "xf-bindings", "xf_bindings").is_empty());
        assert_eq!(
            namespace(Some(&folders), "xf-sports", "xf_sports"),
            ["sources", "data", "sports"]
        );
    }

    #[test]
    fn the_fingerprint_covers_each_package_folder() {
        let layout = |sports: &str| {
            Folders::new(
                Path::new("/work"),
                ("app", Path::new("/work/packages/app")),
                &[("app-sports", Path::new(sports))],
            )
            .unwrap()
            .fingerprint_source()
        };

        assert_eq!(
            layout("/work/packages/data/sports"),
            "app\0\0app-sports\0data/sports\0"
        );
        assert_ne!(
            layout("/work/packages/data/sports"),
            layout("/work/packages/feeds/sports")
        );
    }
}
