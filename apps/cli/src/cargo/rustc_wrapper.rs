//! The `bimetal` command as the `RUSTC_WRAPPER` of the link crate's builds.
//!
//! In every compilation for the build's target, the wrapper adds Bimetal's
//! flags, which give local directories portable names (see `remap`), after
//! the user's, replaces `-C metadata` with a hash of what names the crate in
//! every checkout, and then runs the user's own wrapper, such as sccache.
//! Build scripts and procedural macros pass through unchanged.
//! `docs/internals/reproducible.md` says why.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use bimetal_ir::hash;

/// The variable that holds Bimetal's flags for the wrapper, separated as in
/// `CARGO_ENCODED_RUSTFLAGS`. A `bimetal` process that has it is the wrapper.
const FLAGS: &str = "BIMETAL_RUSTC_FLAGS";

/// The variable that names the user's own wrapper.
const USER_WRAPPER: &str = "BIMETAL_RUSTC_WRAPPER";

/// The separator of the flags in [`FLAGS`].
const SEPARATOR: &str = "\u{1f}";

/// Make the Cargo build `command` run rustc through this executable, which
/// adds `flags`, and then through `user_wrapper`.
///
/// # Errors
///
/// Returns an error when the running executable cannot be found.
pub(crate) fn wrap(
    command: &mut Command,
    flags: &[String],
    user_wrapper: Option<&Path>,
) -> Result<()> {
    let executable = env::current_exe().context("cannot find the bimetal executable")?;
    command
        .env("RUSTC_WRAPPER", executable)
        .env(FLAGS, flags.join(SEPARATOR));
    match user_wrapper {
        Some(wrapper) => command.env(USER_WRAPPER, wrapper),
        None => command.env_remove(USER_WRAPPER),
    };
    Ok(())
}

/// Run rustc for the build that [`wrap`] set up, if it started this process,
/// and return its exit status.
#[must_use]
pub(crate) fn run() -> Option<i32> {
    let flags = env::var(FLAGS).ok()?;
    // Cargo runs `bimetal rustc ...`, or `bimetal <workspace wrapper> rustc ...`.
    let mut arguments = env::args_os().skip(1);
    let Some(program) = env::var_os(USER_WRAPPER).or_else(|| arguments.next()) else {
        eprintln!("error: Cargo ran bimetal as its rustc wrapper without a compiler");
        return Some(1);
    };
    let mut arguments: Vec<OsString> = arguments.collect();
    let flags: Vec<&str> = flags.split(SEPARATOR).collect();
    let expanded = expand(&arguments);
    let for_target = compiles_for_target(&expanded);
    // Cargo gives every compilation the directory of its package.
    if for_target && let Some(manifest_directory) = env::var_os("CARGO_MANIFEST_DIR") {
        let metadata = portable_metadata(&expanded, Path::new(&manifest_directory), &flags);
        if let Err(error) = set_metadata(&mut arguments, &metadata) {
            eprintln!("error: {error:#}");
            return Some(1);
        }
    }
    let mut command = Command::new(&program);
    command.args(&arguments);
    if for_target {
        command.args(&flags);
    }
    Some(execute(command, &program))
}

/// Return the arguments of a rustc call that are UTF-8, with the lines of each
/// `@path` file in its place. Cargo moves the arguments into such a file, one
/// per line, when the command line is too long.
#[must_use]
fn expand(arguments: &[OsString]) -> Vec<String> {
    let mut expanded = Vec::new();
    for argument in arguments.iter().filter_map(|argument| argument.to_str()) {
        match argument.strip_prefix('@') {
            Some(file) => {
                let contents = fs::read_to_string(file).unwrap_or_default();
                expanded.extend(contents.lines().map(str::to_owned));
            }
            None => expanded.push(argument.to_owned()),
        }
    }
    expanded
}

/// Return whether rustc compiles for an explicit target, as Cargo has it do
/// for each crate of a build that names one.
#[must_use]
fn compiles_for_target(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| argument == "--target" || argument.starts_with("--target="))
}

/// Set the `-C metadata` among `arguments`, and in the `@path` files that they
/// name, to `metadata`.
///
/// # Errors
///
/// Returns an error when an `@path` file cannot be rewritten.
fn set_metadata(arguments: &mut [OsString], metadata: &str) -> Result<()> {
    replace_metadata(arguments, metadata);
    for argument in arguments.iter() {
        let Some(file) = argument
            .to_str()
            .and_then(|argument| argument.strip_prefix('@'))
        else {
            continue;
        };
        let contents =
            fs::read_to_string(file).with_context(|| format!("failed to read {file}"))?;
        let mut lines: Vec<OsString> = contents.lines().map(OsString::from).collect();
        if replace_metadata(&mut lines, metadata) {
            let mut rewritten = String::with_capacity(contents.len());
            for line in lines {
                rewritten.push_str(line.to_str().expect("the lines of a string are UTF-8"));
                rewritten.push('\n');
            }
            fs::write(file, rewritten).with_context(|| format!("failed to rewrite {file}"))?;
        }
    }
    Ok(())
}

/// Return the `-C metadata` of the crate in `manifest_directory` that
/// `arguments` compile, which hashes only what every host and checkout sees
/// the same.
#[must_use]
fn portable_metadata(arguments: &[String], manifest_directory: &Path, flags: &[&str]) -> String {
    let mut identity = portable_path(manifest_directory, flags);
    for (index, argument) in arguments.iter().enumerate() {
        for option in ["--crate-name", "--crate-type", "--cfg"] {
            let value = if argument == option {
                arguments.get(index + 1).map(String::as_str)
            } else {
                argument
                    .strip_prefix(option)
                    .and_then(|value| value.strip_prefix('='))
            };
            if let Some(value) = value {
                identity.push_str(&format!("\0{option}={value}"));
            }
        }
    }
    format!("{:016x}", hash::fnv1a(identity.as_bytes()))
}

