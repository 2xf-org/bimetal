//! The names that the generated Python package's modules and classes define,
//! and the imports between its modules.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use bimetal_ir::python_names;
use bimetal_ir::{Namespace, TypeRef};

use crate::contract::{
    self, ARROW_ARRAY_PROTOCOL, ARROW_SCHEMA_PROTOCOL, ARROW_STREAM_PROTOCOL, Contract,
    NamespaceCallable, NamespaceItems,
};
use crate::namespaces::NamespaceRule;
use crate::python;

use super::contract::{Export, Packages, Source};
use super::names;

/// Report Cargo packages, folders, and Rust modules whose names cannot be
/// Python modules.
pub(super) fn validate_modules(
    contract: &Contract<'_>,
    packages: &Packages<'_>,
    problems: &mut Vec<String>,
) {
    let rule = contract.rule();
    for (owner, rust_module) in contract.origins() {
        let namespace = contract.namespace(owner, rust_module);
        if let NamespaceRule::Packages(_) = rule {
            for package in &namespace.package {
                let segment = package.replace('-', "_");
                if let Some(reason) = names::python_name_problem(&segment) {
                    problems.push(format!(
                        "Cargo package `{owner}` becomes the Python module `{}`, but `{segment}` is {reason}; rename the package",
                        packages.python_module(&namespace)
                    ));
                }
            }
        }
        // The first segment is the crate, which the package segment names.
        for segment in rust_module.split("::").skip(1) {
            if let Some(reason) = names::python_name_problem(segment) {
                problems.push(format!(
                    "Rust module `{rust_module}` becomes the Python module `{}`, but `{segment}` is {reason}; rename the module",
                    packages.python_module(&namespace)
                ));
            }
        }
    }
    if let NamespaceRule::Folders(folders) = rule {
        // Each namespace without modules is one folder's, named by its last
        // segment, so a folder that holds several packages is reported once.
        for namespace in contract.namespaces().keys() {
            let segments = namespace.python_segments();
            if let Some(segment) = segments.last()
                && namespace.modules.is_empty()
                && let Some(reason) = names::python_name_problem(segment)
            {
                problems.push(format!(
                    "the folder `{}` becomes the Python module `{}`, but `{segment}` is {reason}; rename the folder",
                    folders.folder(&namespace.package),
                    packages.python_module(namespace)
                ));
            }
        }
    }
}

/// Report exports that the generated module cannot define, and exports that
/// collide.
pub(super) fn validate_exports(
    namespace: &Namespace,
    exports: &[Export],
    contract: &Contract<'_>,
    packages: &Packages<'_>,
    problems: &mut Vec<String>,
) {
    let module = packages.python_module(namespace);
    let namespace_path = namespace.python_segments();
    let child_modules: BTreeMap<String, &Namespace> = contract
        .namespaces()
        .keys()
        .filter_map(|child| {
            let path = child.python_segments();
            (path.len() == namespace_path.len() + 1 && path.starts_with(&namespace_path))
                .then(|| (path[namespace_path.len()].clone(), child))
        })
        .collect();
    let root = *namespace == Namespace::root();
    // The names that the package root defines itself, beside those that
    // every module does.
    let root_name = |name: &str| {
        root && [
            python::RUNTIME_MODULE,
            python_names::NATIVE_MODULE,
            python_names::PANIC_EXCEPTION,
        ]
        .contains(&name)
    };
    let leaf = |name: &str| [python::API_MODULE, python::MODELS_MODULE].contains(&name);
    // Every module may import the protocols that Arrow parameters accept.
    let generated = |name: &str| {
        leaf(name)
            || root_name(name)
            || [
                ARROW_ARRAY_PROTOCOL,
                ARROW_SCHEMA_PROTOCOL,
                ARROW_STREAM_PROTOCOL,
            ]
            .contains(&name)
            || python::imported_names().any(|imported| imported == name)
            || name.starts_with(python::ALIAS_PREFIX)
    };
    for (name, child) in &child_modules {
        if leaf(name) || root_name(name) {
            let source = Source::of(contract, child);
            problems.push(format!(
                "{source} becomes the Python module `{module}.{name}`, which Bimetal generates itself; rename {}",
                source.renamed()
            ));
        }
    }
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for export in exports {
        let name = export.python.as_str();
        let item = &export.item;
        if let Some(reason) = names::python_name_problem(name) {
            problems.push(format!(
                "{item} cannot be exported to Python, because `{name}` is {reason}; rename it"
            ));
        } else if generated(name) {
            problems.push(format!(
                "{item} becomes `{module}.{name}`, which the generated module defines itself; rename it"
            ));
        } else if let Some(child) = child_modules.get(name) {
            let source = Source::of(contract, child);
            problems.push(format!(
                "{item} and {source} both become `{module}.{name}`; rename one"
            ));
        } else if let Some(other) = seen.insert(name, item) {
            problems.push(format!(
                "{other} and {item} both become `{module}.{name}`; rename one"
            ));
        }
    }
}

