//! The files of the generated Python package, and the ownership warning and
//! export list that frame every generated module.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::Path;

use anyhow::Result;
use bimetal_ir::Namespace;
use bimetal_ir::discovery;
use bimetal_ir::python_names::{NATIVE_MODULE, PANIC_EXCEPTION, RUNTIME_MODULE};

use crate::contract::{self, Contract, NamespaceItems};
use crate::literal;
use crate::output;

use super::api;
use super::context::{self, LINE_WIDTH, PythonContext};
use super::models;
use super::render;

/// The modules that each namespace's package holds besides `__init__.py`.
pub(crate) const API_MODULE: &str = "api";
pub(crate) const MODELS_MODULE: &str = "models";

/// The runtime, which [`python_runtime`] completes with what it knows of the
/// contract.
const RUNTIME_SOURCE: &str = include_str!("runtime.py");

/// The declarations of the runtime's tables, which [`python_runtime`] gives
/// their values.
const CONTRACT_DECLARATION: &str = "CONTRACT: str\n";
const SCHEMAS_DECLARATION: &str = "NATIVE_SCHEMAS: dict[str, typing.Any]";
const ERRORS_DECLARATION: &str = "NATIVE_ERRORS: dict[str, tuple[str, str]]";

/// The extension's stub. Its exports are wrapper targets whose names are
/// hashes, which the generated API reaches with `getattr` and types itself, so
/// the stub declares only the names that the runtime imports or reads.
/// The extension registers pyo3's `PanicException`, and `Revalidate` and
/// `Dumped`, with which the generated code calls again with Pydantic's dump of
/// an argument that the extension cannot read as it is.
#[must_use]
fn python_native_stub() -> String {
    format!(
        "\"\"\"\nThe types of the extension, which `{RUNTIME_MODULE}.py` loads.\n\"\"\"\n\nclass {PANIC_EXCEPTION}(BaseException):\n    \"\"\"\n    Raised when the Rust implementation panics.\n    \"\"\"\n\nclass Revalidate(Exception):\n    \"\"\"\n    Raised for an argument that the extension reads only after Pydantic's dump.\n    \"\"\"\n\nclass Dumped:\n    def __init__(self, value: object) -> None: ...\n\n{}: str\n",
        discovery::CONTRACT_HASH_ATTRIBUTE
    )
}

/// The symbols that one namespace's generated modules export.
struct ExportNames {
    /// The exports of `models.py`.
    models: BTreeSet<String>,
    /// The exports of `api.py`.
    api: BTreeSet<String>,
    /// The runtime's names that the package root exports.
    runtime: BTreeSet<String>,
    /// The namespaces below this one, which its `__init__.py` imports.
    namespaces: BTreeSet<String>,
}

/// Emit the Bimetal-owned files of the Python package `python_package`, such
/// as `acme.tools`, into `root`, as they go in the Python project.
///
/// `contract_hash` identifies the contract that the extension must be built
/// from; the package refuses at import an extension with another one.
pub(crate) fn emit(
    python_package: &str,
    contract: &Contract<'_>,
    contract_hash: &str,
    root: &Path,
) -> Result<()> {
    let package = root.join(render::python_package_directory(python_package));
    // The stub types the extension for type checkers.
    output::write(
        &package.join(format!("{NATIVE_MODULE}.pyi")),
        &generated_python(&python_native_stub()),
    )?;
    output::write(
        &package.join(format!("{RUNTIME_MODULE}.py")),
        &generated_python(&python_runtime(contract, python_package, contract_hash)),
    )?;
    output::write(&package.join("py.typed"), &output::banner("#", ""))?;

    for (namespace, items) in contract.namespaces() {
        let namespace_package = namespace
            .python_segments()
            .iter()
            .fold(package.clone(), |path, segment| path.join(segment));
        // Each module imports what its own code uses.
        let (models_imports, api_imports) = (RefCell::default(), RefCell::default());
        let models_context =
            PythonContext::new(contract, python_package, namespace, &models_imports);
        let api_context = PythonContext::new(contract, python_package, namespace, &api_imports);
        let exports = python_export_names(items, contract, namespace);
        output::write(
            &namespace_package.join(format!("{MODELS_MODULE}.py")),
            &generated_python_module(
                &models::python_models(items, &models_context)?,
                &exports.models,
            ),
        )?;
        output::write(
            &namespace_package.join(format!("{API_MODULE}.py")),
            &generated_python_module(&api::python_api(items, &api_context)?, &exports.api),
        )?;
        let module = render::python_namespace_module(python_package, namespace);
        output::write(
            &namespace_package.join("__init__.py"),
            &generated_python(&python_init(&module, &exports)),
        )?;
    }
    Ok(())
}

