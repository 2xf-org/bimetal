//! The build and `check` steps.

use std::fs;
use std::iter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

use crate::cargo;
use crate::cargo::dep_info::Reads;
use crate::config::{Backend, Config, Host};
use crate::contract::Contract;
use crate::dependencies::HostDependencies;
use crate::namespaces::NamespaceRule;
use crate::output::{self, InstalledNative, LocalInputs, Sources};
use crate::problems::{self, Subject};
use crate::python;
use crate::typescript;

use super::Project;
use super::compile::{Built, Compilation, CompileKind, Warnings};
use super::link::{self, LinkCrate};
use super::reports::{self, BuildReport, BuildStatus, CheckReport};
use super::{checkout, discovery, exports, validation};

/// Generated packages in a staging directory.
struct StagedOutput {
    /// The staging tree, removed when this value drops.
    directory: TempDir,
    /// The files of the checkout that the WebAssembly build, or contract
    /// discovery without one, read, which the source fingerprint covers.
    inputs: Vec<String>,
    /// What each build read.
    reads: Vec<Reads>,
    /// The packages that the generated code imports.
    dependencies: HostDependencies,
}

/// Generate, validate, and publish the packages of every enabled host, and
/// install this platform's native artifacts.
///
/// When the recorded fingerprints show that neither sources nor published
/// outputs changed, the build compiles nothing, or only the native artifacts
/// if the installed ones are missing or came from other sources, as they do
/// in a fresh checkout or after switching branches. `force` builds
/// everything regardless.
pub(crate) fn build(project: &Project, force: bool) -> Result<BuildReport<'_>> {
    let _lock = output::project_lock(&project.root, &project.package_name)?;
    let config = current_config(project)?;
    let sources = checkout::source_fingerprint(project, &config, &config.generated.inputs)?;
    if !force && up_to_date(project, &config, &sources)? {
        let artifacts = native_destinations(project);
        match output::installed_native(&project.root, &artifacts, &sources.fingerprint)? {
            InstalledNative::Current => {
                return BuildReport::new(project, BuildStatus::Unchanged, &config.generated);
            }
            InstalledNative::Outdated => {
                compile_native(project, &sources)?;
                return BuildReport::new(project, BuildStatus::Ok, &config.generated);
            }
            InstalledNative::LocalInputsChanged => {}
        }
    }
    let link_crate = link::prepare_link_crate(project)?;
    let native_kinds = project.native_kinds();
    let generated_kinds = generated_kinds(project);
    announce(project, generated_kinds.iter().chain(&native_kinds));
    let builds = generated_kinds.len() + native_kinds.len();
    let mut warnings = Warnings::default();
    // The native builds run alongside discovery and Wasm, and install after
    // the generated files.
    let native = start(project, &link_crate, &native_kinds, builds)?;
    let staged = generate(
        project,
        &config,
        &link_crate,
        &generated_kinds,
        builds,
        &mut warnings,
    )?;
    let config = current_config(project)?;
    let sources = recorded_sources(project, &config, &sources, &staged.inputs)?;
    let published = output::reconcile_generated_files(
        staged.directory.path(),
        &project.hosts().collect::<Vec<_>>(),
        &sources,
        &staged.dependencies,
        &config,
    )?;
    let skipped = LocalInputs::default();
    install_native(
        project,
        &sources,
        native,
        staged.reads,
        skipped,
        &mut warnings,
    )?;
    BuildReport::new(project, BuildStatus::Ok, &published)
}

/// Compile this platform's native artifacts from the sources with this
/// fingerprint, and install them. The caller holds the project lock.
fn compile_native(project: &Project, sources: &Sources) -> Result<()> {
    let link_crate = link::prepare_link_crate(project)?;
    let kinds = project.native_kinds();
    announce(project, &kinds);
    let builds = start(project, &link_crate, &kinds, kinds.len())?;
    // What discovery and Wasm read for these sources still holds.
    let skipped = output::recorded_local_inputs(&project.root, &sources.fingerprint)?;
    install_native(
        project,
        sources,
        builds,
        Vec::new(),
        skipped,
        &mut Warnings::default(),
    )?;
    Ok(())
}

