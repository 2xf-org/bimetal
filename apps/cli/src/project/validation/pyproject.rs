//! Validation of the user-owned `pyproject.toml`.

use std::path::Path;

use anyhow::Result;
use toml::Value;

use crate::config::{Host, HostPackage};
use crate::dependencies::HostDependency;
use crate::literal;
use crate::problems::{self, Subject};
use crate::project::Project;
use crate::python;

use super::user_file;

/// The build backend that `bimetal new` configures, whose settings Bimetal checks.
const PDM_BACKEND: &str = "pdm.backend";

/// Validate the user-owned project of the Python package before generated
/// files are published, given the packages that the generated code imports.
pub(crate) fn validate_python_project(
    project: &Project,
    python: &HostPackage,
    dependencies: &[HostDependency],
) -> Result<()> {
    let path = python.project.join("pyproject.toml");
    let pyproject: Value = user_file::read_user_file(&path, |source| toml::from_str(source))?;
    let expected = Expected {
        version: &project.package_version,
        package: &python.package,
        dependencies,
    };
    validate_pyproject(&path, &pyproject, &expected)
}

/// What the generated package needs from the user's project.
struct Expected<'project> {
    /// The version that Cargo.toml declares.
    version: &'project str,
    /// The dotted import package, such as `bank` or `acme.tools`.
    package: &'project str,
    /// The packages that the generated code imports.
    dependencies: &'project [HostDependency],
}

/// Check the version, the runtime dependencies, and, for pdm-backend, that
/// wheels include the generated package and carry platform tags. Every
/// problem is reported at once.
fn validate_pyproject(path: &Path, pyproject: &Value, expected: &Expected<'_>) -> Result<()> {
    let mut problems = Vec::new();
    if let Some(table) = pyproject.get("project") {
        let version = table.get("version").and_then(Value::as_str);
        problems.extend(user_file::version_problem(
            version,
            "`[project].version`",
            expected.version,
        ));
        let dependencies = table
            .get("dependencies")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let mut missing = Vec::new();
        let mut older = Vec::new();
        for dependency in expected.dependencies {
            match declared(dependencies, dependency.name) {
                None => missing.push(literal::quoted(dependency.requirement)),
                Some(declared) if dependency.allows_older(Host::Python, declared) => {
                    older.push(format!(
                        "{} with {}",
                        literal::quoted(declared),
                        literal::quoted(dependency.requirement)
                    ));
                }
                Some(_) => {}
            }
        }
        if !missing.is_empty() {
            problems.push(format!(
                "must declare the generated runtime dependencies: add {} to `[project].dependencies`",
                missing.join(", ")
            ));
        }
        if !older.is_empty() {
            problems.push(format!(
                "must not allow versions older than the generated code needs: in `[project].dependencies`, replace {}",
                older.join(", and ")
            ));
        }
    } else {
        problems.push("must define `[project]`".to_owned());
    }
    let backend = pyproject
        .get("build-system")
        .and_then(|build_system| build_system.get("build-backend"))
        .and_then(Value::as_str);
    if backend == Some(PDM_BACKEND) {
        validate_pdm_build(pyproject, expected.package, &mut problems);
    }
    problems::report(Subject::File(path), &problems)
}

/// Return the requirement of a requirement list that names a package,
/// ignoring case.
fn declared<'list>(dependencies: &'list [Value], name: &str) -> Option<&'list str> {
    dependencies.iter().filter_map(Value::as_str).find(|item| {
        item.split(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '-' | '_' | '.')
        })
        .next()
        .is_some_and(|declared| declared.eq_ignore_ascii_case(name))
    })
}

