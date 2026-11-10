//! Compiling the targets of the link crate, several at once.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, IsTerminal};
use std::num::NonZero;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use std::{env, panic};

use anyhow::{Context, Result, bail};
use bimetal_ir::discovery;

use crate::cargo::{self, Resolution, messages, rustc_wrapper};
use crate::output::{self, Platform};

use super::Project;
use super::link::LinkCrate;

/// The target triple of the Wasm build.
const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Linker arguments for every macOS library that Bimetal builds, which only
/// the link crate takes. The linker drops the debug symbols: Apple's `strip`,
/// which rustc would run instead, can misalign the string table, and dyld
/// then refuses the library.
const MACOS_LIBRARY_ARGUMENTS: [&str; 4] = ["-C", "strip=none", "-C", "link-arg=-Wl,-S"];

/// The macOS linker arguments that only the Python extension takes, whose
/// libpython symbols the interpreter provides when it loads the extension.
const MACOS_EXTENSION_ARGUMENTS: [&str; 4] =
    ["-C", "link-arg=-undefined", "-C", "link-arg=dynamic_lookup"];

/// The profile of the discovery build, which runs once to describe the
/// contract, and so skips optimization and keeps incremental state between
/// edits. It inherits the release profile of the extension, so that both see
/// the same `cfg(debug_assertions)`, and its own directories hold no Cargo
/// lock that the extension waits for.
const DISCOVERY_PROFILE: &str = "bimetal-discovery";

/// The settings of [`DISCOVERY_PROFILE`].
const DISCOVERY_SETTINGS: [&str; 7] = [
    "inherits='release'",
    "opt-level=0",
    "debug=0",
    "lto=false",
    "codegen-units=256",
    "incremental=true",
    // A panic during discovery must not abort the `bimetal` command, which
    // loads the library.
    "panic='unwind'",
];

/// The profile of the Node.js addon. Its own directories hold no Cargo lock
/// that the Python extension, which keeps the release profile, waits for.
const NATIVE_PROFILE: &str = "bimetal-native";

/// The settings of [`NATIVE_PROFILE`]. The addon reports a panic only if it
/// unwinds, whatever the application's profile says.
const NATIVE_SETTINGS: [&str; 2] = ["inherits='release'", "panic='unwind'"];

/// The directory of the Wasm builds in Bimetal's part of the target directory.
/// Cargo fingerprints neither the rustc wrapper nor what it changes, so the
/// name changes whenever the wrapper compiles differently, and a Wasm build
/// never links what an older wrapper compiled.
const WASM_TARGET_DIRECTORY: &str = "wasm-2";

/// Target-specific build of the link crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompileKind {
    Discovery,
    Python,
    Wasm,
    /// The Node.js addon.
    Native,
}

impl CompileKind {
    /// Return the name of the build in messages.
    #[must_use]
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Discovery => "contract discovery",
            Self::Python => "Python extension",
            Self::Wasm => "WebAssembly module",
            Self::Native => "Node.js addon",
        }
    }

    /// Say that the application's code does not compile for this build.
    #[must_use]
    const fn compile_error(self) -> &'static str {
        match self {
            Self::Discovery => "your crate does not compile",
            Self::Python => "your crate does not compile into the Python extension",
            Self::Wasm => "your crate does not compile for WebAssembly",
            Self::Native => "your crate does not compile into the Node.js addon",
        }
    }
}

/// Return the jobs of one of `builds` builds that run at once: an even share
/// of half again as many jobs as the machine has cores, so that the builds
/// waiting on one crate leave no core idle, and at most one per core.
#[must_use]
fn jobs(builds: usize) -> usize {
    let cores = thread::available_parallelism().map_or(1, NonZero::get);
    (cores * 3).div_ceil(2 * builds.max(1)).min(cores)
}

/// One running Cargo build of a link crate target.
///
/// Cargo writes its JSON messages to an anonymous file, and a thread reads its
/// own output as it comes, so concurrent builds never stall on unread output.
/// Dropping an unfinished build kills Cargo and waits for it to exit, so a
/// failure elsewhere stops the build promptly.
pub(super) struct Compilation<'link> {
    link: &'link LinkCrate,
    kind: CompileKind,
    started: Instant,
    child: Child,
    messages: File,
    /// The thread that reads Cargo's own output; see [`messages::read_output`].
    diagnostics: Option<JoinHandle<io::Result<Vec<u8>>>>,
}

