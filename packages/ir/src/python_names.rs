//! The Python names that the generated package and the extension share.
//!
//! The generator names each wrapper's parameters, and the extension names the
//! same parameters in its argument errors, so both derive them here from the
//! Rust names alone.

use crate::disambiguate;

/// The builtins that generated Python modules use. A contract name that
/// hides one of them makes the generated code reach it through
/// [`BUILTINS_ALIAS`] instead.
pub const BUILTINS: [&str; 19] = [
    "KeyError",
    "RuntimeError",
    "StopAsyncIteration",
    "StopIteration",
    "TypeError",
    "bool",
    "bytes",
    "classmethod",
    "dict",
    "float",
    "getattr",
    "int",
    "isinstance",
    "list",
    "object",
    "str",
    "super",
    "tuple",
    "type",
];

/// The name of the private extension module inside the generated package.
pub const NATIVE_MODULE: &str = "_native";

/// The extension attribute that holds the exception a Rust panic raises,
/// pyo3's `PanicException`, which the generated package exports.
pub const PANIC_EXCEPTION: &str = "PanicException";

/// The name under which a generated module imports `builtins`.
pub const BUILTINS_ALIAS: &str = "_bimetal_builtins";

/// The name under which a generated module imports `datetime`, so that a
/// field or parameter named `date` cannot hide the type.
pub const DATETIME_ALIAS: &str = "_bimetal_datetime";

/// The prefixes of the aliases under which generated modules import other
/// namespaces' modules, each followed by the namespace's index.
pub const MODULE_ALIAS_PREFIXES: [&str; 2] = ["_bimetal_api_", "_bimetal_models_"];

/// Names that a generated wrapper resolves in its own scope besides the
/// builtins and the runtime helpers, which it imports under aliases that
/// start with `_bimetal_`: its receivers, and the imports that its body and its
/// typed locals use.
const WRAPPER_SCOPE: [&str; 7] = [
    "cls",
    "self",
    "pa",
    "Any",
    "AwareDatetime",
    BUILTINS_ALIAS,
    DATETIME_ALIAS,
];

/// The prefix of the locals that generated wrappers bind. Rust parameters
/// cannot start with it, so no parameter can hide a local.
pub const WRAPPER_LOCAL_PREFIX: &str = "__bimetal_";

/// Return whether `value` is a Python keyword.
#[must_use]
pub fn is_keyword(value: &str) -> bool {
    matches!(
        value,
        "False"
            | "None"
            | "True"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
    )
}

/// Return `value` as a legal Python identifier that is not a keyword.
///
/// Other characters become underscores, and a keyword gains `_value`, as
/// does `mro`, which an enum cannot have as a member.
#[must_use]
pub fn identifier(value: &str) -> String {
    let mut name = String::new();
    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_alphanumeric() || character == '_' {
            if index == 0 && character.is_ascii_digit() {
                name.push_str("value_");
            }
            name.push(character);
        } else {
            name.push('_');
        }
    }
    if name.is_empty() {
        name.push_str("value");
    }
    if is_keyword(&name) || name == "mro" {
        name.push_str("_value");
    }
    name
}

/// Return the Python names of a callable's parameters from their Rust names,
/// in order.
///
/// A parameter keeps its Rust name, made a legal identifier, unless the
/// wrapper needs that name itself: a receiver, a runtime helper, a builtin,
/// or an import alias. Such a parameter gains trailing underscores
/// (`bytes_`) until it hides nothing and differs from the other parameters.
#[must_use]
pub fn parameter_names<'name>(rust_names: impl IntoIterator<Item = &'name str>) -> Vec<String> {
    let mut names: Vec<String> = rust_names.into_iter().map(identifier).collect();
    let hides = |name: &str| {
        BUILTINS.contains(&name)
            || WRAPPER_SCOPE.contains(&name)
            || MODULE_ALIAS_PREFIXES.iter().any(|prefix| {
                name.strip_prefix(prefix)
                    .is_some_and(|index| index.bytes().all(|byte| byte.is_ascii_digit()))
            })
            // Inside a class, Python mangles a local such as `__bimetal_native`
            // to `_Class__bimetal_native`, which a parameter could spell.
            || (name.starts_with('_') && name.contains(WRAPPER_LOCAL_PREFIX) && !name.ends_with('_'))
    };
    disambiguate::disambiguate(&mut names, hides);
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_replace_other_characters_and_rename_keywords() {
        assert_eq!(identifier("event-type"), "event_type");
        assert_eq!(identifier("class"), "class_value");
        assert_eq!(identifier("mro"), "mro_value");
        assert_eq!(identifier("2d"), "value_2d");
        assert_eq!(identifier("name"), "name");
    }

    #[test]
    fn parameters_that_hide_wrapper_names_get_trailing_underscores() {
        assert_eq!(
            parameter_names([
                "value",
                "int",
                "int_",
                "class",
                "bytes",
                "self",
                "_bimetal_builtins",
                "_bimetal_models_0",
                "_bimetal_api_12",
                "_bimetal_api_x",
                "_Recorder__bimetal_native",
            ]),
            [
                "value",
                "int__",
                "int_",
                "class_value",
                "bytes_",
                "self_",
                "_bimetal_builtins_",
                "_bimetal_models_0_",
                "_bimetal_api_12_",
                "_bimetal_api_x",
                "_Recorder__bimetal_native_",
            ]
        );
    }
}
