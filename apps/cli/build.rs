//! Hash the generator's sources into the binary.
//!
//! An application's source fingerprint includes this hash, so a change to the
//! code that renders packages makes every application rebuild once, even when
//! the version number stays the same. The generator is this crate's sources
//! and its version, which decides the crates it renders with, because the
//! manifests pin them exactly. Both are the same in a checkout and in the
//! crate that crates.io serves, so every build of one release has one hash.
//! Line endings are normalized so that the hash is the same on every platform.

use std::path::{Path, PathBuf};
use std::{env, fs};

use bimetal_ir::hash;

fn main() {
    // Read at run time: Cargo reuses this script after a checkout moves, and
    // `env!` would keep the old path.
    let manifest = env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR");
    let sources = Path::new(&manifest).join("src");
    let mut files = Vec::new();
    collect(&sources, &mut files);
    files.sort();
    let version = env::var("CARGO_PKG_VERSION").expect("Cargo sets CARGO_PKG_VERSION");
    // The version, then each file as its relative path and its source, each
    // ended by a NUL.
    let mut records: Vec<u8> = version.bytes().chain([0]).collect();
    for path in &files {
        let relative = path
            .strip_prefix(&sources)
            .expect("generator sources are inside the source directory")
            .to_string_lossy()
            .replace('\\', "/");
        let source = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
            .replace("\r\n", "\n");
        records.extend(relative.bytes().chain([0]).chain(source.bytes()).chain([0]));
    }
    println!(
        "cargo:rustc-env=BIMETAL_GENERATOR_FINGERPRINT={:016x}",
        hash::fnv1a(&records)
    );
    println!("cargo:rerun-if-changed={}", sources.display());
}

/// The extensions of the sources below `src`: Rust, and the runtimes that
/// the generator includes in the packages. Other files, such as an editor's,
/// must not change the hash.
const SOURCE_EXTENSIONS: [&str; 4] = ["rs", "py", "js", "ts"];

/// Append every source below `directory` to `files`, in no particular order.
fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("the source directory is readable") {
        let path = entry.expect("source entries are readable").path();
        if path.is_dir() {
            collect(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| SOURCE_EXTENSIONS.iter().any(|known| extension == *known))
        {
            files.push(path);
        }
    }
}
