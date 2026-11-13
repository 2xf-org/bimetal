//! Validation of the user-owned `package.json`.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::config::{Host, HostPackage};
use crate::dependencies::HostDependency;
use crate::literal;
use crate::problems::{self, Subject};
use crate::project::Project;

use super::user_file;

/// Validate the user-owned npm package name, version, the generated
/// directory's place in `exports` and `files`, and the peer dependencies that
/// the generated code imports, before publication.
pub(crate) fn validate_typescript_project(
    project: &Project,
    typescript: &HostPackage,
    peer_dependencies: &[HostDependency],
) -> Result<()> {
    let path = typescript.project.join("package.json");
    let package: Value = user_file::read_user_file(&path, |source| serde_json::from_str(source))?;
    let expected = Expected {
        name: &typescript.package,
        version: &project.package_version,
        directory: &project.package_name,
        peer_dependencies,
    };
    validate_package(&path, &package, &expected)
}

/// What the generated package needs from the user's `package.json`.
struct Expected<'project> {
    /// The npm package name, or the application name until `package.json`
    /// sets one.
    name: &'project str,
    /// The version that Cargo.toml declares.
    version: &'project str,
    /// The directory, beside `package.json`, that holds the generated files.
    directory: &'project str,
    /// The packages that the generated code imports as peers.
    peer_dependencies: &'project [HostDependency],
}

/// Check one parsed `package.json`, and report every problem at once.
fn validate_package(path: &Path, package: &Value, expected: &Expected<'_>) -> Result<()> {
    let mut problems = Vec::new();
    // `name` is the npm package name, which the application name stands in
    // for until it is set.
    if package["name"].as_str().is_none() {
        problems.push(format!(
            "must set `\"name\"`, the npm package name, such as `\"name\": {}`",
            literal::quoted(expected.name)
        ));
    }
    problems.extend(user_file::version_problem(
        package["version"].as_str(),
        "`version`",
        expected.version,
    ));
    if package["type"] != "module" {
        problems.push(
            "must declare `\"type\": \"module\"`; the generated JavaScript uses ES modules"
                .to_owned(),
        );
    }
    let generated = format!("./{}/", expected.directory);
    if let Some(root) = package["exports"].get(".").or(Some(&package["exports"]))
        && !root.is_null()
        && !export_targets(root)
            .iter()
            .any(|target| target.starts_with(&generated))
    {
        problems.push(format!(
            "must export the generated package: `exports[\".\"]` should point into `{generated}`, such as `{generated}index.js`"
        ));
    }
    if let Some(files) = package["files"].as_array() {
        let covered = files.iter().filter_map(Value::as_str).any(|entry| {
            let entry = entry.trim_start_matches("./").trim_end_matches('/');
            entry == expected.directory
                || entry.starts_with(&format!("{}/", expected.directory))
                || entry.contains('*')
        });
        if !covered {
            problems.push(format!(
                "must list the generated directory in `files`, such as {}; otherwise `npm pack` omits it",
                literal::quoted(expected.directory)
            ));
        }
    }
    // A peer dependency shares the application's copy, whose classes the
    // generated code must recognize.
    let mut missing = Vec::new();
    let mut older = Vec::new();
    for dependency in expected.peer_dependencies {
        let entry = format!(
            "{}: {}",
            literal::quoted(dependency.name),
            literal::quoted(dependency.requirement)
        );
        match &package["peerDependencies"][dependency.name] {
            Value::Null => missing.push(entry),
            Value::String(declared) if dependency.allows_older(Host::TypeScript, declared) => {
                older.push(format!("{entry} instead of {}", literal::quoted(declared)));
            }
            _ => {}
        }
    }
    if !missing.is_empty() {
        problems.push(format!(
            "must list the generated code's imports in `peerDependencies`: add {}",
            missing.join(", ")
        ));
    }
    if !older.is_empty() {
        problems.push(format!(
            "must not allow versions older than the generated code needs: in `peerDependencies`, set {}",
            older.join(", and ")
        ));
    }
    problems::report(Subject::File(path), &problems)
}

