//! The synthetic link crate, which links every application package into one
//! library for each build, outside the application's source tree.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bimetal_ir::hash;

use crate::cargo::{self, PathRemaps, Resolution, Toolchain};
use crate::literal;
use crate::output;
use crate::paths;

use super::Project;

/// The portable name of the directory that holds an application's link
/// crates.
const LINK_CRATES_NAME: &str = "/bimetal-link";

/// Synthetic Cargo package that links all selected application crates.
pub(super) struct LinkCrate {
    pub(super) manifest: PathBuf,
    pub(super) target_directory: PathBuf,
    pub(super) package_id: String,
    pub(super) package_name: String,
    /// The host's target triple, which the host builds name, so that a
    /// configured `build.target` applies to none of them.
    pub(super) host: String,
    /// The portable names of local directories, which also turn the paths in
    /// diagnostics back into local ones.
    pub(super) remaps: PathRemaps,
    /// The flags that give rustc those names.
    pub(super) remap_flags: Vec<String>,
    /// The user's `RUSTC_WRAPPER`, which Bimetal's runs.
    pub(super) rustc_wrapper: Option<PathBuf>,
}

/// Materialize the stable synthetic link crate under Cargo's target tree.
pub(super) fn prepare_link_crate(project: &Project) -> Result<LinkCrate> {
    use std::fmt::Write as _;

    let link_crates = link_crates_directory(&project.target_directory, &project.workspace_root);
    let key = link_crate_key(&project.workspace_root, project.config_path())?;
    let root = link_crates.join(format!("{key:016x}"));
    let source_root = root.join("src");
    // The package name is part of the artifacts, so it depends only on where
    // bimetal.toml is inside its workspace, never on where the checkout is.
    let package_name = format!("bimetal-link-{key:016x}");

    let mut source = String::from("bimetal::application!(\n");
    for index in 0..project.linked_packages.len() {
        let _ = writeln!(source, "    bimetal_application_{index},");
    }
    source.push_str(");\n");
    output::persist(&source_root.join("lib.rs"), source.as_bytes())?;

    let link_manifest = root.join("Cargo.toml");
    let workspace = read_workspace_manifest(&project.workspace_root)?;
    output::persist(
        &link_manifest,
        render_manifest(project, &package_name, &workspace)?.as_bytes(),
    )?;
    // The application's lockfile pins every dependency of the link crate's
    // own workspace, and resolving it here lets every build run `--locked`.
    let workspace_lock = project.workspace_root.join("Cargo.lock");
    if workspace_lock.is_file() {
        fs::copy(&workspace_lock, root.join("Cargo.lock"))
            .context("failed to seed the Bimetal link crate lockfile")?;
    }
    // The link crate's lockfile gains the link crate itself, so only the
    // application's own lockfile can be locked.
    let resolution = Resolution {
        locked: false,
        ..project.resolution
    };
    let package_id = cargo::metadata(&link_manifest, &project.root, resolution)
        .context("failed to resolve the generated Bimetal link crate")?
        .workspace_members
        .first()
        .context("generated Bimetal link crate metadata has no package")?
        .clone();

    let remaps = path_remaps(
        project,
        cargo::home(&project.root),
        &project.toolchain,
        link_crates,
        key,
    );
    // An empty `RUSTC_WRAPPER` turns a configured one off, as in Cargo.
    let rustc_wrapper = project
        .configuration
        .tool(&project.root, "rustc-wrapper")
        .filter(|wrapper| !wrapper.as_os_str().is_empty());
    Ok(LinkCrate {
        manifest: link_manifest,
        target_directory: project.target_directory.clone(),
        package_id,
        package_name,
        host: project.toolchain.host().to_owned(),
        remap_flags: remaps.flags()?,
        remaps,
        rustc_wrapper,
    })
}

fn read_workspace_manifest(workspace_root: &Path) -> Result<toml::Table> {
    let path = workspace_root.join("Cargo.toml");
    let source =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&source).with_context(|| format!("failed to parse {}", path.display()))
}

/// Render the link crate's manifest: its dependencies, and the settings of
/// the application's workspace manifest that decide how they resolve and
/// compile. Cargo reads those only from the root manifest of the workspace it
/// builds, which for the link crate is the link crate's own.
fn render_manifest(
    project: &Project,
    package_name: &str,
    workspace: &toml::Table,
) -> Result<String> {
    use std::fmt::Write as _;

    let mut manifest = format!(
        "[package]\nname = {}\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[dependencies]\n",
        literal::quoted(package_name)
    );
    let _ = writeln!(
        manifest,
        "bimetal = {}",
        project.bimetal_dependency.manifest_value()
    );
    for (index, package) in project.linked_packages.iter().enumerate() {
        let root = package
            .manifest
            .parent()
            .context("linked Cargo manifest has no parent")?;
        let _ = writeln!(
            manifest,
            "bimetal_application_{index} = {{ package = {}, path = {} }}",
            literal::quoted(&package.name),
            literal::quoted(&root.to_string_lossy())
        );
    }
    let _ = writeln!(
        manifest,
        "\n[workspace]\nresolver = {}",
        literal::quoted(&resolver(workspace))
    );
    let settings = workspace_settings(workspace, &project.workspace_root)?;
    if !settings.is_empty() {
        manifest.push('\n');
        manifest.push_str(
            &toml::to_string(&settings).context("failed to render the workspace settings")?,
        );
    }
    Ok(manifest)
}

