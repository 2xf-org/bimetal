//! What the rustc of a build reports about itself.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::config_files::ConfigFiles;

/// The rustc that Cargo runs for an application.
#[derive(Debug, Clone)]
pub(crate) struct Toolchain {
    /// The host target triple.
    host: String,
    /// What `rustc -vV` reports, except the host.
    identity: String,
    /// The rust-lang/rust commit the compiler was built from.
    commit_hash: Option<String>,
    /// The version, such as `1.94.0`.
    release: String,
    /// The toolchain directory.
    sysroot: PathBuf,
}

impl Toolchain {
    /// Ask the rustc that Cargo runs in `directory`: the one that `RUSTC` or
    /// `build.rustc` names, or `rustc` from the path. Running it in the same
    /// directory as Cargo lets rustup choose the same toolchain.
    ///
    /// # Errors
    ///
    /// Returns an error when rustc cannot run or reports no host or version.
    pub(crate) fn query(directory: &Path, config: &ConfigFiles) -> Result<Self> {
        let executable = config
            .tool(directory, "rustc")
            .map_or_else(|| OsString::from("rustc"), PathBuf::into_os_string);
        let version = run(&executable, directory, &["-vV"])?;
        let field = |name: &str| {
            version
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .map(|value| value.trim().to_owned())
        };
        let host = field("host:").context("rustc -vV reported no host")?;
        check_host(&host)?;
        let release = field("release:").context("rustc -vV reported no release")?;
        let commit_hash = field("commit-hash:").filter(|hash| hash != "unknown");
        let identity: Vec<&str> = version
            .lines()
            .filter(|line| !line.starts_with("host:"))
            .collect();
        let sysroot = PathBuf::from(run(&executable, directory, &["--print", "sysroot"])?.trim());
        Ok(Self {
            host,
            identity: identity.join("\n"),
            commit_hash,
            release,
            sysroot,
        })
    }

    /// Return the host target triple.
    #[must_use]
    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    /// Return what identifies the compiler on every host that has it: what
    /// `rustc -vV` reports, such as the release, commit, and LLVM version,
    /// except the host.
    #[must_use]
    pub(crate) fn identity(&self) -> &str {
        &self.identity
    }

    /// Return the directory that rustc reads the standard library's sources
    /// from when the `rust-src` component is installed. Code that the standard
    /// library inlines into a crate then names its files there.
    #[must_use]
    pub(crate) fn library_source_directory(&self) -> PathBuf {
        self.sysroot
            .join("lib")
            .join("rustlib")
            .join("src")
            .join("rust")
    }

    /// Return the directory name that the standard library was built with,
    /// `/rustc/<commit>`, which every installation of this toolchain shares.
    #[must_use]
    pub(crate) fn library_source_name(&self) -> String {
        format!(
            "/rustc/{}",
            self.commit_hash.as_deref().unwrap_or(&self.release)
        )
    }
}

/// Reject a host whose native artifacts Bimetal cannot name: on Windows, it
/// installs the files that the MSVC toolchain builds, such as `<prefix>.dll.lib`.
fn check_host(host: &str) -> Result<()> {
    if host.contains("-windows-gnu") {
        bail!(
            "rustc's host is `{host}`, but on Windows Bimetal builds only with an MSVC toolchain, whose libraries it installs; install one, such as `stable-x86_64-pc-windows-msvc`, and select it with rustup"
        );
    }
    Ok(())
}

/// Run rustc and return its standard output.
fn run(executable: &OsStr, directory: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(executable)
        .args(args)
        .current_dir(directory)
        .output()
        .with_context(|| format!("failed to run {}", executable.to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "`{} {}` failed\n{}",
            executable.to_string_lossy(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).context("rustc printed invalid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_hosts_need_the_msvc_toolchain() {
        for host in [
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
            "x86_64-unknown-linux-gnu",
            "aarch64-apple-darwin",
        ] {
            check_host(host).unwrap();
        }
        for host in ["x86_64-pc-windows-gnu", "aarch64-pc-windows-gnullvm"] {
            let error = check_host(host).unwrap_err().to_string();
            assert!(
                error.contains(&format!("rustc's host is `{host}`")) && error.contains("MSVC"),
                "{error}"
            );
        }
    }
}
