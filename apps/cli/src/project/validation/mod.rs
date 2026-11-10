//! Cross-language validation of linked names, namespaces, and dependencies,
//! and of the user-owned manifests of the hosts' projects.
//!
//! Validation runs before rendering so generators can assume that every
//! public name is legal and unique in each enabled host, and checks only the
//! names of those hosts. Python namespace dependency cycles are rejected
//! because generated modules use eager imports. Every problem is collected,
//! so that one build reports them all, and each names the Rust items
//! involved.
//!
//! - `contract` validates a discovered contract, through the checks below;
//! - `names` holds the grammar of the application's and packages' names;
//! - `python` and `typescript` check the names of each host's modules and
//!   classes;
//! - `types` checks the models and the values that cross;
//! - `pyproject` and `package_json` check the user-owned manifests, which
//!   `user_file` reads.

mod contract;
mod names;
mod package_json;
mod pyproject;
mod python;
mod resources;
mod types;
mod typescript;
mod user_file;

pub(super) use contract::validate_contract;
pub(crate) use names::{
    validate_application_name, validate_python_package, validate_typescript_package,
};
pub(super) use package_json::validate_typescript_project;
pub(super) use pyproject::validate_python_project;