/// Finish the native builds, install their artifacts, and record that they
/// came from the sources with this fingerprint, with what they read, what
/// the command's other builds read, `reads`, and the local inputs of the
/// builds that it skipped, `skipped`. The caller holds the project lock.
fn install_native(
    project: &Project,
    sources: &Sources,
    builds: Vec<Compilation<'_>>,
    mut reads: Vec<Reads>,
    skipped: LocalInputs,
    warnings: &mut Warnings,
) -> Result<Vec<PathBuf>> {
    let mut built = Vec::new();
    for build in builds {
        let build = build.finish(warnings)?;
        reads.push(read(project, &build)?);
        built.push(build);
    }
    let reads: Vec<&Reads> = reads.iter().collect();
    let mut local = checkout::local_inputs(project, &reads, &sources.inputs)?;
    local.extend(skipped);
    unchanged_sources(project, &current_config(project)?, sources)?;
    // Every copy waits in the scratch directory, which the next command
    // clears if this one dies, before one replaces an installed file.
    let staging = output::staging_directory(&project.root)?;
    let artifacts = project.native_artifacts();
    let mut copies = Vec::new();
    for artifact in &artifacts {
        let build = built
            .iter()
            .find(|build| build.kind == artifact.kind)
            .expect("every native artifact's build ran");
        let copy = staging.path().join(copies.len().to_string());
        fs::copy(&build.library, &copy)
            .with_context(|| format!("failed to stage {}", build.library.display()))?;
        copies.push(copy);
    }
    output::forget_native(&project.root)?;
    for (copy, artifact) in copies.iter().zip(&artifacts) {
        output::install(copy, &artifact.destination)?;
    }
    let destinations = native_destinations(project);
    output::record_native(&project.root, &destinations, &sources.fingerprint, &local)?;
    Ok(destinations)
}

/// Return where the native artifacts of the enabled hosts go.
#[must_use]
fn native_destinations(project: &Project) -> Vec<PathBuf> {
    project
        .native_artifacts()
        .into_iter()
        .map(|artifact| artifact.destination)
        .collect()
}

/// Verify that every owned output matches the current Rust sources.
///
/// Matching fingerprints prove freshness without compiling, unless `rebuild`
/// asks to compile anyway, which also proves that the build reproduces the
/// committed files. Any difference falls through to a full regeneration and
/// comparison for a precise report. No native artifact is built.
pub(crate) fn check(project: &Project, rebuild: bool) -> Result<CheckReport<'_>> {
    let _lock = output::project_lock(&project.root, &project.package_name)?;
    let mut config = current_config(project)?;
    let sources = checkout::source_fingerprint(project, &config, &config.generated.inputs)?;
    if rebuild || !up_to_date(project, &config, &sources)? {
        let link_crate = link::prepare_link_crate(project)?;
        let generated_kinds = generated_kinds(project);
        announce(project, &generated_kinds);
        let staged = generate(
            project,
            &config,
            &link_crate,
            &generated_kinds,
            generated_kinds.len(),
            &mut Warnings::default(),
        )?;
        config = current_config(project)?;
        let sources = recorded_sources(project, &config, &sources, &staged.inputs)?;
        output::check_generated_files(
            staged.directory.path(),
            &project.hosts().collect::<Vec<_>>(),
            &sources,
            &staged.dependencies,
            &config,
        )?;
    }
    Ok(CheckReport {
        application: &project.package_name,
        version: &project.package_version,
        hosts: reports::host_reports(project, &config.generated)?,
    })
}

/// Return whether the published files match the recorded fingerprints, and
/// then check the user's manifests, which no fingerprint covers, against the
/// packages that the generated code imports.
fn up_to_date(project: &Project, config: &Config, sources: &Sources) -> Result<bool> {
    if !output::generated_up_to_date(config, sources)? {
        return Ok(false);
    }
    // A list that names a package that no generated code imports, as after
    // a hand edit, takes a full build, which records the list again.
    let Some(dependencies) = recorded_dependencies(config) else {
        return Ok(false);
    };
    report(manifest_problems(project, &dependencies))?;
    Ok(true)
}

/// Return the packages that the last build recorded that the generated code
/// imports, or `None` when `bimetal.toml` names one that it cannot import.
#[must_use]
fn recorded_dependencies(config: &Config) -> Option<HostDependencies> {
    let dependencies = |host| {
        config
            .generated
            .record(host)
            .map_or(&[][..], |record| record.dependencies.as_slice())
    };
    HostDependencies::named(dependencies(Host::Python), dependencies(Host::TypeScript))
}