impl<'link> Compilation<'link> {
    /// Start compiling one link crate target, as one of `builds` builds that
    /// the command runs at once.
    pub(super) fn start(
        project: &Project,
        link: &'link LinkCrate,
        kind: CompileKind,
        builds: usize,
    ) -> Result<Self> {
        let mut command = Command::new(cargo::executable());
        // The link crate's lockfile is complete, so every build is locked.
        let resolution = Resolution {
            locked: true,
            ..project.resolution
        };
        // Bimetal reads Cargo's own messages, so they stay plain, and shows
        // rustc's diagnostics, in colour on a terminal.
        let message_format = if io::stderr().is_terminal() {
            "--message-format=json-diagnostic-rendered-ansi"
        } else {
            "--message-format=json"
        };
        command
            .args(["rustc", "--lib", "--color", "never", message_format])
            .args(resolution.flags())
            .arg("--manifest-path")
            .arg(&link.manifest);
        // Each build's own jobserver would start a job per core, and the
        // builds together several times as many rustc processes as cores.
        if env::var_os("CARGO_BUILD_JOBS").is_none()
            && !project.configuration.sets(&["build", "jobs"])
        {
            command.args(["--jobs", &jobs(builds).to_string()]);
        }
        let target_directory = select_target(&mut command, link, kind);
        let library = project.shared_library_name(kind);
        let link_arguments = link_arguments(Platform::CURRENT, kind, library.as_deref());
        if !link_arguments.is_empty() {
            // They reach the link crate alone, so every dependency keeps the
            // flags of the user's own builds.
            command.arg("--").args(link_arguments);
        }
        rustc_wrapper::wrap(
            &mut command,
            &link.remap_flags,
            link.rustc_wrapper.as_deref(),
        )?;
        let messages = tempfile::tempfile()?;
        let mut child = command
            // Cargo reads the `.cargo/config.toml` files of the directory it
            // runs in, so every build of the application reads the same ones.
            .current_dir(&project.root)
            .env("CARGO_TARGET_DIR", target_directory)
            .env(discovery::APPLICATION_NAME_VARIABLE, &project.package_name)
            .env(
                discovery::APPLICATION_VERSION_VARIABLE,
                &project.package_version,
            )
            .stdin(Stdio::null())
            .stdout(messages.try_clone()?)
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to start the {} build", kind.label()))?;
        let output = child
            .stderr
            .take()
            .context("Cargo's output is not readable")?;
        let label = kind.label();
        Ok(Self {
            link,
            kind,
            started: Instant::now(),
            child,
            messages,
            diagnostics: Some(thread::spawn(move || messages::read_output(output, label))),
        })
    }

    /// Wait for the build, print the warnings that `warnings` has not shown,
    /// and return the libraries that Cargo reported.
    pub(super) fn finish(mut self, warnings: &mut Warnings) -> Result<Built> {
        let label = self.kind.label();
        let status = self
            .child
            .wait()
            .with_context(|| format!("failed to compile the {label}"))?;
        let messages = messages::parse(&read_all(&mut self.messages)?);
        // rustc wrote the portable names; show the files where they are.
        let remaps = &self.link.remaps;
        if !status.success() {
            let output = match self.diagnostics.take().map(JoinHandle::join) {
                Some(Ok(output)) => output?,
                Some(Err(panic)) => panic::resume_unwind(panic),
                None => Vec::new(),
            };
            let errors = messages::compiler_messages(&messages, |level| level.starts_with("error"));
            let summary = if errors.is_empty() {
                format!("Cargo could not build the {label}")
            } else {
                self.kind.compile_error().to_owned()
            };
            let diagnostics = messages::compiler_messages(&messages, |_| true).concat();
            let cargo = messages::errors(&output);
            bail!(
                "{summary}:\n{}{}",
                remaps.restore_paths(&diagnostics),
                remaps.restore_paths(&cargo)
            );
        }
        let files = messages::cdylib_files(&messages, &self.link.package_id);
        let Some(library) = files.into_iter().find(|path| output::is_library(path)) else {
            bail!(
                "Cargo did not report the {label} cdylib for `{}`",
                self.link.package_name
            );
        };
        let built = Built {
            kind: self.kind,
            library,
            records: cargo::dep_info::local_records(&messages),
        };
        for warning in messages::compiler_messages(&messages, |level| level == "warning") {
            warnings.show(remaps.restore_paths(&warning));
        }
        eprintln!(
            "  Compiled the {label} in {:.1}s",
            self.started.elapsed().as_secs_f64()
        );
        Ok(built)
    }
}

