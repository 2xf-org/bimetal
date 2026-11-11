//! Collect and validate all exports linked into one application.
//!
//! Procedural macros submit lazy registrations through `inventory`. Discovery
//! materializes those records, validates their identities and references,
//! then sorts the resulting manifest so generated packages are deterministic
//! across link orders. The CLI validates the rest, such as wire names and
//! host names, with the problems of the host projects.
//!
//! Only native builds have a registry: discovery, the Python extension, and
//! the Node.js addon read the contract, and the Wasm module, which nothing
//! asks for it, does not carry it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::ContractType;
#[cfg(any(feature = "python", feature = "node"))]
use crate::ir::discovery;
use crate::ir::{
    ConstantDef, DefinitionId, ErrorDef, FunctionDef, Manifest, ResourceDef, TypeDef, TypeRef,
};

/// A linked type contract produced by [`crate::Model`].
pub struct TypeRegistration(
    /// Construct the type definition at discovery time, and add a problem
    /// for each default of a field that cannot enter the contract.
    pub fn(&mut Vec<String>) -> TypeDef,
);
/// A linked error contract produced by [`crate::Error`].
pub struct ErrorRegistration(
    /// Construct the error definition at discovery time.
    pub fn() -> ErrorDef,
);
/// A linked function contract produced by [`crate::export`].
pub struct FunctionRegistration(
    /// Construct the function definition at discovery time.
    pub fn() -> FunctionDef,
);
/// A linked resource contract produced by [`crate::export`].
pub struct ResourceRegistration(
    /// Construct the resource definition at discovery time.
    pub fn() -> ResourceDef,
);
/// A linked constant contract produced by [`crate::export`].
pub struct ConstantRegistration(
    /// Serialize and construct the constant definition at discovery time.
    pub fn() -> Result<ConstantDef, String>,
);

inventory::collect!(TypeRegistration);
inventory::collect!(ErrorRegistration);
inventory::collect!(FunctionRegistration);
inventory::collect!(ResourceRegistration);
inventory::collect!(ConstantRegistration);

/// One problem with the exports linked into an application.
#[must_use]
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// Two exports share the name that the host modules register them under.
    #[error("two exports share the native name `{name}`")]
    DuplicateNativeName {
        /// The repeated native name.
        name: String,
    },
    /// Two linked model or error declarations share one identity.
    #[error("two linked {kind} declarations are both `{}`", identity.id)]
    DuplicateIdentity {
        /// Kind of declaration that collided.
        kind: &'static str,
        /// Repeated identity.
        identity: DefinitionId,
    },
    /// An export uses a type that no linked crate derives `Model` for.
    #[error("an export uses `{}`, but no linked crate derives `Model` for it", identity.id)]
    MissingType {
        /// The type's identity.
        identity: DefinitionId,
    },
    /// An export returns an error that no linked crate derives `bimetal::Error`
    /// for.
    #[error(
        "an export returns `{}`, but no linked crate derives `bimetal::Error` for it",
        identity.id
    )]
    MissingError {
        /// The error's identity.
        identity: DefinitionId,
    },
    /// An exported constant could not be serialized into the contract.
    #[error("{0}")]
    InvalidConstant(String),
    /// The default of a field could not enter the contract.
    #[error("{0}")]
    InvalidDefault(String),
}

/// Every problem with the exports linked into an application, each once, so
/// that one build reports them all.
#[must_use]
#[derive(Debug)]
pub struct RegistryErrors {
    problems: Vec<RegistryError>,
}