/// Report resource methods and factories whose names collide with the
/// members that the generated classes define, or that a class cannot have.
pub(super) fn validate_resource_members(items: &NamespaceItems<'_>, problems: &mut Vec<String>) {
    for resource in &items.resources {
        let class = &resource.name;
        // Python methods and factories keep their Rust names, in one class
        // namespace with the generated members.
        let python_members = resource
            .factories
            .iter()
            .map(|factory| factory.rust_name.as_str())
            .chain(
                resource
                    .methods
                    .iter()
                    .map(|method| method.callable.rust_name.as_str()),
            );
        let mut python = BTreeSet::new();
        for name in python_members {
            if !python.insert(name) {
                problems.push(format!(
                    "methods and constructors of resource `{class}` collide at the Python name `{name}`; rename one"
                ));
            } else if python::RESOURCE_MEMBERS.contains(&name) {
                problems.push(format!(
                    "Rust method `{name}` of resource `{class}` collides with the member `{name}` of the generated Python class; rename the method"
                ));
            } else if name.starts_with("__") && name.ends_with("__") {
                problems.push(format!(
                    "Rust method `{name}` of resource `{class}` has a Python special method name, which the generated Python class reserves for its own protocols; rename the method"
                ));
            } else if name.starts_with("__") {
                // Python mangles `__name` inside a class body, also in the
                // wrapper's call to the extension.
                problems.push(format!(
                    "Rust method `{name}` of resource `{class}` starts with `__`, which Python mangles inside a class; rename the method"
                ));
            } else if python_names::is_keyword(name) {
                problems.push(format!(
                    "Rust method `{name}` of resource `{class}` is a Python keyword, so the generated Python class cannot define it; rename the method"
                ));
            }
        }
    }
}

/// Require the parameters of each callable to keep distinct names, after
/// Python renames the ones that it reserves, and to leave the module's models
/// and errors visible to the wrapper, which uses them.
pub(super) fn validate_parameters(
    items: &NamespaceItems<'_>,
    callables: &[NamespaceCallable<'_>],
    problems: &mut Vec<String>,
) {
    let wrapper_names: BTreeSet<String> = python::python_model_names(items)
        .into_iter()
        .chain(items.errors.iter().map(|error| error.name.clone()))
        .collect();
    for callable in callables {
        let params = &callable.callable.params;
        let mut names = BTreeSet::new();
        for (param, name) in params.iter().zip(python::python_param_names(params)) {
            if wrapper_names.contains(&name) {
                problems.push(format!(
                    "parameter `{}` of {} hides the Python model or error `{name}`, which the generated wrapper uses; rename the parameter",
                    param.rust_name, callable.item
                ));
            }
            if !names.insert(name.clone()) {
                problems.push(format!(
                    "parameters of {} collide at the Python name `{name}`; rename one",
                    callable.item
                ));
            }
        }
    }
}

/// Report cross-namespace model cycles that Python cannot import eagerly.
pub(super) fn validate_model_namespace_cycles(
    contract: &Contract<'_>,
    packages: &Packages<'_>,
    problems: &mut Vec<String>,
) -> Result<()> {
    let mut graph: BTreeMap<Namespace, BTreeSet<Namespace>> = BTreeMap::new();
    for definition in &contract.manifest.types {
        let source = contract.namespace(&definition.owner, &definition.rust_module);
        graph.entry(source.clone()).or_default();
        for node in definition.type_refs().flat_map(contract::nodes) {
            if let TypeRef::Named { identity } = node {
                let target = contract.type_definition(identity)?;
                let target = contract.namespace(&target.owner, &target.rust_module);
                if target != source {
                    graph.entry(source.clone()).or_default().insert(target);
                }
            }
        }
    }
    let mut complete = BTreeSet::new();
    let mut active = BTreeSet::new();
    let mut stack = Vec::new();
    for namespace in graph.keys() {
        if let Some(cycle) =
            namespace_cycle(namespace, &graph, &mut active, &mut complete, &mut stack)
        {
            let path = cycle
                .iter()
                .map(|namespace| packages.python_module(namespace))
                .collect::<Vec<_>>()
                .join(" -> ");
            problems.push(format!(
                "the models of the Python modules {path} import each other in a cycle; move the declarations into one Rust module or remove the cyclic type reference"
            ));
            // One cycle explains the problem; the others usually share it.
            break;
        }
    }
    Ok(())
}

/// Find one cycle reachable from `namespace` using depth-first traversal.
fn namespace_cycle(
    namespace: &Namespace,
    graph: &BTreeMap<Namespace, BTreeSet<Namespace>>,
    active: &mut BTreeSet<Namespace>,
    complete: &mut BTreeSet<Namespace>,
    stack: &mut Vec<Namespace>,
) -> Option<Vec<Namespace>> {
    if complete.contains(namespace) {
        return None;
    }
    if active.contains(namespace) {
        let start = stack.iter().position(|item| item == namespace).unwrap_or(0);
        let mut cycle = stack[start..].to_vec();
        cycle.push(namespace.clone());
        return Some(cycle);
    }
    active.insert(namespace.clone());
    stack.push(namespace.clone());
    if let Some(targets) = graph.get(namespace) {
        for target in targets {
            if let Some(cycle) = namespace_cycle(target, graph, active, complete, stack) {
                return Some(cycle);
            }
        }
    }
    stack.pop();
    active.remove(namespace);
    complete.insert(namespace.clone());
    None
}
