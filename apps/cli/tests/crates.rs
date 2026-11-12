//! The crates that crates.io serves: the package of each crate that the workspace publishes holds
//! `LICENSE-MIT` and `LICENSE-APACHE`, each a copy of the file at the root. Cargo copies a
//! `readme` from outside a package but no second license file, so each crate keeps its own copy.

// Tests unwrap; Clippy's test exemption doesn't reach the helpers of an integration test.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The license files that each published crate ships.
const LICENSES: [&str; 2] = ["LICENSE-MIT", "LICENSE-APACHE"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The folder and the name of each workspace member that crates.io serves. Cargo resolves
/// `publish.workspace = true` and the globs in `members`, and writes `publish = false` as `[]`.
fn published_crates() -> Vec<(PathBuf, String)> {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
        ])
        .current_dir(root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut crates = Vec::new();
    for package in metadata["packages"].as_array().unwrap() {
        // `null` allows every registry, and a list names the registries that may serve it.
        let published = match &package["publish"] {
            serde_json::Value::Null => true,
            serde_json::Value::Array(registries) => {
                registries.iter().any(|name| name == "crates-io")
            }
            other => panic!("`publish` is null or a list, not {other}"),
        };
        if !published {
            continue;
        }
        let manifest = Path::new(package["manifest_path"].as_str().unwrap());
        let name = package["name"].as_str().unwrap().to_owned();
        crates.push((manifest.parent().unwrap().to_owned(), name));
    }
    crates
}

#[test]
fn each_published_crate_ships_both_licenses() {
    let crates = published_crates();
    assert!(!crates.is_empty(), "the workspace publishes crates");
    for (folder, name) in crates {
        for license in LICENSES {
            assert_eq!(
                fs::read(folder.join(license)).unwrap_or_default(),
                fs::read(root().join(license)).unwrap(),
                "`{name}` holds a copy of the root's {license}"
            );
        }
        let output = Command::new(env!("CARGO"))
            .args([
                "package",
                "--list",
                "--allow-dirty",
                "--offline",
                "-p",
                &name,
            ])
            .current_dir(root())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cargo package --list -p {name} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let listed = String::from_utf8(output.stdout).unwrap();
        for license in LICENSES {
            assert!(
                listed.lines().any(|line| line == license),
                "the package of `{name}` lists {license}:\n{listed}"
            );
        }
    }
}
