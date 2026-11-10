//! The names that the generated npm package's modules and classes define.

use std::collections::{BTreeMap, BTreeSet};

use bimetal_ir::Namespace;
use bimetal_ir::typescript_names;

use crate::contract::{NamespaceCallable, NamespaceItems};
use crate::literal;
use crate::typescript;

use super::contract::{Export, Packages};

/// The words that TypeScript reserves beside JavaScript's, which a declared
/// name cannot take.
const TYPESCRIPT_RESERVED: [&str; 18] = [
    "any",
    "as",
    "boolean",
    "constructor",
    "declare",
    "get",
    "module",
    "namespace",
    "never",
    "number",
    "readonly",
    "require",
    "set",
    "string",
    "symbol",
    "type",
    "undefined",
    "unknown",
];

/// Return whether `value` is a usable, non-keyword TypeScript identifier. A
/// module, which is strict code, cannot bind `arguments`, `await`, or `eval`
/// either.
#[must_use]
fn is_identifier(value: &str) -> bool {
    literal::is_identifier(value)
        && !typescript_names::is_reserved_binding(value)
        && !TYPESCRIPT_RESERVED.contains(&value)
}

/// Report exports that the generated module cannot define, and exports that
/// collide.
pub(super) fn validate_exports(
    namespace: &Namespace,
    exports: &[Export],
    packages: &Packages<'_>,
    problems: &mut Vec<String>,
) {
    let module = packages.typescript_module(namespace);
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    // Generated buffer types exist only in Python.
    for export in exports
        .iter()
        .filter(|export| !export.typescript.is_empty())
    {
        let name = export.typescript.as_str();
        let item = &export.item;
        if !is_identifier(name) {
            problems.push(format!(
                "{item} cannot be exported to TypeScript, because `{name}` is a reserved word there; rename it"
            ));
        } else if *namespace == Namespace::root() && name == typescript::NATIVE_READY {
            problems.push(format!(
                "{item} becomes the export `{name}` of `{module}`, which the package root defines to load the Rust code; rename it"
            ));
        } else if name == "then" {
            problems.push(format!(
                "{item} becomes the export `then` of `{module}`, which makes the module a thenable that `await import(...)` calls; rename it"
            ));
        } else if name == typescript::JSON_VALUE {
            problems.push(format!(
                "{item} becomes the TypeScript name `{name}` in `{module}`, which the generated module defines for JSON values; rename it"
            ));
        } else if name == typescript::RECORD_BATCH_STREAM || name == typescript::RECORD_BATCH_SOURCE
        {
            problems.push(format!(
                "{item} becomes the TypeScript name `{name}` in `{module}`, which the generated module defines for Arrow streams; rename it"
            ));
        } else if name == typescript::ARROW_NAMESPACE
            || name.starts_with(typescript::API_ALIAS_PREFIX)
            || name.starts_with(typescript::MODELS_ALIAS_PREFIX)
        {
            problems.push(format!(
                "{item} becomes the TypeScript name `{name}` in `{module}`, which the generated module uses for its imports; rename it"
            ));
        } else if let Some(other) = seen.insert(name, item) {
            problems.push(format!(
                "{other} and {item} both become the TypeScript export `{name}` of `{module}`; rename one"
            ));
        }
    }
}

/// Report resource methods and factories whose names collide with the
/// members that the generated classes define, or that a class cannot have.
pub(super) fn validate_resource_members(items: &NamespaceItems<'_>, problems: &mut Vec<String>) {
    for resource in &items.resources {
        let class = &resource.name;
        let mut methods = BTreeSet::new();
        for method in &resource.methods {
            let name = method.callable.host_name.as_str();
            if typescript::RESOURCE_MEMBERS.contains(&name) {
                problems.push(format!(
                    "Rust method `{}` of resource `{class}` becomes the TypeScript member `{name}`, which the generated class defines; rename the method",
                    method.callable.rust_name
                ));
            } else if name == "then" {
                // An instance with a `then` method is a thenable, so `await`
                // and an async factory would call it.
                problems.push(format!(
                    "Rust method `{}` of resource `{class}` becomes the TypeScript method `then`, which makes every instance a thenable that `await` calls; rename the method",
                    method.callable.rust_name
                ));
            } else if !methods.insert(name) {
                problems.push(format!(
                    "methods of resource `{class}` collide at the TypeScript name `{name}`; rename one"
                ));
            }
        }
        let mut factories = BTreeSet::new();
        for factory in &resource.factories {
            let name = factory.host_name.as_str();
            if name == "prototype" {
                problems.push(format!(
                    "Rust constructor `{}` of resource `{class}` becomes the TypeScript static method `prototype`, which a class cannot define; rename the constructor",
                    factory.rust_name
                ));
            } else if !factories.insert(name) {
                problems.push(format!(
                    "constructors of resource `{class}` collide at the TypeScript name `{name}`; rename one"
                ));
            }
        }
    }
}

/// Require the parameters of each callable to keep distinct names, after
/// TypeScript renames the ones that a module reserves.
pub(super) fn validate_parameters(callables: &[NamespaceCallable<'_>], problems: &mut Vec<String>) {
    for callable in callables {
        let mut names = BTreeSet::new();
        for param in &callable.callable.params {
            let name = typescript::parameter_name(param);
            if !names.insert(name.clone()) {
                problems.push(format!(
                    "parameters of {} collide at the TypeScript name `{name}`; rename one",
                    callable.item
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_exclude_the_reserved_words_of_both_languages() {
        for word in ["type", "class", "eval", "let"] {
            assert!(!is_identifier(word), "{word}");
        }
        assert!(is_identifier("Model"));
    }
}