/// Return the dependency resolver of a workspace manifest: the configured
/// one, or the default that Cargo derives from the root package's edition.
#[must_use]
fn resolver(workspace: &toml::Table) -> String {
    let configured = workspace
        .get("workspace")
        .and_then(|table| table.get("resolver"))
        .or_else(|| workspace.get("package")?.get("resolver"))
        .and_then(toml::Value::as_str);
    if let Some(resolver) = configured {
        return resolver.to_owned();
    }
    // A virtual workspace without a resolver uses the first one.
    let Some(package) = workspace.get("package") else {
        return "1".to_owned();
    };
    let edition = match package.get("edition") {
        Some(toml::Value::String(edition)) => edition.as_str(),
        Some(toml::Value::Table(_)) => workspace
            .get("workspace")
            .and_then(|table| table.get("package")?.get("edition")?.as_str())
            .unwrap_or("2015"),
        _ => "2015",
    };
    match edition {
        "2015" | "2018" => "1",
        "2021" => "2",
        _ => "3",
    }
    .to_owned()
}

/// Copy `[patch]`, `[replace]`, and `[profile]` from the application's
/// workspace manifest. The link crate lives elsewhere, so the relative paths
/// of patches and replacements become absolute.
fn workspace_settings(workspace: &toml::Table, workspace_root: &Path) -> Result<toml::Table> {
    let mut settings = toml::Table::new();
    if let Some(toml::Value::Table(patches)) = workspace.get("patch") {
        let mut patches = patches.clone();
        for (_, source) in patches.iter_mut() {
            if let toml::Value::Table(dependencies) = source {
                for (_, dependency) in dependencies.iter_mut() {
                    rebase_path(dependency, workspace_root)?;
                }
            }
        }
        settings.insert("patch".to_owned(), toml::Value::Table(patches));
    }
    if let Some(toml::Value::Table(replacements)) = workspace.get("replace") {
        let mut replacements = replacements.clone();
        for (_, dependency) in replacements.iter_mut() {
            rebase_path(dependency, workspace_root)?;
        }
        settings.insert("replace".to_owned(), toml::Value::Table(replacements));
    }
    if let Some(profiles) = workspace.get("profile") {
        settings.insert("profile".to_owned(), profiles.clone());
    }
    Ok(settings)
}

/// Make a dependency's relative `path` absolute against the workspace root,
/// with `..` resolved the way Cargo resolves it, without following symlinks.
fn rebase_path(dependency: &mut toml::Value, workspace_root: &Path) -> Result<()> {
    let Some(path) = dependency.get_mut("path") else {
        return Ok(());
    };
    let Some(relative) = path.as_str() else {
        return Ok(());
    };
    let absolute = paths::normalized(&workspace_root.join(relative));
    let absolute = absolute
        .to_str()
        .with_context(|| format!("{} is not a UTF-8 path", absolute.display()))?;
    *path = toml::Value::String(absolute.to_owned());
    Ok(())
}

/// Identify a link crate by its checkout-independent workspace-relative config path.
fn link_crate_key(workspace_root: &Path, config_path: &Path) -> Result<u64> {
    let relative = config_path.strip_prefix(workspace_root).with_context(|| {
        format!(
            "bimetal configuration {} is outside Cargo workspace {}",
            config_path.display(),
            workspace_root.display()
        )
    })?;
    let identity = paths::portable(relative)?;
    Ok(hash::fnv1a(identity.as_bytes()))
}

/// Return the directory of a workspace's link crates. Its name hashes the
/// workspace's location, so applications that share a target directory never
/// share a link crate. Every build names it [`LINK_CRATES_NAME`], so the
/// location never reaches an artifact, and every application of a workspace
/// gives its dependencies the same flags.
#[must_use]
fn link_crates_directory(target_directory: &Path, workspace_root: &Path) -> PathBuf {
    let key = hash::fnv1a(workspace_root.as_os_str().as_encoded_bytes());
    target_directory
        .join("bimetal")
        .join(format!("link-{key:016x}"))
}