/// Require pdm-backend to package the generated directory and to tag wheels
/// for a platform, which the native extension needs.
fn validate_pdm_build(pyproject: &Value, package: &str, problems: &mut Vec<String>) {
    let build = pyproject
        .get("tool")
        .and_then(|tool| tool.get("pdm"))
        .and_then(|pdm| pdm.get("build"));
    if build
        .and_then(|build| build.get("is-purelib"))
        .and_then(Value::as_bool)
        != Some(false)
    {
        problems.push("must set `is-purelib = false` under `[tool.pdm.build]`: the package ships a native extension, so its wheels must be platform-specific".to_owned());
    }
    let Some(includes) = build
        .and_then(|build| build.get("includes"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let directory = python::python_package_directory(package);
    let top_level = directory.split('/').next().unwrap_or(&directory);
    let covered = includes.iter().filter_map(Value::as_str).any(|include| {
        let include = include.trim_end_matches('/');
        include.contains('*')
            || include == top_level
            || directory == include
            || directory.starts_with(&format!("{include}/"))
    });
    if !covered {
        problems.push(format!(
            "must list the generated package in `[tool.pdm.build].includes`, for example `includes = [{}]`; otherwise wheels ship without it",
            literal::quoted(top_level)
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dependencies::{NUMPY, PYDANTIC};

    fn check(source: &str, package: &str, dependencies: &[HostDependency]) -> Result<()> {
        let pyproject: Value = toml::from_str(source).expect("valid test TOML");
        let expected = Expected {
            version: "0.1.0",
            package,
            dependencies,
        };
        validate_pyproject(Path::new("pyproject.toml"), &pyproject, &expected)
    }

    const PROJECT: &str = r#"
[build-system]
build-backend = "pdm.backend"

[project]
version = "0.1.0"
dependencies = ["pydantic>=2.7,<3"]

[tool.pdm.build]
includes = ["acme"]
is-purelib = false
"#;

    #[test]
    fn a_scaffolded_project_passes() {
        check(PROJECT, "acme", &[PYDANTIC]).expect("the scaffolded shape is valid");
        check(PROJECT, "acme.tools", &[PYDANTIC]).expect("a parent directory covers the package");
    }

    #[test]
    fn every_missing_dependency_is_reported_at_once() {
        let error = check(PROJECT, "acme", &[PYDANTIC, NUMPY])
            .expect_err("numpy is missing")
            .to_string();

        assert!(error.contains("\"numpy>=2,<3\""), "{error}");
    }

    #[test]
    fn a_dependency_must_not_allow_versions_that_the_generated_code_lacks() {
        let older = PROJECT.replace("pydantic>=2.7,<3", "pydantic>=2,<3");
        let error = check(&older, "acme", &[PYDANTIC])
            .expect_err("pydantic 2.6 lacks what the generated code needs")
            .to_string();

        assert!(
            error.contains("replace \"pydantic>=2,<3\" with \"pydantic>=2.7,<3\""),
            "{error}"
        );
        let newer = PROJECT.replace("pydantic>=2.7,<3", "pydantic>=2.9");
        check(&newer, "acme", &[PYDANTIC]).expect("a higher floor is fine");
    }

    #[test]
    fn wheels_must_include_the_generated_package() {
        let error = check(PROJECT, "tools", &[PYDANTIC])
            .expect_err("`acme` does not cover `tools`")
            .to_string();

        assert!(error.contains("includes = [\"tools\"]"), "{error}");
    }

    #[test]
    fn wheels_must_be_platform_specific() {
        let purelib = PROJECT.replace("is-purelib = false", "is-purelib = true");
        let error = check(&purelib, "acme", &[PYDANTIC])
            .expect_err("a native extension needs a platform wheel")
            .to_string();

        assert!(error.contains("is-purelib = false"), "{error}");
    }

    #[test]
    fn other_build_backends_are_not_inspected() {
        let hatch = PROJECT
            .replace("pdm.backend", "hatchling.build")
            .replace("is-purelib = false", "");
        check(&hatch, "tools", &[PYDANTIC]).expect("Bimetal checks only pdm-backend settings");
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let broken = PROJECT
            .replace("0.1.0", "0.2.0")
            .replace("is-purelib = false", "");
        let error = check(&broken, "acme", &[PYDANTIC, NUMPY])
            .expect_err("three problems")
            .to_string();

        assert!(
            error.starts_with("pyproject.toml has 3 problems:\n  - declares version `0.2.0`"),
            "{error}"
        );
        assert!(error.contains("\n  - must declare"), "{error}");
        assert!(
            error.contains("\n  - must set `is-purelib = false`"),
            "{error}"
        );
    }

    #[test]
    fn versions_must_match_cargo() {
        let other = PROJECT.replace("0.1.0", "0.2.0");
        let error = check(&other, "acme", &[PYDANTIC])
            .expect_err("versions differ")
            .to_string();

        assert!(error.contains("`0.2.0`"), "{error}");
    }
}