/// Reject Rust source changes made while the command generated outputs, so a
/// recorded fingerprint always describes the sources the outputs came from.
fn unchanged_sources(project: &Project, config: &Config, before: &Sources) -> Result<()> {
    let after = checkout::source_fingerprint(project, config, &before.inputs)?;
    if after != *before {
        bail!("Rust sources changed during the command; run it again");
    }
    Ok(())
}

/// Return the sources to record after a build: those it started from, with
/// the inputs that the build reported reading.
fn recorded_sources(
    project: &Project,
    config: &Config,
    before: &Sources,
    inputs: &[String],
) -> Result<Sources> {
    unchanged_sources(project, config, before)?;
    checkout::source_fingerprint(project, config, inputs)
}

/// Re-read configuration, reject settings changes made mid-command, and
/// reject records of paths outside the hosts' projects.
fn current_config(project: &Project) -> Result<Config> {
    let config = Config::read(project.config_path())?;
    if config.settings != project.config.settings {
        bail!("the tables of bimetal.toml changed during the command; run it again");
    }
    output::check_records(&project.root, &project.target_directory, &config.generated)?;
    Ok(config)
}

/// Return the builds that generating the packages takes: contract discovery,
/// and the Wasm module when a host runs one.
#[must_use]
fn generated_kinds(project: &Project) -> Vec<CompileKind> {
    let mut kinds = vec![CompileKind::Discovery];
    if project.builds_wasm() {
        kinds.push(CompileKind::Wasm);
    }
    kinds
}

/// Say which builds a command runs.
fn announce<'kind>(project: &Project, kinds: impl IntoIterator<Item = &'kind CompileKind>) {
    let labels: Vec<&str> = kinds.into_iter().map(|kind| kind.label()).collect();
    eprintln!("Compiling {}: {}", project.package_name, labels.join(", "));
}

/// Start the builds of `kinds`, of the `builds` that the command runs at
/// once.
fn start<'link>(
    project: &Project,
    link_crate: &'link LinkCrate,
    kinds: &[CompileKind],
    builds: usize,
) -> Result<Vec<Compilation<'link>>> {
    kinds
        .iter()
        .map(|kind| Compilation::start(project, link_crate, *kind, builds))
        .collect()
}

/// Run the builds of `kinds`, which [`generated_kinds`] returns: discover the
/// contract, and build the Wasm module when they include it. Then stage the
/// generated packages of every enabled host in a temporary directory,
/// `<stage>/<host>/`. The command runs `builds` builds at once.
fn generate(
    project: &Project,
    config: &Config,
    link_crate: &LinkCrate,
    kinds: &[CompileKind],
    builds: usize,
    warnings: &mut Warnings,
) -> Result<StagedOutput> {
    let directory = output::staging_directory(&project.root)?;
    // The builds hold no common Cargo lock: discovery and each native build
    // use their own profiles, and Wasm its own target directory.
    let wasm_build = kinds
        .contains(&CompileKind::Wasm)
        .then(|| Compilation::start(project, link_crate, CompileKind::Wasm, builds))
        .transpose()?;
    let discovery = Compilation::start(project, link_crate, CompileKind::Discovery, builds)?
        .finish(warnings)?;
    // An error here drops the slower builds, which stops them.
    let discovered = match discovery::read_contract(&discovery.library, &link_crate.package_name) {
        Ok(discovered) => discovered,
        // The manifests are checked against what the last build needed.
        Err(error) => {
            let recorded = recorded_dependencies(config).unwrap_or_default();
            return Err(combined(error, manifest_problems(project, &recorded)));
        }
    };
    // Without a namespace for each declaration there is no contract to
    // check, so the manifests are checked as after a failed discovery.
    let rule = match NamespaceRule::new(&discovered.manifest, project.folders.as_ref()) {
        Ok(rule) => rule,
        Err(error) => {
            let recorded = recorded_dependencies(config).unwrap_or_default();
            return Err(combined(error, manifest_problems(project, &recorded)));
        }
    };
    let contract = Contract::new(&discovered.manifest, rule);
    let dependencies = HostDependencies::new(&contract);
    let contract_problem = validation::validate_contract(project, &contract).err();
    report(
        contract_problem
            .into_iter()
            .chain(manifest_problems(project, &dependencies)),
    )?;
    let wasm = wasm_build.map(|build| build.finish(warnings)).transpose()?;
    if let Some(wasm) = &wasm {
        exports::check_wasm_exports(&discovered.manifest, &wasm.library)?;
    }
    // The Wasm build reads the same files on every host, so its reads are
    // the inputs that bimetal.toml records; without one, discovery's are.
    let discovery_reads = read(project, &discovery)?;
    let wasm_reads = wasm.as_ref().map(|wasm| read(project, wasm)).transpose()?;
    let inputs =
        checkout::committed_inputs(project, wasm_reads.as_ref().unwrap_or(&discovery_reads))?;
    emit_packages(
        project,
        &contract,
        &discovered.hash,
        wasm.as_ref(),
        directory.path(),
    )?;
    Ok(StagedOutput {
        directory,
        inputs,
        reads: iter::once(discovery_reads).chain(wasm_reads).collect(),
        dependencies,
    })
}

