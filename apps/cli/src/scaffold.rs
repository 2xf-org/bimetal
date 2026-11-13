//! `bimetal new`: the user-owned files of a new application.
//!
//! The scaffold writes only files that Bimetal never changes afterwards. The
//! first `bimetal build` generates the Python and TypeScript packages.

use std::collections::BTreeMap;
use std::fs;
use std::path::{self, Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::cli::NewArgs;
use crate::dependencies::{NUMPY, PYARROW, PYDANTIC};
use crate::literal;
use crate::output;
use crate::paths;
use crate::project::{self, BimetalDependency, GitReference};
use crate::python;

/// The result of `bimetal new`, which says `"status": "ok"` in JSON.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename = "ok")]
pub(super) struct NewReport {
    pub(super) application: String,
    pub(super) directory: PathBuf,
    pub(super) files: Vec<PathBuf>,
}

/// Create a new application directory, and its parents, with its user-owned
/// files. The directory can also be an empty one that exists, such as `.`.
pub(super) fn create(args: &NewArgs) -> Result<NewReport> {
    let directory = path::absolute(&args.directory)
        .with_context(|| format!("cannot find {}", args.directory.display()))?;
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .context("the new application directory needs a name")?
        .to_owned();
    project::validate_application_name(&name)?;
    let python_package = python::python_package_name(&name);
    project::validate_python_package(&python_package)?;
    project::validate_typescript_package(&name)?;
    if fs::read_dir(&directory).is_ok_and(|mut entries| entries.next().is_some()) {
        bail!(
            "{} already exists and is not empty",
            args.directory.display()
        );
    }
    let dependency = bimetal_dependency(args)?;
    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let directory = paths::canonical(&directory)
        .with_context(|| format!("cannot find {}", directory.display()))?;
    let dependency = match dependency {
        BimetalDependency::Path(runtime) => {
            BimetalDependency::Path(relative_path(&directory, &runtime))
        }
        other => other,
    };

    let files = [
        ("Cargo.toml", cargo_manifest(&name, &dependency)),
        ("src/lib.rs", LIBRARY.to_owned()),
        ("bimetal.toml", configuration(&name, &python_package)),
        (
            "python/pyproject.toml",
            python_project(&name, &python_package),
        ),
        ("python/pdm_build.py", PDM_BUILD_HOOK.to_owned()),
        ("typescript/package.json", typescript_package(&name)?),
        (".gitignore", GITIGNORE.to_owned()),
        (".gitattributes", GITATTRIBUTES.to_owned()),
    ];
    let mut written = Vec::new();
    for (relative, source) in files {
        output::write(&directory.join(relative), &source)?;
        written.push(PathBuf::from(relative));
    }
    Ok(NewReport {
        application: name,
        directory,
        files: written,
    })
}

/// Resolve how the new application depends on the `bimetal` runtime: on
/// crates.io unless the arguments name a checkout or a git repository. A path
/// dependency names the canonical runtime directory.
fn bimetal_dependency(args: &NewArgs) -> Result<BimetalDependency> {
    if let Some(checkout) = &args.bimetal_path {
        let runtime = paths::canonical(&checkout.join("packages/runtime"))
            .ok()
            .filter(|runtime| runtime.join("Cargo.toml").is_file())
            .with_context(|| format!("{} is not a Bimetal checkout", checkout.display()))?;
        return Ok(BimetalDependency::Path(runtime));
    }
    let Some(url) = args.bimetal_git.clone() else {
        return Ok(BimetalDependency::Registry);
    };
    let reference = [
        ("tag", &args.tag),
        ("branch", &args.branch),
        ("rev", &args.rev),
    ]
    .into_iter()
    .find_map(|(key, value)| {
        value.clone().map(|value| GitReference {
            key: key.to_owned(),
            value,
        })
    });
    Ok(BimetalDependency::Git { url, reference })
}

/// Express `target` relative to `base` when both are below a shared directory
/// other than the root, with `/` between its components, so that the manifest
/// works once committed and checked out on another platform; and as the
/// absolute `target` otherwise.
#[must_use]
fn relative_path(base: &Path, target: &Path) -> PathBuf {
    let relative = paths::relative(base, target);
    let climbs = relative
        .components()
        .take_while(|component| *component == Component::ParentDir)
        .count();
    // A drive prefix and the root are no shared directory.
    let depth = base
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    match paths::portable(&relative) {
        // A target on another drive has no relative path, so `portable`
        // refuses it.
        Ok(portable) if climbs < depth => PathBuf::from(portable),
        _ => target.to_path_buf(),
    }
}

/// Render the application's `Cargo.toml`.
#[must_use]
fn cargo_manifest(name: &str, dependency: &BimetalDependency) -> String {
    format!(
        r#"[package]
name = {name}
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
bimetal = {dependency}
serde = {{ version = "1.0", features = ["derive"] }}

# The application is its own Cargo workspace. To add it to an existing
# workspace instead, remove this table and list the directory in that
# workspace's `members`.
[workspace]
"#,
        name = literal::quoted(name),
        dependency = dependency.manifest_value(),
    )
}

/// Render `bimetal.toml`, whose `[generated]` tables the first build adds.
#[must_use]
pub(super) fn configuration(name: &str, underscored: &str) -> String {
    format!(
        r#"# Bimetal configuration. Edit the tables above [generated]; `bimetal build`
# maintains [generated].

[application]
# Every setting is optional.
# The application name; defaults to the Cargo package name.
# name = {name}
# Other packages of this Cargo workspace to link into the application.
# additional_packages = []
# "packages" names their namespaces after the Cargo packages, "folders" after
# their folders.
# namespaces = "packages"

# Each host table turns on a package for that host, and every setting in it
# is optional. Projects are folders relative to this file.

[python]
# The import package; defaults to the name with underscores.
# package = {underscored}
# project = "python"

[typescript]
# The npm package takes the name that package.json declares.
# project = "typescript"
# "wasm" runs the Rust code as a WebAssembly module, "node" as a native
# Node.js addon.
# backend = "wasm"
"#,
        name = literal::quoted(name),
        underscored = literal::quoted(underscored),
    )
}

/// Render the Python project's `pyproject.toml`, which ships the import
/// package `package`.
#[must_use]
fn python_project(name: &str, package: &str) -> String {
    format!(
        r#"[build-system]
requires = ["pdm-backend>=2.4.9,<3"]
build-backend = "pdm.backend"

[project]
name = {name}
version = "0.1.0"
requires-python = ">={MINIMUM_PYTHON}"
# PyPI rejects any upload with this classifier; remove it to publish.
classifiers = ["Private :: Do Not Upload"]
# Add {numpy} when the contract uses buffers, and {pyarrow} when it
# uses Arrow values.
dependencies = [
    {pydantic},
]

[tool.pdm.build]
includes = [{package}]
# The package ships a native extension, so wheels are platform-specific;
# pdm_build.py tags them for the stable ABI and the extension's platform.
is-purelib = false
"#,
        name = literal::quoted(name),
        package = literal::quoted(package),
        numpy = literal::quoted(NUMPY.requirement),
        pyarrow = literal::quoted(PYARROW.requirement),
        pydantic = literal::quoted(PYDANTIC.requirement),
    )
}

/// Render the npm package's `package.json`.
fn typescript_package(name: &str) -> Result<String> {
    let entry = |path: &str| ExportEntry {
        types: format!("./{name}/{path}.d.ts"),
        default: format!("./{name}/{path}.js"),
    };
    let package = PackageManifest {
        name,
        version: "0.1.0",
        private: true,
        module_type: "module",
        side_effects: true,
        exports: BTreeMap::from([(".", entry("index")), ("./*", entry("*/index"))]),
        files: [name],
        engines: Engines { node: MINIMUM_NODE },
    };
    Ok(format!("{}\n", serde_json::to_string_pretty(&package)?))
}

/// The scaffolded `package.json`, with its fields in the conventional order.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackageManifest<'name> {
    name: &'name str,
    version: &'static str,
    private: bool,
    #[serde(rename = "type")]
    module_type: &'static str,
    side_effects: bool,
    exports: BTreeMap<&'static str, ExportEntry>,
    files: [&'name str; 1],
    engines: Engines,
}

/// One `exports` condition set. TypeScript resolves conditions in order, so
/// `types` must precede `default`.
#[derive(Serialize)]
struct ExportEntry {
    types: String,
    default: String,
}

/// The runtimes that the package supports.
#[derive(Serialize)]
struct Engines {
    node: &'static str,
}

/// The oldest Node.js that loads the generated package: the runtime reads the
/// Wasm module with `process.getBuiltinModule`, which Node.js 20.16 added.
const MINIMUM_NODE: &str = ">=20.16";

/// The oldest Python that the generated package supports: the release whose
/// stable ABI the extension targets, which pyo3's `abi3-py311` feature sets,
/// and for which `pdm_build.py` tags the wheels.
const MINIMUM_PYTHON: &str = "3.11";

/// The build hook that builds only wheels and tags them for the stable ABI
/// and the extension's platform.
const PDM_BUILD_HOOK: &str = include_str!("python/pdm_build.py");

const LIBRARY: &str = r#"//! The Rust API that Bimetal turns into Python and TypeScript packages.

use bimetal::Model;
use serde::{Deserialize, Serialize};

/// A greeting for one person.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct Greeting {
    /// The full greeting text.
    pub message: String,
}