/// Name every local directory that can reach an artifact portably, so the
/// same commit builds the same bytes in any checkout on any machine.
fn path_remaps(
    project: &Project,
    cargo_home: Option<PathBuf>,
    toolchain: &Toolchain,
    link_crates: PathBuf,
    key: u64,
) -> PathRemaps {
    // Every flag depends only on the workspace, so that its applications
    // share their dependencies' builds.
    let mut remaps = PathRemaps::default();
    // The workspace's files keep their paths relative to it, such as
    // `src/lib.rs`, which panic messages show.
    remaps.insert(project.workspace_root.clone(), String::new());
    // Cargo passes rustc the link crate's own sources relative to it, so
    // they need a name that no file of the workspace has.
    remaps.insert_alias(
        PathBuf::from("src"),
        format!("{LINK_CRATES_NAME}/{key:016x}/src"),
    );
    if let Some(home) = cargo_home {
        remaps.insert(home, "/cargo".to_owned());
    }
    remaps.insert(project.target_directory.clone(), "/target".to_owned());
    // With `rust-src` installed, code that the standard library inlines names
    // its local copy of the sources instead of the name it was built with.
    remaps.insert(
        toolchain.library_source_directory(),
        toolchain.library_source_name(),
    );
    // Path dependencies outside the workspace, such as a Bimetal checkout, by
    // the path that Cargo passes to rustc.
    let mut names = BTreeSet::new();
    for package in &project.local_packages {
        if package.directory.starts_with(&project.workspace_root) {
            continue;
        }
        let mut name = package.name.clone();
        let mut suffix = 1;
        while !names.insert(name.clone()) {
            suffix += 1;
            name = format!("{}-{suffix}", package.name);
        }
        remaps.insert(package.directory.clone(), format!("/source/{name}"));
    }
    remaps.insert(link_crates, LINK_CRATES_NAME.to_owned());
    remaps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_crate_key_is_independent_of_checkout_root() {
        let relative = Path::new("packages/app/bimetal.toml");
        let first_root = Path::new("checkout-a");
        let second_root = Path::new("checkout-b");

        let first =
            link_crate_key(first_root, &first_root.join(relative)).expect("first link crate key");
        let second = link_crate_key(second_root, &second_root.join(relative))
            .expect("second link crate key");

        assert_eq!(first, second);
    }

    #[test]
    fn link_crate_directories_differ_between_workspaces() {
        let target = Path::new("/shared/target");

        let first = link_crates_directory(target, Path::new("/checkouts/first"));
        let second = link_crates_directory(target, Path::new("/checkouts/second"));

        assert_ne!(first, second);
        assert!(first.starts_with("/shared/target/bimetal"));
        assert_eq!(
            first,
            link_crates_directory(target, Path::new("/checkouts/first"))
        );
    }

    #[test]
    fn workspace_settings_keep_patches_profiles_and_the_resolver() {
        let workspace: toml::Table = toml::from_str(
            r#"
            [package]
            name = "app"
            edition = "2021"

            [workspace]

            [patch.crates-io]
            serde = { path = "vendor/serde" }
            itoa = { git = "https://example.com/itoa" }
            absolute = { path = "/elsewhere/absolute" }

            [patch."https://example.com/bimetal"]
            bimetal = { path = "../bimetal/packages/runtime" }

            [replace]
            "ryu:1.0.0" = { path = "vendor/ryu" }

            [profile.release]
            debug = true
            "#,
        )
        .expect("workspace manifest");
        let root = Path::new("/work/app");

        let settings = workspace_settings(&workspace, root).expect("workspace settings");

        assert_eq!(resolver(&workspace), "2");
        // Rebased paths are written with this platform's separators.
        let native = |path: &str| Some(paths::normalized(Path::new(path)).display().to_string());
        let path = |table: &str, source: &str, name: &str| {
            let mut value = &settings[table];
            if !source.is_empty() {
                value = &value[source];
            }
            value[name]
                .get("path")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        };
        assert_eq!(
            path("patch", "crates-io", "serde"),
            native("/work/app/vendor/serde")
        );
        assert_eq!(path("patch", "crates-io", "itoa"), None);
        assert_eq!(
            path("patch", "crates-io", "absolute"),
            native("/elsewhere/absolute")
        );
        assert_eq!(
            path("patch", "https://example.com/bimetal", "bimetal"),
            native("/work/bimetal/packages/runtime")
        );
        assert_eq!(
            path("replace", "", "ryu:1.0.0"),
            native("/work/app/vendor/ryu")
        );
        assert_eq!(
            settings["profile"]["release"]["debug"].as_bool(),
            Some(true)
        );
        let rendered = toml::to_string(&settings).expect("render settings");
        assert!(rendered.contains("[patch.crates-io.serde]"), "{rendered}");
    }

    #[test]
    fn resolvers_follow_cargo_defaults() {
        let resolver_of =
            |source: &str| resolver(&toml::from_str(source).expect("workspace manifest"));

        assert_eq!(resolver_of("[workspace]\nresolver = \"2\"\n"), "2");
        assert_eq!(resolver_of("[workspace]\nmembers = []\n"), "1");
        assert_eq!(resolver_of("[package]\nname = \"a\"\n"), "1");
        assert_eq!(resolver_of("[package]\nedition = \"2018\"\n"), "1");
        assert_eq!(resolver_of("[package]\nedition = \"2024\"\n"), "3");
        assert_eq!(
            resolver_of(
                "[package]\nedition.workspace = true\n[workspace.package]\nedition = \"2021\"\n"
            ),
            "2"
        );
        assert_eq!(
            resolver_of("[package]\nedition = \"2024\"\nresolver = \"1\"\n"),
            "1"
        );
        assert!(
            workspace_settings(&toml::Table::new(), Path::new("/a"))
                .expect("empty settings")
                .is_empty()
        );
    }
}
