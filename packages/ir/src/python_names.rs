//! The Python names that the generated package and the extension share.
//!
//! The generator names each wrapper's parameters, and the extension names the
//! same parameters in its argument errors, so both derive them here from the
//! Rust names alone.

use crate::disambiguate;

/// The builtins that generated Python modules use. A contract name that
/// hides one of them makes the generated code reach it through the
/// `builtins` module instead.
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

/// The module of the generated package that loads the extension and holds
/// the helpers that the generated modules share.
pub const RUNTIME_MODULE: &str = "bimetal";

/// The name of the extension module inside the generated package.
pub const NATIVE_MODULE: &str = "native";

/// The extension attribute that holds the exception a Rust panic raises,
/// pyo3's `PanicException`, which the generated package exports.
pub const PANIC_EXCEPTION: &str = "PanicException";

/// The names that generated modules bind by importing a module: the runtime,
/// then the modules of the standard library and of other packages whose
/// members they reach through the module, such as `typing.Literal`. A field
/// or a parameter of the same name would hide the module, and an export
/// would replace it.
pub const IMPORTED_MODULES: [&str; 9] = [
    RUNTIME_MODULE,
    "builtins",
    "collections",
    "datetime",
    "enum",
    "numpy",
    "pyarrow",
    "pydantic",
    "typing",
];

/// The starts of the names that generated modules give their own constants,
/// such as `BIMETAL_ADAPTER_0`, the aliases of other namespaces' modules,
/// and the locals of their wrappers, such as `bimetal_result`. None of these
/// names ends with an underscore.
pub const RESERVED_PREFIXES: [&str; 2] = ["BIMETAL_", "bimetal_"];

/// Return whether `name` is one that generated modules reserve: an
/// [imported module](IMPORTED_MODULES), or a name that starts with one of
/// the [`RESERVED_PREFIXES`] and does not end with an underscore.
#[must_use]
pub fn is_reserved(name: &str) -> bool {
    IMPORTED_MODULES.contains(&name)
        || (RESERVED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
            && !name.ends_with('_'))
}

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
/// wrapper needs that name itself: a receiver, a builtin, or a
/// [reserved](is_reserved) name. Such a parameter gains trailing underscores
/// (`bytes_`) until it hides nothing and differs from the other parameters.
#[must_use]
pub fn parameter_names<'name>(rust_names: impl IntoIterator<Item = &'name str>) -> Vec<String> {
    let mut names: Vec<String> = rust_names.into_iter().map(identifier).collect();
    let hides = |name: &str| {
        BUILTINS.contains(&name) || ["cls", "self"].contains(&name) || is_reserved(name)
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
                "datetime",
                "bimetal",
                "bimetal_result",
                "BIMETAL_COUNT_NATIVE",
                "Bimetal_result",
            ]),
            [
                "value",
                "int__",
                "int_",
                "class_value",
                "bytes_",
                "self_",
                "datetime_",
                "bimetal_",
                "bimetal_result_",
                "BIMETAL_COUNT_NATIVE_",
                "Bimetal_result",
            ]
        );
    }
}
