//! The files that say what a build read.
//!
//! Cargo writes a dep-info file beside an artifact: a Makefile rule that
//! lists every file that the build of the artifact and of its local
//! dependencies read, including `include_str!` files and the paths that build
//! scripts watch. The environment variables are elsewhere: in the comments of
//! the dep-info file that rustc writes for each crate, which name those that
//! `env!` and `option_env!` read, and in each build script's output, which
//! names those that it watches.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::mem;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::config_files::ConfigFiles;

/// What one build read.
#[derive(Debug, Default)]
pub(crate) struct Reads {
    /// The files, and the directories that build scripts watch, by absolute
    /// path.
    pub(crate) files: Vec<PathBuf>,
    /// The environment variables that the local packages read, except those
    /// that Cargo and Bimetal set, which follow from the manifests and
    /// `bimetal.toml`.
    pub(crate) environment: BTreeSet<String>,
}

/// Read what the build of `artifact` read: the files that Cargo's dep-info
/// file beside it lists, such as `libapp.d` beside `libapp.dylib`, and the
/// environment variables that `records`, from [`local_records`], name.
///
/// `base` is the directory of Cargo's `build.dep-info-basedir`, below which
/// Cargo writes paths relative to it.
///
/// # Errors
///
/// Returns an error when a file cannot be read, or when a path is relative
/// and no `base` says where it starts.
pub(crate) fn read(artifact: &Path, records: &[PathBuf], base: Option<&Path>) -> Result<Reads> {
    let path = artifact.with_extension("d");
    let source =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut files = Vec::new();
    for file in parse(&source) {
        if file.is_absolute() {
            files.push(file);
        } else if let Some(base) = base {
            files.push(base.join(file));
        } else {
            bail!(
                "{} lists the relative path {}, but Cargo's `build.dep-info-basedir` is not set, so Bimetal cannot tell what it is relative to",
                path.display(),
                file.display()
            );
        }
    }
    let mut environment = BTreeSet::new();
    for record in records {
        let source = fs::read_to_string(record)
            .with_context(|| format!("failed to read {}", record.display()))?;
        let names = variables(&source).filter(|name| !is_set_by_cargo(name));
        environment.extend(names.map(str::to_owned));
    }
    Ok(Reads { files, environment })
}

/// Return the directory that Cargo's `build.dep-info-basedir` names, from
/// its environment variable, which is relative to `directory`, where Cargo
/// runs, or from its configuration files.
#[must_use]
pub(crate) fn base_directory(directory: &Path, config: &ConfigFiles) -> Option<PathBuf> {
    env::var_os("CARGO_BUILD_DEP_INFO_BASEDIR")
        .map(|base| directory.join(base))
        .or_else(|| config.path(&["build", "dep-info-basedir"]))
}

/// Return the files that name the environment variables that a build's
/// local packages read, from Cargo's JSON messages: the dep-info file that
/// rustc wrote for each library, such as `deps/app-1a2b.d` beside
/// `deps/libapp-1a2b.rlib`, and each build script's output.
///
/// Only a library sits in `deps/` with the hash in its name, so the link
/// crate, whose environment Bimetal sets, and build scripts, which read the
/// environment when they run, add no dep-info file.
#[must_use]
pub(crate) fn local_records(messages: &[Value]) -> Vec<PathBuf> {
    let mut records = Vec::new();
    for message in messages {
        let local = message["package_id"]
            .as_str()
            .is_some_and(|id| id.contains("path+file://"));
        if !local {
            continue;
        }
        let record = match message["reason"].as_str() {
            Some("compiler-artifact") => library_dep_info(message),
            Some("build-script-executed") => message["out_dir"]
                .as_str()
                .map(|directory| Path::new(directory).with_file_name("output")),
            _ => None,
        };
        records.extend(record);
    }
    records
}