/// Render the runtime: [`RUNTIME_SOURCE`] with the hash of the contract that
/// the extension must match, the schema of each model, and the module and
/// class of each error, inside `python_package`.
#[must_use]
fn python_runtime(contract: &Contract<'_>, python_package: &str, contract_hash: &str) -> String {
    let schemas: Vec<String> = contract
        .manifest
        .types
        .iter()
        .map(|definition| {
            render::python_json_lines(
                &format!(
                    "    {}: ",
                    literal::quoted(&definition.identity().to_string())
                ),
                &contract::named_spec(definition),
                "",
                "    ",
            )
        })
        .collect();
    let errors: Vec<String> = contract
        .manifest
        .errors
        .iter()
        .map(|error| {
            let namespace = contract.namespace(&error.owner, &error.rust_module);
            let module = format!(
                "{}.{API_MODULE}",
                render::python_namespace_module(python_package, &namespace)
            );
            format!(
                "    {}: ({}, {})",
                literal::quoted(&error.identity().to_string()),
                literal::quoted(&module),
                literal::quoted(&error.name)
            )
        })
        .collect();
    let table = |declaration: &str, entries: &[String]| {
        if entries.is_empty() {
            return format!("{declaration} = {{}}");
        }
        let mut table = format!("{declaration} = {{\n");
        for entry in entries {
            let _ = writeln!(table, "{entry},");
        }
        table.push('}');
        table
    };
    for declaration in [
        CONTRACT_DECLARATION,
        SCHEMAS_DECLARATION,
        ERRORS_DECLARATION,
    ] {
        debug_assert!(
            RUNTIME_SOURCE.matches(declaration).count() == 1,
            "runtime.py declares `{declaration}` once"
        );
    }
    RUNTIME_SOURCE
        .replacen(
            CONTRACT_DECLARATION,
            &format!("CONTRACT = {}\n", literal::quoted(contract_hash)),
            1,
        )
        .replacen(
            SCHEMAS_DECLARATION,
            &table(SCHEMAS_DECLARATION, &schemas),
            1,
        )
        .replacen(ERRORS_DECLARATION, &table(ERRORS_DECLARATION, &errors), 1)
}

/// Prefix generated Python source with the ownership warning.
#[must_use]
fn generated_python(source: &str) -> String {
    format!("{}\n{source}", output::banner("#", ""))
}

/// Prefix a module with the ownership warning, and append its export list,
/// after two blank lines when the module ends with a class or a function.
#[must_use]
fn generated_python_module(source: &str, exports: &BTreeSet<String>) -> String {
    let mut generated = generated_python(source);
    if generated
        .trim_end()
        .lines()
        .last()
        .is_some_and(|line| line.starts_with(' '))
    {
        generated.push('\n');
    }
    push_exports(&mut generated, exports);
    generated
}

/// Render the entry point of the namespace `module`, which re-exports its
/// modules' names and imports the namespaces below it.
///
/// The imports and `__all__` are explicit, because type checkers, such as
/// pyright, cannot follow an `__all__` built from other modules' lists.
#[must_use]
fn python_init(module: &str, exports: &ExportNames) -> String {
    let mut source = format!("\"\"\"\nThe public API of `{module}`.\n\"\"\"\n");
    let mut imports = Vec::new();
    if !exports.namespaces.is_empty() {
        imports.push(context::import_list(".", &exports.namespaces));
    }
    for (module, names) in [
        (API_MODULE, &exports.api),
        (RUNTIME_MODULE, &exports.runtime),
        (MODELS_MODULE, &exports.models),
    ] {
        if !names.is_empty() {
            imports.push(context::import_list(&format!(".{module}"), names));
        }
    }
    if !imports.is_empty() {
        let _ = writeln!(source, "\n{}", imports.join("\n"));
    }
    push_exports(
        &mut source,
        exports
            .namespaces
            .iter()
            .chain(&exports.api)
            .chain(&exports.runtime)
            .chain(&exports.models),
    );
    source
}