/// Write the package of every enabled host into `stage`, `<stage>/<host>/`:
/// from `contract`, whose discovery JSON has the hash `contract_hash`, and the
/// Wasm module of `wasm` for the Wasm backend.
fn emit_packages(
    project: &Project,
    contract: &Contract<'_>,
    contract_hash: &str,
    wasm: Option<&Built>,
    stage: &Path,
) -> Result<()> {
    if let Some(python) = &project.python {
        let root = stage.join(Host::Python.name());
        python::emit(&python.package, contract, contract_hash, &root)?;
    }
    if project.typescript.is_some() {
        let native = match project.backend {
            Backend::Wasm => {
                typescript::Native::Wasm(&wasm.expect("the Wasm backend builds the module").library)
            }
            Backend::Node => typescript::Native::Node { contract_hash },
        };
        let root = stage.join(Host::TypeScript.name());
        typescript::emit(&project.package_name, contract, &native, &root)?;
    }
    Ok(())
}

/// Return what one build read.
fn read(project: &Project, built: &Built) -> Result<Reads> {
    let base = cargo::dep_info::base_directory(&project.root, &project.configuration);
    cargo::dep_info::read(&built.library, &built.records, base.as_deref())
}

/// Return the problems of the enabled hosts' manifests, given the packages
/// that the generated code imports.
fn manifest_problems(
    project: &Project,
    dependencies: &HostDependencies,
) -> impl Iterator<Item = anyhow::Error> {
    let python = project
        .python
        .as_ref()
        .map(|python| validation::validate_python_project(project, python, &dependencies.python));
    let typescript = project.typescript.as_ref().map(|typescript| {
        validation::validate_typescript_project(project, typescript, &dependencies.typescript)
    });
    python.into_iter().chain(typescript).filter_map(Result::err)
}

/// Report every problem in one error, so that one build shows everything to
/// fix.
fn report(problems: impl IntoIterator<Item = anyhow::Error>) -> Result<()> {
    let mut problems = problems.into_iter();
    match problems.next() {
        Some(first) => Err(combined(first, problems)),
        None => Ok(()),
    }
}

/// Return one error that holds `first` and every other problem, or `first`
/// itself when it is the only one.
#[must_use]
fn combined(first: anyhow::Error, others: impl Iterator<Item = anyhow::Error>) -> anyhow::Error {
    let others: Vec<anyhow::Error> = others.collect();
    if others.is_empty() {
        return first;
    }
    let problems: Vec<String> = iter::once(&first)
        .chain(&others)
        .map(|problem| format!("{problem:#}"))
        .collect();
    problems::error(Subject::Application, &problems)
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn problems_are_reported_together() {
        assert!(report(iter::empty()).is_ok());
        let single = report([anyhow!("one")]).unwrap_err();
        assert_eq!(format!("{single:#}"), "one");

        let discovery = anyhow!("duplicate function name").context("invalid Bimetal application");
        let many = report([discovery, anyhow!("package.json has 2 problems:\n  - name")]);

        assert_eq!(
            format!("{:#}", many.unwrap_err()),
            "the application has 2 problems:\n  - invalid Bimetal application: duplicate function name\n  - package.json has 2 problems:\n      - name"
        );
    }
}