/// Return the dep-info file that rustc wrote for the library of a
/// `compiler-artifact` message: `<crate>-<hash>.d` in the `deps` directory
/// that holds the library.
#[must_use]
fn library_dep_info(message: &Value) -> Option<PathBuf> {
    let name = message["target"]["name"].as_str()?.replace('-', "_");
    let filenames = message["filenames"].as_array()?;
    filenames.iter().filter_map(Value::as_str).find_map(|file| {
        let file = Path::new(file);
        if file.parent()?.file_name()? != "deps" {
            return None;
        }
        let stem = file.file_name()?.to_str()?.split('.').next()?;
        let (_, hash) = stem.rsplit_once('-')?;
        Some(file.with_file_name(format!("{name}-{hash}.d")))
    })
}

/// Return the environment variables that a rustc dep-info file or a build
/// script's output names: `# env-dep:NAME=value`, or `# env-dep:NAME` for
/// one that is not set, and `cargo:rerun-if-env-changed=NAME`.
fn variables(source: &str) -> impl Iterator<Item = &str> {
    source.lines().filter_map(|line| {
        if let Some(variable) = line.strip_prefix("# env-dep:") {
            return variable.split('=').next();
        }
        line.strip_prefix("cargo:rerun-if-env-changed=")
            .or_else(|| line.strip_prefix("cargo::rerun-if-env-changed="))
            .map(str::trim)
    })
}

/// Return whether Cargo or Bimetal sets an environment variable for the
/// builds, such as `CARGO_PKG_VERSION`, `OUT_DIR`, or pyo3's extension-module
/// switch.
#[must_use]
fn is_set_by_cargo(name: &str) -> bool {
    name.starts_with("CARGO")
        || name == bimetal_ir::discovery::APPLICATION_NAME_VARIABLE
        || name == bimetal_ir::discovery::APPLICATION_VERSION_VARIABLE
        || name == "OUT_DIR"
        || name == super::PYTHON_EXTENSION_VARIABLE
}

/// Return the prerequisites of every rule in a dep-info file.
///
/// A rule is `target: input input ...`, continued over lines that end with a
/// backslash. Cargo and rustc write a space in a path as `\ `, and keep every
/// other backslash, so Windows paths read as written.
#[must_use]
fn parse(source: &str) -> Vec<PathBuf> {
    let joined = source.replace("\\\r\n", " ").replace("\\\n", " ");
    let mut inputs = Vec::new();
    for line in joined.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let Some(prerequisites) = after_rule_colon(line) else {
            continue;
        };
        inputs.extend(words(prerequisites).into_iter().map(PathBuf::from));
    }
    inputs
}

/// Return the text after the colon that ends a rule's target: the first
/// colon followed by whitespace or the end of the line, unlike the colon of a
/// Windows drive.
#[must_use]
fn after_rule_colon(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b':' if bytes
                .get(index + 1)
                .is_none_or(|next| next.is_ascii_whitespace()) =>
            {
                return Some(&line[index + 1..]);
            }
            _ => index += 1,
        }
    }
    None
}