/// Collect every string target of an `exports` entry, through conditions.
fn export_targets(entry: &Value) -> Vec<&str> {
    match entry {
        Value::String(target) => vec![target.as_str()],
        Value::Array(entries) => entries.iter().flat_map(export_targets).collect(),
        Value::Object(conditions) => conditions.values().flat_map(export_targets).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::dependencies::APACHE_ARROW;

    fn check(package: &Value, peer_dependencies: &[HostDependency]) -> Result<()> {
        let expected = Expected {
            name: "my-app",
            version: "0.1.0",
            directory: "my-app",
            peer_dependencies,
        };
        validate_package(Path::new("package.json"), package, &expected)
    }

    fn scaffolded() -> Value {
        json!({
            "name": "my-app",
            "version": "0.1.0",
            "type": "module",
            "exports": {
                ".": {"types": "./my-app/index.d.ts", "default": "./my-app/index.js"},
                "./*": {"types": "./my-app/*/index.d.ts", "default": "./my-app/*/index.js"},
            },
            "files": ["my-app"],
        })
    }

    #[test]
    fn a_scaffolded_package_passes() {
        check(&scaffolded(), &[]).expect("the scaffolded shape is valid");
    }

    #[test]
    fn a_missing_name_says_which_name_to_set() {
        let mut package = scaffolded();
        package["name"] = json!(null);
        let error = check(&package, &[])
            .expect_err("the name is missing")
            .to_string();
        assert!(
            error.ends_with(
                "must set `\"name\"`, the npm package name, such as `\"name\": \"my-app\"`"
            ),
            "{error}"
        );
    }

    #[test]
    fn exports_must_point_into_the_generated_directory() {
        let mut package = scaffolded();
        package["exports"]["."] = json!("./old-name/index.js");
        let error = check(&package, &[])
            .expect_err("the root export points elsewhere")
            .to_string();

        assert!(error.contains("./my-app/"), "{error}");
        package["exports"] = json!("./my-app/index.js");
        check(&package, &[]).expect("a string export is accepted");
    }

    #[test]
    fn files_must_include_the_generated_directory() {
        let mut package = scaffolded();
        package["files"] = json!(["dist"]);
        let error = check(&package, &[])
            .expect_err("npm pack would omit the package")
            .to_string();

        assert!(error.contains("\"my-app\""), "{error}");
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        let mut package = scaffolded();
        package["version"] = json!("0.2.0");
        package["type"] = json!("commonjs");
        let error = check(&package, &[APACHE_ARROW])
            .expect_err("three problems")
            .to_string();

        assert!(
            error.starts_with("package.json has 3 problems:\n  - declares version `0.2.0`"),
            "{error}"
        );
        assert!(
            error.contains("\n  - must declare `\"type\": \"module\"`"),
            "{error}"
        );
        assert!(
            error.contains("\n  - must list the generated code's imports"),
            "{error}"
        );
    }

    #[test]
    fn arrow_needs_a_peer_dependency() {
        let error = check(&scaffolded(), &[APACHE_ARROW])
            .expect_err("apache-arrow is missing")
            .to_string();

        assert!(error.contains("\"apache-arrow\": \"^21.2.0\""), "{error}");
        let mut package = scaffolded();
        package["peerDependencies"] = json!({"apache-arrow": "^21.2.0"});
        check(&package, &[APACHE_ARROW]).expect("the peer dependency is declared");
        package["peerDependencies"] = json!({"apache-arrow": ">=21"});
        let error = check(&package, &[APACHE_ARROW])
            .expect_err("apache-arrow 21.0 lacks what the generated code needs")
            .to_string();
        assert!(
            error.contains("set \"apache-arrow\": \"^21.2.0\" instead of \">=21\""),
            "{error}"
        );
    }
}
