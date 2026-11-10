//! The TypeScript names and messages that the generated package and its
//! native code, the Wasm module or the Node.js addon, share.
//!
//! The generator names each wrapper's parameters, and the native code names
//! the same parameters in its argument errors, so both derive them here from
//! the host names in the contract.

use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};

/// The property by which the generated JavaScript recognizes a typed contract
/// error that Rust throws, `{bimetalErrorCode, message}`, which no `Error` has.
pub const ERROR_CODE_PROPERTY: &str = "bimetalErrorCode";

/// The property of such an error that holds its fields as JSON, when it
/// declares them.
pub const ERROR_DETAILS_PROPERTY: &str = "bimetalErrorDetails";

/// What the message of the `Error` that a Rust panic throws starts with.
pub const PANIC_PREFIX: &str = "Rust panicked: ";

/// A callable as a JavaScript caller writes it, which argument errors name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callable<'name> {
    /// A function: `greet`.
    Function(&'name str),
    /// A class's constructor: `new Counter`.
    Constructor(&'name str),
    /// A factory or a method of a class: `Counter.add`.
    Member {
        /// The class.
        class: &'name str,
        /// The member's host name.
        name: &'name str,
    },
}

impl Display for Callable<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Function(name) => formatter.write_str(name),
            Self::Constructor(class) => write!(formatter, "new {class}"),
            Self::Member { class, name } => write!(formatter, "{class}.{name}"),
        }
    }
}

/// Return how errors about an argument name it: ``greet(): argument `name` ``,
/// for the callable as [`Callable`] writes it and the parameter as
/// [`parameter_name`] does.
#[must_use]
pub fn argument_label(callable: &str, parameter: &str) -> String {
    format!("{callable}(): argument `{parameter}`")
}

/// Return the name of a parameter in the generated JavaScript: its host name,
/// with a trailing underscore when a module cannot bind that name
/// (`package_`).
#[must_use]
pub fn parameter_name(host_name: &str) -> Cow<'_, str> {
    if is_reserved_binding(host_name) {
        Cow::Owned(format!("{host_name}_"))
    } else {
        Cow::Borrowed(host_name)
    }
}

/// Return whether JavaScript module code, which is strict, cannot use `name`
/// as a parameter: its reserved words, and `arguments` and `eval`.
#[must_use]
pub fn is_reserved_binding(name: &str) -> bool {
    matches!(
        name,
        "arguments"
            | "await"
            | "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "eval"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "interface"
            | "let"
            | "new"
            | "null"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "return"
            | "static"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_labels_name_the_callable_as_callers_write_it() {
        let labels = [
            Callable::Function("greet"),
            Callable::Constructor("Counter"),
            Callable::Member {
                class: "Counter",
                name: "addMany",
            },
        ]
        .map(|callable| argument_label(&callable.to_string(), "value"));
        assert_eq!(
            labels,
            [
                "greet(): argument `value`",
                "new Counter(): argument `value`",
                "Counter.addMany(): argument `value`",
            ]
        );
    }
}