/// Split at whitespace that no backslash escapes.
#[must_use]
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' if characters.peek() == Some(&' ') => {
                current.push(' ');
                characters.next();
            }
            character if character.is_whitespace() => {
                if !current.is_empty() {
                    words.push(mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_dep_info_lists_every_input() {
        let source =
            "/target/debug/libapp.dylib: /a\\ b/src/lib.rs /a\\ b/data.txt \\\n  /a\\ b/build.rs\n";

        assert_eq!(
            parse(source),
            [
                PathBuf::from("/a b/src/lib.rs"),
                PathBuf::from("/a b/data.txt"),
                PathBuf::from("/a b/build.rs"),
            ]
        );
    }

    #[test]
    fn rustc_dep_info_rules_and_comments_add_nothing_extra() {
        let source = "/t/deps/app.d: src/lib.rs src/data.txt\n\nsrc/lib.rs:\nsrc/data.txt:\n\n# env-dep:CARGO_PKG_NAME=app\n";

        assert_eq!(
            parse(source),
            [PathBuf::from("src/lib.rs"), PathBuf::from("src/data.txt")]
        );
    }

    #[test]
    fn windows_paths_keep_drives_and_backslashes() {
        let source = "C:\\t\\app.dll: C:\\a\\src\\lib.rs C:\\a\\my\\ data.txt\r\n";

        assert_eq!(
            parse(source),
            [
                PathBuf::from("C:\\a\\src\\lib.rs"),
                PathBuf::from("C:\\a\\my data.txt"),
            ]
        );
    }

    #[test]
    fn relative_paths_start_from_the_configured_base() {
        let target = tempfile::tempdir().unwrap();
        let artifact = target.path().join("libapp.dylib");
        let absolute = target.path().join("elsewhere.txt");
        fs::write(
            artifact.with_extension("d"),
            format!(
                "{}: src/data.txt {}\n",
                artifact.display(),
                absolute.display()
            ),
        )
        .unwrap();

        let reads = read(&artifact, &[], Some(Path::new("/app"))).unwrap();
        assert_eq!(
            reads.files,
            [Path::new("/app/src/data.txt"), absolute.as_path()]
        );

        let error = read(&artifact, &[], None).unwrap_err().to_string();
        assert!(error.contains("relative path src/data.txt"), "{error}");
        assert!(error.contains("build.dep-info-basedir"), "{error}");
    }

    #[test]
    fn environment_variables_come_from_rustc_and_build_scripts() {
        let target = tempfile::tempdir().unwrap();
        let artifact = target.path().join("libapp.dylib");
        fs::write(artifact.with_extension("d"), "libapp.dylib:\n").unwrap();
        let rustc = target.path().join("app-1a2b.d");
        fs::write(
            &rustc,
            "app.d: src/lib.rs\n\n# env-dep:API_URL=https\\://example.com\n# env-dep:FEATURE\n# env-dep:CARGO_PKG_NAME=app\n# env-dep:OUT_DIR=/t/out\n",
        )
        .unwrap();
        let script = target.path().join("output");
        fs::write(
            &script,
            "cargo:rerun-if-changed=build.rs\ncargo::rerun-if-env-changed=PROTOC\ncargo:rerun-if-env-changed=CARGO_CFG_UNIX\n",
        )
        .unwrap();

        let reads = read(&artifact, &[rustc, script], None).unwrap();

        assert_eq!(
            reads.environment,
            BTreeSet::from(["API_URL", "FEATURE", "PROTOC"].map(str::to_owned))
        );
    }

    #[test]
    fn records_are_found_for_local_libraries_and_build_scripts() {
        let messages: Vec<Value> = [
            r#"{"reason":"compiler-artifact","package_id":"path+file:///a/app#0.1.0","target":{"name":"my-app"},"filenames":["/t/debug/libmy_app.rlib","/t/debug/deps/libmy_app-1a2b.rmeta"]}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///a/macros#0.1.0","target":{"name":"macros"},"filenames":["/t/debug/deps/macros-3c4d.dll"]}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///a/app#0.1.0","target":{"name":"build-script-build"},"filenames":["/t/debug/build/app-5e6f/build-script-build"]}"#,
            r#"{"reason":"build-script-executed","package_id":"path+file:///a/app#0.1.0","out_dir":"/t/debug/build/app-7a8b/out"}"#,
            r#"{"reason":"compiler-artifact","package_id":"registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0","target":{"name":"serde"},"filenames":["/t/debug/deps/libserde-9c0d.rlib"]}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///a/link#0.0.0","target":{"name":"link"},"filenames":["/t/debug/liblink.dylib"]}"#,
        ]
        .iter()
        .map(|message| serde_json::from_str(message).unwrap())
        .collect();

        assert_eq!(
            local_records(&messages),
            [
                PathBuf::from("/t/debug/deps/my_app-1a2b.d"),
                PathBuf::from("/t/debug/deps/macros-3c4d.d"),
                PathBuf::from("/t/debug/build/app-7a8b/output"),
            ]
        );
    }
}