/// Greet someone by name.
#[bimetal::export]
#[must_use]
pub fn greet(name: String) -> Greeting {
    Greeting {
        message: format!("Hello, {name}!"),
    }
}
"#;

/// Keeps Git for Windows from checking out the generated files with CRLF.
const GITATTRIBUTES: &str =
    "# `bimetal check` compares generated files byte for byte, and Bimetal writes LF.
* text=auto eol=lf
*.wasm binary
";

const GITIGNORE: &str = "target/
.venv/
node_modules/
__pycache__/

# Packages that `uv build` and `npm pack` write.
dist/
*.tgz

# Each platform builds its own native artifacts: the Python extension and the
# Node.js addon.
*.abi3.so
*.pyd
*.node
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Backend, Config, Namespaces};

    fn arguments(directory: PathBuf) -> NewArgs {
        NewArgs {
            directory,
            bimetal_path: Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")),
            bimetal_git: None,
            tag: None,
            branch: None,
            rev: None,
        }
    }

    #[test]
    fn creates_only_user_owned_files() {
        let parent = tempfile::tempdir().unwrap();
        let report = create(&arguments(parent.path().join("my-app"))).unwrap();

        assert_eq!(report.application, "my-app");
        let directory = paths::canonical(parent.path()).unwrap().join("my-app");
        let manifest = fs::read_to_string(directory.join("Cargo.toml")).unwrap();
        assert!(manifest.contains("name = \"my-app\""));
        assert!(manifest.contains("bimetal = { path = \""));
        let pyproject = fs::read_to_string(directory.join("python/pyproject.toml")).unwrap();
        assert!(pyproject.contains("includes = [\"my_app\"]"));
        assert!(pyproject.contains("classifiers = [\"Private :: Do Not Upload\"]"));
        let package: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(directory.join("typescript/package.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(package["exports"]["."]["default"], "./my-app/index.js");
        assert_eq!(package["engines"]["node"], ">=20.16");
        let ignored = fs::read_to_string(directory.join(".gitignore")).unwrap();
        assert!(
            ignored.lines().any(|line| line == "dist/")
                && ignored.lines().any(|line| line == "*.tgz"),
            "built packages stay out of the repository"
        );
        assert!(
            ["*.abi3.so", "*.pyd", "*.node"]
                .iter()
                .all(|pattern| ignored.lines().any(|line| line == *pattern)),
            "native artifacts stay out of the repository"
        );
        let settings = Config::read(&directory.join("bimetal.toml"))
            .unwrap()
            .settings;
        assert!(settings.python.is_some() && settings.typescript.is_some());
        let manifest = fs::read_to_string(directory.join("typescript/package.json")).unwrap();
        assert!(
            manifest.find("\"types\"") < manifest.find("\"default\""),
            "TypeScript resolves export conditions in order, so `types` comes first"
        );
        assert!(directory.join("python/pdm_build.py").is_file());
        let attributes = fs::read_to_string(directory.join(".gitattributes")).unwrap();
        assert!(
            attributes.lines().any(|line| line == "* text=auto eol=lf")
                && attributes.lines().any(|line| line == "*.wasm binary"),
            "Git checks out the generated files as Bimetal wrote them"
        );
        assert!(!directory.join("python/my_app").exists());
    }

    #[test]
    fn the_commented_settings_are_valid() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bimetal.toml");
        let configuration = configuration("my-app", "my_app")
            .replace("# package = ", "package = ")
            .replace("# backend = ", "backend = ")
            .replace("# namespaces = ", "namespaces = ");
        fs::write(&path, configuration).unwrap();

        let settings = Config::read(&path).unwrap().settings;

        assert_eq!(settings.application.namespaces, Namespaces::Packages);
        assert_eq!(settings.python.unwrap().package.as_deref(), Some("my_app"));
        assert_eq!(settings.typescript.unwrap().backend, Backend::Wasm);
    }

    #[test]
    fn the_examples_use_the_scaffolded_build_hook() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut configs = Vec::new();
        for entry in ignore::Walk::new(workspace.join("apps/examples")) {
            let path = entry.expect("walk the examples").into_path();
            if path.file_name().is_some_and(|name| name == "bimetal.toml") {
                configs.push(path);
            }
        }
        assert!(configs.len() >= 3, "{configs:?}");
        for path in configs {
            let config = Config::read(&path).expect("read the example's bimetal.toml");
            let Some(python) = &config.settings.python else {
                continue;
            };
            let project = python.project.as_deref().unwrap_or("python");
            let hook = config.root().join(project).join("pdm_build.py");
            let hook = fs::read_to_string(&hook)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", hook.display()));
            assert_eq!(
                hook,
                PDM_BUILD_HOOK,
                "{} has another build hook",
                path.display()
            );
        }
    }

    #[test]
    fn the_python_floor_is_the_stable_abi_of_the_extension() {
        let tag = MINIMUM_PYTHON.replace('.', "");
        let workspace =
            fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
                .expect("read the workspace manifest");
        assert!(
            workspace.contains(&format!("features = [\"abi3-py{tag}\"]")),
            "pyo3 must build for the stable ABI of Python {MINIMUM_PYTHON}"
        );
        assert!(PDM_BUILD_HOOK.contains(&format!("STABLE_ABI_PYTHON = \"cp{tag}\"\n")));
        assert!(
            python_project("app", "app")
                .contains(&format!("requires-python = \">={MINIMUM_PYTHON}\"\n"))
        );
    }

    #[test]
    fn refuses_a_directory_with_files() {
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("app");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("notes.txt"), "keep").unwrap();

        let error = create(&arguments(directory)).unwrap_err().to_string();

        assert!(error.contains("not empty"));
    }

    #[test]
    fn creates_missing_parents_and_names_an_empty_directory_after_itself() {
        let parent = tempfile::tempdir().unwrap();
        let root = paths::canonical(parent.path()).unwrap();

        let nested = create(&arguments(parent.path().join("apps").join("my-app"))).unwrap();
        assert_eq!(nested.directory, root.join("apps").join("my-app"));
        assert!(nested.directory.join("Cargo.toml").is_file());

        // As `bimetal new .` does in an empty directory.
        fs::create_dir(parent.path().join("here-app")).unwrap();
        let here = create(&arguments(parent.path().join("here-app").join("."))).unwrap();
        assert_eq!(here.application, "here-app");
        assert_eq!(here.directory, root.join("here-app"));
    }

    #[test]
    fn git_dependencies_keep_their_reference() {
        let parent = tempfile::tempdir().unwrap();
        let mut args = arguments(parent.path().join("app"));
        args.bimetal_path = None;
        args.bimetal_git = Some("ssh://git@example.com/team/bimetal.git".to_owned());
        args.tag = Some("v1.0.0".to_owned());

        create(&args).unwrap();

        let manifest = fs::read_to_string(
            paths::canonical(parent.path())
                .unwrap()
                .join("app/Cargo.toml"),
        )
        .unwrap();
        assert!(manifest.contains(
            r#"bimetal = { git = "ssh://git@example.com/team/bimetal.git", tag = "v1.0.0" }"#
        ));
    }

    #[test]
    fn the_dependency_is_this_release_unless_a_source_is_named() {
        let parent = tempfile::tempdir().unwrap();
        let mut args = arguments(parent.path().join("app"));
        args.bimetal_path = None;

        create(&args).unwrap();

        let manifest = fs::read_to_string(
            paths::canonical(parent.path())
                .unwrap()
                .join("app/Cargo.toml"),
        )
        .unwrap();
        assert!(manifest.contains(concat!("bimetal = \"=", env!("CARGO_PKG_VERSION"), "\"\n")));
    }

    #[test]
    fn relative_paths_climb_to_the_shared_ancestor() {
        let relative = |base: &str, target: &str| relative_path(Path::new(base), Path::new(target));
        assert_eq!(
            relative("/work/apps/demo", "/work/bimetal/packages/runtime"),
            Path::new("../../bimetal/packages/runtime")
        );
        assert_eq!(
            relative("/work/demo", "/work/bimetal/packages/runtime"),
            Path::new("../bimetal/packages/runtime")
        );
        assert_eq!(
            relative("/tmp/demo", "/work/bimetal/packages/runtime"),
            Path::new("/work/bimetal/packages/runtime")
        );
        // The manifest names the path with `/` on every platform.
        let written = relative("/work/apps/demo", "/work/bimetal/packages/runtime");
        assert_eq!(written.to_str(), Some("../../bimetal/packages/runtime"));
    }
}
