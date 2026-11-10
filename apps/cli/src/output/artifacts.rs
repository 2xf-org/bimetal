//! The names of the native artifacts that a build installs beside the
//! generated files: the Python extension and the Node.js addon.
//!
//! Each platform builds its own, so no `[generated]` record lists them. A
//! build installs this platform's, and publication deletes every platform's
//! once the generated files that imply them are stale.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use bimetal_ir::python_names::NATIVE_MODULE;

use crate::config::Host;
use crate::typescript::{NATIVE_DIRECTORY, NODE_ADDON};

/// Return whether a file that a build produced is its library: a `.dylib`,
/// `.so`, or `.dll`, or the Wasm module, and not what Windows writes beside
/// a `.dll`, its import library and debug symbols.
#[must_use]
pub(crate) fn is_library(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| matches!(extension, "dylib" | "so" | "dll" | "wasm"))
}

/// An operating system that names native artifacts its own way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    Linux,
    MacOs,
    Windows,
}

impl Platform {
    /// Every platform.
    pub(crate) const ALL: [Self; 3] = [Self::Linux, Self::MacOs, Self::Windows];

    /// The platform that `bimetal` runs on, whose artifacts its builds install.
    pub(crate) const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else if cfg!(target_os = "macos") {
        Self::MacOs
    } else {
        Self::Linux
    };
}

/// Return the native artifact of `host` on `platform`, relative to the host's
/// project. `package` places it: for Python, the import package's directory,
/// such as `my_app/stats`; and for TypeScript, which has one only with the
/// Node.js backend, the package's directory.
#[must_use]
pub(crate) fn native_file(host: Host, package: &str, platform: Platform) -> PathBuf {
    match host {
        Host::Python => {
            // CPython's stable ABI on Unix, and Windows' own suffix.
            let suffix = match platform {
                Platform::Linux | Platform::MacOs => "abi3.so",
                Platform::Windows => "pyd",
            };
            Path::new(package).join(format!("{NATIVE_MODULE}.{suffix}"))
        }
        Host::TypeScript => Path::new(package).join(NATIVE_DIRECTORY).join(NODE_ADDON),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_library_is_its_shared_file() {
        for library in ["/t/link.dll", "/t/liblink.so", "/t/native_bg.wasm"] {
            assert!(is_library(Path::new(library)), "{library}");
        }
        for other in ["/t/link.dll.lib", "/t/link.pdb", "/t/link"] {
            assert!(!is_library(Path::new(other)), "{other}");
        }
    }
}