/// Write `path` as rustc does under the `--remap-path-prefix` flags among
/// `flags`, which apply the last mapping that matches, with `/` between its
/// components.
#[must_use]
fn portable_path(path: &Path, flags: &[&str]) -> String {
    let remap = flags.iter().rev().find_map(|flag| {
        let (local, portable) = flag
            .strip_prefix("--remap-path-prefix=")?
            .rsplit_once('=')?;
        Some((portable, path.strip_prefix(local).ok()?))
    });
    let Some((portable, below)) = remap else {
        return path.to_string_lossy().replace('\\', "/");
    };
    let mut written = portable.to_owned();
    for component in below {
        if !written.is_empty() && !written.ends_with('/') {
            written.push('/');
        }
        written.push_str(&component.to_string_lossy());
    }
    written
}

/// Set each `-C metadata` value among `arguments` to `metadata`, and return
/// whether there was one.
fn replace_metadata(arguments: &mut [OsString], metadata: &str) -> bool {
    let mut replaced = false;
    let mut codegen_option = false;
    for argument in arguments {
        let text = argument.to_str().unwrap_or_default();
        let prefix = if codegen_option {
            Some("metadata=").filter(|prefix| text.starts_with(prefix))
        } else {
            ["-Cmetadata=", "--codegen=metadata="]
                .into_iter()
                .find(|prefix| text.starts_with(prefix))
        };
        codegen_option = matches!(text, "-C" | "--codegen");
        if let Some(prefix) = prefix {
            *argument = format!("{prefix}{metadata}").into();
            replaced = true;
        }
    }
    replaced
}

/// Replace this process with `command`.
#[cfg(unix)]
fn execute(mut command: Command, program: &OsStr) -> i32 {
    use std::os::unix::process::CommandExt;

    let error = command.exec();
    eprintln!("error: failed to run {}: {error}", program.display());
    1
}

/// Run `command`, and return its exit status.
#[cfg(not(unix))]
fn execute(mut command: Command, program: &OsStr) -> i32 {
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("error: failed to run {}: {error}", program.display());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn for_target(values: &[&str]) -> bool {
        compiles_for_target(&expand(&arguments(values)))
    }

    #[test]
    fn only_compilations_for_the_target_take_the_flags() {
        assert!(for_target(&[
            "rustc",
            "--crate-name",
            "app",
            "--target",
            "wasm32-unknown-unknown",
        ]));
        assert!(for_target(&["rustc", "--target=wasm32-unknown-unknown"]));
        // A build script, a procedural macro, and Cargo's version query.
        assert!(!for_target(&[
            "rustc",
            "--crate-name",
            "build_script_build"
        ]));
        assert!(!for_target(&["rustc", "-vV"]));

        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("arguments");
        fs::write(
            &file,
            "--crate-name\napp\n--target\nwasm32-unknown-unknown\n",
        )
        .unwrap();
        let argument = format!("@{}", file.display());
        assert!(for_target(&["rustc", &argument]));
    }

    #[test]
    fn metadata_is_the_same_wherever_the_build_runs() {
        let compilation = |features: &str| {
            strings(&[
                "--crate-name",
                "serde",
                "--crate-type",
                "lib",
                "--cfg",
                features,
                "-C",
                "metadata=2b331c2f8acacdca",
                "--target",
                "wasm32-unknown-unknown",
            ])
        };
        let macos = ["--remap-path-prefix=/Users/ada/.cargo=/cargo"];
        let linux = ["--remap-path-prefix=/home/runner/.cargo=/cargo"];
        let package = |home: &str, version: &str| {
            Path::new(home).join(format!(
                "registry/src/index.crates.io-1949cf8c6b5b557f/serde-{version}"
            ))
        };
        let std = compilation("feature=\"std\"");
        let metadata = portable_metadata(&std, &package("/Users/ada/.cargo", "1.0.229"), &macos);

        assert_eq!(
            portable_metadata(&std, &package("/home/runner/.cargo", "1.0.229"), &linux),
            metadata
        );
        // Another version or other features make another crate.
        assert_ne!(
            portable_metadata(&std, &package("/Users/ada/.cargo", "1.0.230"), &macos),
            metadata
        );
        assert_ne!(
            portable_metadata(
                &compilation("feature=\"alloc\""),
                &package("/Users/ada/.cargo", "1.0.229"),
                &macos
            ),
            metadata
        );
    }

    #[test]
    fn metadata_is_replaced_on_the_command_line_and_in_argument_files() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("arguments");
        fs::write(
            &file,
            "--crate-name\napp\n-C\nmetadata=0123\n--target\nwasm32-unknown-unknown\n",
        )
        .unwrap();
        let flags = ["--remap-path-prefix=/work/app="];
        let mut command_line =
            arguments(&["rustc", "-Cmetadata=4567", &format!("@{}", file.display())]);
        let metadata = portable_metadata(&expand(&command_line), Path::new("/work/app"), &flags);
        set_metadata(&mut command_line, &metadata).unwrap();

        assert_eq!(
            command_line[1],
            OsString::from(format!("-Cmetadata={metadata}"))
        );
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            format!(
                "--crate-name\napp\n-C\nmetadata={metadata}\n--target\nwasm32-unknown-unknown\n"
            )
        );
    }
}