impl Drop for Compilation<'_> {
    fn drop(&mut self) {
        // A finished build has exited, so only an unfinished one is killed.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Add the arguments that select the target, profile, and features of a
/// build of `kind` to `command`, and return its target directory.
fn select_target(command: &mut Command, link: &LinkCrate, kind: CompileKind) -> PathBuf {
    match kind {
        CompileKind::Discovery => {
            command.args(["--target", &link.host, "--profile", DISCOVERY_PROFILE]);
            for setting in DISCOVERY_SETTINGS {
                command.args([
                    "--config",
                    &format!("profile.{DISCOVERY_PROFILE}.{setting}"),
                ]);
            }
        }
        CompileKind::Python => {
            // Only the extension enables `python`, so no binary links
            // libpython. `PanicException` needs a panic that unwinds.
            command
                .args([
                    "--target",
                    &link.host,
                    "--release",
                    "--features",
                    "bimetal/python",
                    "--config",
                    "profile.release.panic='unwind'",
                ])
                .env(cargo::PYTHON_EXTENSION_VARIABLE, "1");
        }
        CompileKind::Wasm => {
            command.args([
                "--target",
                WASM_TARGET,
                "--release",
                "--config",
                &format!(
                    "target.{WASM_TARGET}.rustflags=['--cfg','getrandom_backend=\"wasm_js\"']"
                ),
            ]);
            // The module is downloaded, so optimize it for size across
            // crates. These settings change only Wasm fingerprints.
            for setting in ["opt-level='s'", "lto=true", "codegen-units=1"] {
                command.args(["--config", &format!("profile.release.{setting}")]);
            }
            // A separate target directory has its own Cargo lock, so Wasm
            // builds alongside the host-native targets.
            return link
                .target_directory
                .join("bimetal")
                .join(WASM_TARGET_DIRECTORY);
        }
        CompileKind::Native => {
            command.args([
                "--target",
                &link.host,
                "--profile",
                NATIVE_PROFILE,
                "--features",
                "bimetal/node",
            ]);
            for setting in NATIVE_SETTINGS {
                command.args(["--config", &format!("profile.{NATIVE_PROFILE}.{setting}")]);
            }
        }
    }
    link.target_directory.clone()
}

/// Return the arguments that only the link crate's rustc takes: on macOS,
/// the linker arguments of every library that Bimetal builds, and the name
/// that it installs `library` under. The discovery library is loaded where
/// it is built, so it has no such name.
#[must_use]
fn link_arguments(platform: Platform, kind: CompileKind, library: Option<&str>) -> Vec<String> {
    let mut arguments = Vec::new();
    if platform != Platform::MacOs || kind == CompileKind::Wasm {
        return arguments;
    }
    if kind == CompileKind::Python {
        arguments.extend(MACOS_EXTENSION_ARGUMENTS.map(str::to_owned));
    }
    arguments.extend(MACOS_LIBRARY_ARGUMENTS.map(str::to_owned));
    if let Some(library) = library {
        arguments.extend([
            "-C".to_owned(),
            format!("link-arg=-Wl,-install_name,@rpath/{library}"),
        ]);
    }
    arguments
}

/// Read a build's output file from its start.
fn read_all(file: &mut File) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut contents = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut contents)?;
    Ok(contents)
}

/// A finished build of one link crate target.
pub(super) struct Built {
    pub(super) kind: CompileKind,
    /// The shared library, or the Wasm module, that the build produced, beside
    /// which Cargo writes the dep-info file of the build.
    pub(super) library: PathBuf,
    /// The files that name the environment variables that the build's local
    /// packages read; see [`cargo::dep_info::local_records`].
    pub(super) records: Vec<PathBuf>,
}

/// The warnings that the builds of one command have shown. The builds of
/// every target compile the same code, so each shows only the warnings that
/// no other build has shown, such as those of code for Wasm alone.
#[derive(Default)]
pub(super) struct Warnings {
    shown: BTreeSet<String>,
}

impl Warnings {
    /// Print a rendered warning unless a build showed it already.
    fn show(&mut self, rendered: String) {
        if !self.shown.contains(&rendered) {
            eprint!("{rendered}");
            self.shown.insert(rendered);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_builds_share_the_cores() {
        let cores = thread::available_parallelism().map_or(1, NonZero::get);
        assert_eq!(jobs(1), cores);
        assert_eq!(jobs(cores * 2), 1);
    }

    #[test]
    fn every_macos_library_skips_apples_strip() {
        let strips = |arguments: &[String]| {
            arguments
                .windows(2)
                .any(|pair| pair[0] == "-C" && pair[1] == "strip=none")
        };
        for kind in [
            CompileKind::Discovery,
            CompileKind::Python,
            CompileKind::Native,
        ] {
            assert!(
                strips(&link_arguments(Platform::MacOs, kind, None)),
                "the {} would go through Apple's strip",
                kind.label()
            );
        }
        let extension = link_arguments(Platform::MacOs, CompileKind::Python, Some("_native.so"));
        assert!(extension.contains(&"link-arg=-Wl,-install_name,@rpath/_native.so".to_owned()));
        assert!(link_arguments(Platform::MacOs, CompileKind::Wasm, None).is_empty());
        assert!(
            link_arguments(Platform::Linux, CompileKind::Python, Some("_native.so")).is_empty()
        );
    }
}
