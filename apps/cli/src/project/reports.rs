//! The reports that the build, `check`, and `build-native` steps return.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use crate::config::{GeneratedState, Host};
use crate::paths;

use super::Project;

/// Whether a build compiled the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BuildStatus {
    /// Bimetal compiled the application and published the outputs.
    Ok,
    /// The recorded fingerprints matched, so Bimetal compiled nothing.
    Unchanged,
}

/// The result of `bimetal build`.
#[derive(Debug, Serialize)]
pub(crate) struct BuildReport<'project> {
    pub(crate) status: BuildStatus,
    pub(crate) application: &'project str,
    pub(crate) version: &'project str,
    pub(crate) hosts: Vec<HostReport<'project>>,
}

impl<'project> BuildReport<'project> {
    /// Report a build that left the packages that `generated` records.
    pub(super) fn new(
        project: &'project Project,
        status: BuildStatus,
        generated: &GeneratedState,
    ) -> Result<Self> {
        Ok(Self {
            status,
            application: &project.package_name,
            version: &project.package_version,
            hosts: host_reports(project, generated)?,
        })
    }
}

/// The result of `bimetal check`, which says `"status": "ok"` in JSON.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename = "ok")]
pub(crate) struct CheckReport<'project> {
    pub(crate) application: &'project str,
    pub(crate) version: &'project str,
    pub(crate) hosts: Vec<HostReport<'project>>,
}

/// One host's package in a report.
#[derive(Debug, Serialize)]
pub(crate) struct HostReport<'project> {
    pub(crate) host: Host,
    /// The Python import package, or the npm package name.
    pub(crate) package: &'project str,
    /// The project that holds the package.
    pub(crate) directory: &'project Path,
    /// The package's files, relative to the project with `/` separators:
    /// the generated files, then this platform's native artifacts, which
    /// `bimetal build` and `bimetal build-native` install.
    pub(crate) files: Vec<String>,
}

/// Return the report of every enabled host, whose generated files
/// `generated` records.
pub(super) fn host_reports<'project>(
    project: &'project Project,
    generated: &GeneratedState,
) -> Result<Vec<HostReport<'project>>> {
    let native = project.native_artifacts();
    let mut reports = Vec::new();
    for package in project.hosts() {
        let mut files = generated
            .record(package.host)
            .map(|record| record.files.clone())
            .unwrap_or_default();
        for artifact in &native {
            // No project holds another, so each artifact is in one.
            if let Ok(relative) = artifact.destination.strip_prefix(&package.project) {
                files.push(paths::portable(relative)?);
            }
        }
        reports.push(HostReport {
            host: package.host,
            package: &package.package,
            directory: &package.project,
            files,
        });
    }
    Ok(reports)
}

/// The result of `bimetal build-native`, which says `"status": "ok"` in JSON.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename = "ok")]
pub(crate) struct NativeBuildReport<'project> {
    pub(crate) application: &'project str,
    /// The installed files, none for an application without native
    /// artifacts.
    pub(crate) artifacts: Vec<PathBuf>,
}