impl fmt::Display for RegistryErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let [problem] = self.problems.as_slice() {
            return write!(formatter, "{problem}");
        }
        write!(
            formatter,
            "the Rust API has {} problems:",
            self.problems.len()
        )?;
        for problem in &self.problems {
            write!(formatter, "\n  - {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RegistryErrors {}

/// Serialize the application's contract, as discovery returns it to the CLI
/// and as the Python extension and the Node.js addon hash it.
///
/// # Errors
///
/// Returns [`RegistryErrors`] when [`manifest`] does.
///
/// # Panics
///
/// Never in practice: a manifest has only string keys, so it serializes.
pub fn contract_json(package_name: &str, package_version: &str) -> Result<String, RegistryErrors> {
    let manifest = manifest(package_name, package_version)?;
    Ok(
        serde_json::to_string(&manifest)
            .expect("a manifest has only string keys, so it serializes"),
    )
}

/// Return the hash of the application's contract, which the Python extension
/// and the Node.js addon carry for the generated package to check at import.
///
/// # Errors
///
/// Returns [`RegistryErrors`] when [`manifest`] does.
#[cfg(any(feature = "python", feature = "node"))]
pub(crate) fn contract_hash(
    package_name: &str,
    package_version: &str,
) -> Result<String, RegistryErrors> {
    let contract = contract_json(package_name, package_version)?;
    Ok(discovery::contract_hash(contract.as_bytes()))
}

/// Collect and validate every export linked into an application.
///
/// # Errors
///
/// Returns every [`RegistryError`] found: exports that share a native name,
/// that use an unlinked type or error, or that contain a constant or a field
/// default that cannot enter the contract.
pub fn manifest(package_name: &str, package_version: &str) -> Result<Manifest, RegistryErrors> {
    let mut problems = Vec::new();
    let mut manifest = collect(package_name, package_version, &mut problems);
    validate(&manifest, &mut problems);
    if !problems.is_empty() {
        // Exports that share a type report its problem once.
        let mut seen = BTreeSet::new();
        problems.retain(|problem| seen.insert(problem.to_string()));
        return Err(RegistryErrors { problems });
    }
    sort(&mut manifest);
    Ok(manifest)
}

/// Build the manifest of every linked registration, and add a problem for
/// each constant and each field default that cannot enter the contract.
fn collect(
    package_name: &str,
    package_version: &str,
    problems: &mut Vec<RegistryError>,
) -> Manifest {
    let mut constants = Vec::new();
    for registration in inventory::iter::<ConstantRegistration> {
        match (registration.0)() {
            Ok(constant) => constants.push(constant),
            Err(message) => problems.push(RegistryError::InvalidConstant(message)),
        }
    }
    let mut defaults = Vec::new();
    let types = inventory::iter::<TypeRegistration>
        .into_iter()
        .map(|item| (item.0)(&mut defaults))
        .collect();
    // The registrations come in link order, which differs between builds.
    defaults.sort();
    problems.extend(defaults.into_iter().map(RegistryError::InvalidDefault));
    Manifest {
        package_name: package_name.to_owned(),
        package_version: package_version.to_owned(),
        types,
        errors: inventory::iter::<ErrorRegistration>
            .into_iter()
            .map(|item| (item.0)())
            .collect(),
        functions: inventory::iter::<FunctionRegistration>
            .into_iter()
            .map(|item| (item.0)())
            .collect(),
        resources: inventory::iter::<ResourceRegistration>
            .into_iter()
            .map(|item| (item.0)())
            .collect(),
        constants,
    }
}

/// Return what an absent `field` of the declaration `owner` holds: the Rust
/// `Default` of its type, as Serde writes it. A default that cannot enter
/// the contract gives `None` and adds to `problems`.
///
/// A package rebuilds its contract when it loads and compares it with the
/// one that the build read, so the default must be the same on every call.
#[doc(hidden)]
pub fn field_default<T>(owner: &str, field: &str, problems: &mut Vec<String>) -> Option<Value>
where
    T: ContractType + Default + Serialize,
{
    let default = T::default();
    let problem = match default.check_rules() {
        Err(violation) => format!("breaks a field rule: {violation}"),
        Ok(()) => match (
            serde_json::to_value(default),
            serde_json::to_value(T::default()),
        ) {
            (Ok(first), Ok(second)) => {
                let first = sorted(first);
                if first == sorted(second) {
                    return Some(first);
                }
                "changes from one call of `Default::default` to the next".to_owned()
            }
            (Err(error), _) | (_, Err(error)) => format!("could not serialize as JSON: {error}"),
        },
    };
    problems.push(format!(
        "the default of field `{field}` of `{owner}` {problem}"
    ));
    None
}

/// Return `value` with the keys of every object in order. Serde writes a
/// `HashMap` in an order that changes from one process to the next, which
/// `serde_json` keeps when a crate turns on its `preserve_order`.
fn sorted(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        Value::Object(entries) => {
            let entries: BTreeMap<String, Value> = entries
                .into_iter()
                .map(|(key, value)| (key, sorted(value)))
                .collect();
            Value::Object(entries.into_iter().collect())
        }
        scalar => scalar,
    }
}

/// Add a problem for each pair of exports that share a native name, each
/// repeated identity, and each type or error that no linked crate registered.
fn validate(manifest: &Manifest, problems: &mut Vec<RegistryError>) {
    let mut native_names = BTreeSet::new();
    let exports = manifest
        .functions
        .iter()
        .map(|item| item.native_name.as_str())
        .chain(
            manifest
                .resources
                .iter()
                .map(|item| item.native_name.as_str()),
        );
    for name in exports {
        if !native_names.insert(name) {
            problems.push(RegistryError::DuplicateNativeName {
                name: name.to_owned(),
            });
        }
    }
    let type_ids = unique_ids(
        "model",
        manifest.types.iter().map(TypeDef::identity),
        problems,
    );
    let error_ids = unique_ids(
        "error",
        manifest.errors.iter().map(ErrorDef::identity),
        problems,
    );
    let mut missing_types = BTreeSet::new();
    for reference in manifest.type_refs() {
        collect_missing_types(reference, &type_ids, &mut missing_types);
    }
    problems.extend(
        missing_types
            .into_iter()
            .map(|identity| RegistryError::MissingType { identity }),
    );
    let missing_errors: BTreeSet<&DefinitionId> = manifest
        .callables()
        .filter_map(|callable| callable.error.as_ref())
        .filter(|identity| !error_ids.contains(*identity))
        .collect();
    problems.extend(
        missing_errors
            .into_iter()
            .map(|identity| RegistryError::MissingError {
                identity: identity.clone(),
            }),
    );
}

/// Sort every list of the manifest by namespace and name, so that the
/// generated packages do not depend on the link order.
fn sort(manifest: &mut Manifest) {
    let rule = manifest.namespace_rule();
    manifest.types.sort_by_cached_key(|item| {
        (
            rule.namespace(&item.owner, &item.rust_module),
            item.name.clone(),
            item.id.clone(),
        )
    });
    manifest.errors.sort_by_cached_key(|item| {
        (
            rule.namespace(&item.owner, &item.rust_module),
            item.name.clone(),
            item.id.clone(),
        )
    });
    manifest.functions.sort_by_cached_key(|item| {
        (
            rule.namespace(&item.owner, &item.rust_module),
            item.callable.host_name.clone(),
            item.callable.rust_name.clone(),
        )
    });
    manifest.resources.sort_by_cached_key(|item| {
        (
            rule.namespace(&item.owner, &item.rust_module),
            item.name.clone(),
        )
    });
    manifest.constants.sort_by_cached_key(|item| {
        (
            rule.namespace(&item.owner, &item.rust_module),
            item.host_name.clone(),
        )
    });
}

/// Collect identities, reporting each one that repeats.
fn unique_ids(
    kind: &'static str,
    values: impl Iterator<Item = DefinitionId>,
    problems: &mut Vec<RegistryError>,
) -> BTreeSet<DefinitionId> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value.clone()) {
            problems.push(RegistryError::DuplicateIdentity {
                kind,
                identity: value,
            });
        }
    }
    seen
}

/// Collect the named types that a reference uses but no linked crate
/// registered.
fn collect_missing_types(
    reference: &TypeRef,
    types: &BTreeSet<DefinitionId>,
    missing: &mut BTreeSet<DefinitionId>,
) {
    if let TypeRef::Named { identity } = reference
        && !types.contains(identity)
    {
        missing.insert(identity.clone());
    }
    for child in reference.children() {
        collect_missing_types(child, types, missing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_is_reported_after_a_count() {
        let errors = RegistryErrors {
            problems: vec![
                RegistryError::InvalidConstant("constant `A` breaks a field rule".to_owned()),
                RegistryError::DuplicateNativeName {
                    name: "__bimetal_function_app_0".to_owned(),
                },
            ],
        };

        assert_eq!(
            errors.to_string(),
            "the Rust API has 2 problems:\n  - constant `A` breaks a field rule\n  - two exports share the native name `__bimetal_function_app_0`"
        );
    }
}