/// Append `__all__`, which lists `names` in order, after a blank line: on one
/// line when it fits, and otherwise one name per line.
fn push_exports<'name>(source: &mut String, names: impl IntoIterator<Item = &'name String>) {
    let names: Vec<String> = context::ordered(names, /* fold_case */ false)
        .into_iter()
        .map(literal::quoted)
        .collect();
    if !source.is_empty() {
        source.push('\n');
    }
    // Strict pyright reads an empty list without a type as `list[Unknown]`.
    if names.is_empty() {
        source.push_str("__all__: list[str] = []\n");
        return;
    }
    let line = format!("__all__ = [{}]", names.join(", "));
    if line.len() <= LINE_WIDTH {
        let _ = writeln!(source, "{line}");
        return;
    }
    source.push_str("__all__ = [\n");
    for name in names {
        let _ = writeln!(source, "    {name},");
    }
    source.push_str("]\n");
}

/// Compute the model and API symbols exported by one namespace.
#[must_use]
fn python_export_names(
    items: &NamespaceItems<'_>,
    contract: &Contract<'_>,
    namespace: &Namespace,
) -> ExportNames {
    let runtime = if *namespace == Namespace::root() {
        api::python_root_exports(contract)
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        BTreeSet::new()
    };
    let path = namespace.python_segments();
    let namespaces = contract
        .namespaces()
        .keys()
        .filter_map(|other| {
            let other = other.python_segments();
            (other.len() > path.len() && other.starts_with(&path))
                .then(|| other[path.len()].clone())
        })
        .collect();
    ExportNames {
        models: render::python_model_names(items),
        api: items
            .api_names()
            .into_iter()
            .map(|name| name.python)
            .collect(),
        runtime,
        namespaces,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_imports_and_exports_the_exception_that_the_extension_registers() {
        assert!(RUNTIME_SOURCE.contains(&format!(
            "    from .{NATIVE_MODULE} import Dumped, {PANIC_EXCEPTION}, Revalidate\n"
        )));
        assert!(RUNTIME_SOURCE.contains(&format!("    \"{PANIC_EXCEPTION}\",\n")));
    }

    #[test]
    fn the_runtime_reads_the_contract_hash_where_the_extension_keeps_it() {
        let read = format!(
            "getattr(native, \"{}\", None)",
            discovery::CONTRACT_HASH_ATTRIBUTE
        );
        assert!(
            RUNTIME_SOURCE.contains(&read),
            "runtime.py must read {read}"
        );
    }

    #[test]
    fn the_runtime_declares_each_table_once_for_the_build_to_fill() {
        for declaration in [
            CONTRACT_DECLARATION,
            SCHEMAS_DECLARATION,
            ERRORS_DECLARATION,
        ] {
            assert_eq!(
                RUNTIME_SOURCE.matches(declaration).count(),
                1,
                "{declaration}"
            );
        }
    }

    #[test]
    fn generated_modules_list_their_exports_constants_and_classes_first() {
        let exports = BTreeSet::from([
            "zip_all".to_owned(),
            "Zulu".to_owned(),
            "Alpha".to_owned(),
            "LIMIT".to_owned(),
        ]);
        let generated = generated_python_module("class Alpha:\n    pass\n", &exports);

        // Two blank lines follow a class.
        assert!(
            generated.ends_with(
                "    pass\n\n\n__all__ = [\"LIMIT\", \"Alpha\", \"Zulu\", \"zip_all\"]\n"
            ),
            "{generated}"
        );
        // An empty list says what it holds, for strict type checkers.
        let empty =
            generated_python_module("from __future__ import annotations\n", &BTreeSet::new());
        assert!(empty.ends_with("annotations\n\n__all__: list[str] = []\n"));
    }

    #[test]
    fn generated_package_init_imports_and_lists_every_export_explicitly() {
        let exports = ExportNames {
            models: BTreeSet::from(["Point".to_owned()]),
            api: BTreeSet::from(["distance".to_owned()]),
            runtime: BTreeSet::from([PANIC_EXCEPTION.to_owned()]),
            namespaces: BTreeSet::from(["stats".to_owned()]),
        };
        let empty = ExportNames {
            models: BTreeSet::new(),
            api: BTreeSet::new(),
            runtime: BTreeSet::new(),
            namespaces: BTreeSet::new(),
        };

        assert_eq!(
            python_init("app", &exports),
            "\"\"\"\nThe public API of `app`.\n\"\"\"\n\nfrom . import stats\nfrom .api import distance\nfrom .bimetal import PanicException\nfrom .models import Point\n\n__all__ = [\"PanicException\", \"Point\", \"distance\", \"stats\"]\n"
        );
        assert_eq!(
            python_init("app.stats", &empty),
            "\"\"\"\nThe public API of `app.stats`.\n\"\"\"\n\n__all__: list[str] = []\n"
        );
    }
}
