//! The files of the generated Python package, and the ownership warning and
//! export list that frame every generated module.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::Path;

use anyhow::Result;
use bimetal_ir::Namespace;
use bimetal_ir::{discovery, python_names};

use crate::contract::{self, Contract, NamespaceItems};
use crate::literal;
use crate::output;

use super::api;
use super::context::PythonContext;
use super::models;
use super::render;

/// The private module that loads the extension and holds the shared adapters.
pub(crate) const RUNTIME_MODULE: &str = "_runtime";

/// The modules that each namespace's package holds besides `__init__.py`.
pub(crate) const API_MODULE: &str = "api";
pub(crate) const MODELS_MODULE: &str = "models";

/// The private runtime, which [`python_runtime`] completes with what it
/// knows of the contract.
const RUNTIME_SOURCE: &str = include_str!("runtime.py");

/// The extension's stub. Its exports are wrapper targets whose names are
/// hashes, which the generated API reaches with `getattr` and types itself, so
/// the stub declares only the names that the runtime imports or reads.
/// `PanicException` is pyo3's, which the extension registers.
#[must_use]
fn python_native_stub() -> String {
    format!(
        "class {}(BaseException):\n    \"\"\"Raised when the Rust implementation panics.\"\"\"\n\n\n{}: str\n",
        python_names::PANIC_EXCEPTION,
        discovery::CONTRACT_HASH_ATTRIBUTE
    )
}

/// The symbols that one namespace's generated modules export.
struct ExportNames {
    /// The exports of `models.py`.
    models: BTreeSet<String>,
    /// The exports of `api.py`.
    api: BTreeSet<String>,
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
    // The extension and the runtime are private modules; the stub types the
    // extension for type checkers.
    output::write(
        &package.join(format!("{}.pyi", python_names::NATIVE_MODULE)),
        &generated_python(&python_native_stub()),
    )?;
    output::write(
        &package.join(format!("{RUNTIME_MODULE}.py")),
        &generated_python(&python_runtime(contract, contract_hash)),
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
        output::write(
            &namespace_package.join("__init__.py"),
            &generated_python(&python_init(&exports)),
        )?;
    }
    Ok(())
}

/// Render the private runtime: [`RUNTIME_SOURCE`], then the call that checks the
/// extension's contract hash, and the schema of each model.
#[must_use]
fn python_runtime(contract: &Contract<'_>, contract_hash: &str) -> String {
    let mut source = format!(
        "{RUNTIME_SOURCE}\n\nrequire_contract({})\n\nnative_schemas = {{\n",
        literal::quoted(contract_hash)
    );
    for definition in &contract.manifest.types {
        let _ = writeln!(
            source,
            "    {}: {},",
            literal::quoted(&definition.identity().to_string()),
            render::python_json(&contract::named_spec(definition))
        );
    }
    source.push_str("}\n");
    source
}

/// Prefix generated Python source with the ownership warning.
#[must_use]
fn generated_python(source: &str) -> String {
    format!("{}\n{source}", output::banner("#", ""))
}

/// Prefix a module with the ownership warning, and append its export list.
#[must_use]
fn generated_python_module(source: &str, exports: &BTreeSet<String>) -> String {
    let mut generated = generated_python(source);
    push_exports(&mut generated, exports);
    generated
}

/// Render a generated subpackage's entrypoint, which re-exports its modules.
///
/// The imports and `__all__` are explicit, because type checkers, such as
/// pyright, cannot follow an `__all__` built from other modules' lists.
#[must_use]
fn python_init(exports: &ExportNames) -> String {
    let mut source = String::new();
    for (module, names) in [(API_MODULE, &exports.api), (MODELS_MODULE, &exports.models)] {
        if names.is_empty() {
            continue;
        }
        let _ = writeln!(source, "from .{module} import (");
        for name in names {
            let _ = writeln!(source, "    {name},");
        }
        source.push_str(")\n");
    }
    push_exports(&mut source, exports.api.iter().chain(&exports.models));
    source
}

/// Append `__all__`, which lists `names` in order, after a blank line.
fn push_exports<'name>(source: &mut String, names: impl IntoIterator<Item = &'name String>) {
    let names: BTreeSet<&String> = names.into_iter().collect();
    if !source.is_empty() {
        source.push('\n');
    }
    // Strict pyright reads an empty list without a type as `list[Unknown]`.
    if names.is_empty() {
        source.push_str("__all__: list[str] = []\n");
        return;
    }
    source.push_str("__all__ = [\n");
    for name in names {
        let _ = writeln!(source, "    {},", literal::quoted(name));
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
    let mut api: BTreeSet<String> = items
        .api_names()
        .into_iter()
        .map(|name| name.python)
        .collect();
    if *namespace == Namespace::root() {
        api.extend(
            api::python_root_exports(contract)
                .into_iter()
                .map(str::to_owned),
        );
    }
    ExportNames {
        models: render::python_model_names(items),
        api,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_imports_the_exception_that_the_extension_registers() {
        assert!(RUNTIME_SOURCE.contains(&format!(
            "from .{} import {}\n",
            python_names::NATIVE_MODULE,
            python_names::PANIC_EXCEPTION
        )));
        assert!(RUNTIME_SOURCE.contains(&format!("    \"{}\",\n", python_names::PANIC_EXCEPTION)));
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
    fn generated_modules_have_sorted_explicit_exports() {
        let exports = BTreeSet::from(["Zulu".to_owned(), "Alpha".to_owned()]);
        let generated = generated_python_module("from __future__ import annotations\n", &exports);

        assert!(generated.ends_with("\n\n__all__ = [\n    \"Alpha\",\n    \"Zulu\",\n]\n"));
        // An empty list says what it holds, for strict type checkers.
        let empty =
            generated_python_module("from __future__ import annotations\n", &BTreeSet::new());
        assert!(empty.ends_with("annotations\n\n__all__: list[str] = []\n"));
    }

    #[test]
    fn generated_package_init_imports_and_lists_every_export_explicitly() {
        let exports = ExportNames {
            models: BTreeSet::from(["Point".to_owned()]),
            api: BTreeSet::from(["distance".to_owned(), "PanicException".to_owned()]),
        };
        let empty = ExportNames {
            models: BTreeSet::new(),
            api: BTreeSet::new(),
        };

        assert_eq!(
            python_init(&exports),
            "from .api import (\n    PanicException,\n    distance,\n)\nfrom .models import (\n    Point,\n)\n\n__all__ = [\n    \"PanicException\",\n    \"Point\",\n    \"distance\",\n]\n"
        );
        assert_eq!(python_init(&empty), "__all__: list[str] = []\n");
    }
}
