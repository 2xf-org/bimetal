//! The files of the generated npm package: the private runtime, each
//! namespace's modules, and for the Wasm backend wasm-bindgen's loader and
//! module. The runtime starts with the loader of its backend: Wasm's, whose
//! wasm-bindgen output calls into the module through the runtime's guard, or
//! the Node.js addon's, which `bimetal build` installs beside it.

use std::fmt::Write;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use bimetal_ir::Namespace;
use wasm_bindgen_cli_support::Bindgen;

use crate::contract::{self, Contract};
use crate::literal;
use crate::output;

use super::api;
use super::context::TypeScriptContext;
use super::models;
use super::module::{self, Module};
use super::render;

/// The stems of the modules that the package root holds besides `index`:
/// the private runtime, and the Arrow adapters of a package that passes
/// Arrow values.
pub(crate) const RUNTIME_MODULE: &str = "runtime";
pub(super) const ARROW_MODULE: &str = "arrow";

/// The stems of the modules that each namespace's directory holds besides
/// `index`.
pub(super) const API_MODULE: &str = "api";
pub(super) const MODELS_MODULE: &str = "models";

/// The private runtime, which [`typescript_runtime`] starts with a loader and
/// completes with the contract's schemas.
const RUNTIME_SOURCE: &str = include_str!("runtime.js");

/// The loaders of the Wasm module and of the Node.js addon, the declarations
/// of what the runtime exports with each, and what each says of a resource's
/// `close()`.
const WASM_LOADER: &str = include_str!("loader-wasm.js");
const WASM_DECLARATIONS: &str = include_str!("loader-wasm.d.ts");
const WASM_CLOSE: &str = "Releases the Rust value. Later calls throw. Once a trap aborts the WebAssembly module, this does nothing.";
const NODE_LOADER: &str = include_str!("loader-node.js");
const NODE_DECLARATIONS: &str = include_str!("loader-node.d.ts");
const NODE_CLOSE: &str = "Releases the Rust value. Later calls throw.";

/// The directory beside the runtime that holds the native code, and the
/// files there that have no banner: the Wasm module and the Node.js addon.
pub(crate) const NATIVE_DIRECTORY: &str = "native";
pub(crate) const WASM_MODULE: &str = "native_bg.wasm";
pub(crate) const NODE_ADDON: &str = "native.node";

/// The Arrow adapters, which only the generated JavaScript imports.
const ARROW_SOURCE: &str = include_str!("arrow.js");

const TYPESCRIPT_INDEX: &str = "export * from \"./models.js\";\nexport * from \"./api.js\";\n";

/// The promise that resolves once the Rust code is ready, which the package
/// root exports.
pub(crate) const NATIVE_READY: &str = "nativeReady";

/// The Rust code that the package runs.
pub(crate) enum Native<'build> {
    /// The WebAssembly module at this path, which the package ships.
    Wasm(&'build Path),
    /// The Node.js addon, which `bimetal build` installs as
    /// `native/native.node` for each platform, built from the contract with
    /// this hash.
    Node {
        /// The hash that the addon reports as `__bimetal_contract__`.
        contract_hash: &'build str,
    },
}

/// Emit JavaScript and declarations, and wasm-bindgen's artifacts for the
/// Wasm backend, into `<root>/<directory>`, as they go in the npm project.
pub(crate) fn emit(
    directory: &str,
    contract: &Contract<'_>,
    native: &Native<'_>,
    root: &Path,
) -> Result<()> {
    let package = root.join(directory);
    let (loader, declarations, close) = match native {
        Native::Wasm(module) => {
            emit_wasm(module, &package.join(NATIVE_DIRECTORY))?;
            (WASM_LOADER.to_owned(), WASM_DECLARATIONS, WASM_CLOSE)
        }
        // The loader checks the addon's contract, as the Python package
        // checks its extension's.
        Native::Node { contract_hash } => (
            format!(
                "{NODE_LOADER}requireContract({});\n",
                literal::quoted(contract_hash)
            ),
            NODE_DECLARATIONS,
            NODE_CLOSE,
        ),
    };
    Module {
        javascript: typescript_runtime(&loader, contract),
        declarations: declarations.to_owned(),
    }
    .write(&package, RUNTIME_MODULE)?;
    if contract.uses_arrow() {
        output::write(
            &package.join(format!("{ARROW_MODULE}.js")),
            &module::generated_typescript(ARROW_SOURCE),
        )?;
    }
    for (namespace, items) in contract.namespaces() {
        let namespace_package = namespace
            .typescript_segments()
            .iter()
            .fold(package.clone(), |path, segment| path.join(segment));
        // Each module imports what its own code uses.
        let models_context = TypeScriptContext::new(contract, namespace);
        let api_context = TypeScriptContext::new(contract, namespace);
        models::typescript_models(items, &models_context)?
            .write(&namespace_package, MODELS_MODULE)?;
        api::typescript_api(items, close, &api_context)?.write(&namespace_package, API_MODULE)?;
        let mut index = TYPESCRIPT_INDEX.to_owned();
        // The runtime is private, so the root re-exports its one public value.
        if *namespace == Namespace::root() {
            let _ = writeln!(
                index,
                "export {{ {NATIVE_READY} }} from \"./{RUNTIME_MODULE}.js\";"
            );
        }
        Module {
            javascript: index.clone(),
            declarations: index,
        }
        .write(&namespace_package, "index")?;
    }
    Ok(())
}

/// Emit wasm-bindgen's loader, its declarations, and the module itself,
/// built from `wasm`, into `native`.
fn emit_wasm(wasm: &Path, native: &Path) -> Result<()> {
    let bindgen_output = tempfile::tempdir()?;
    let mut bindgen = Bindgen::new();
    bindgen
        .input_path(wasm)
        .web(true)?
        .typescript(true)
        .omit_default_module_path(false)
        // Debug names and toolchain metadata only enlarge the shipped module.
        .remove_name_section(true)
        .remove_producers_section(true)
        .out_name("native")
        .generate(bindgen_output.path())
        .context("failed to generate TypeScript WebAssembly bindings")?;
    let bindings = fs::read_to_string(bindgen_output.path().join("native.js"))?;
    output::write(
        &native.join("native.js"),
        &module::generated_typescript(&guard_native_exports(&bindings)?),
    )?;
    let declarations = fs::read_to_string(bindgen_output.path().join("native.d.ts"))?;
    output::write(
        &native.join("native.d.ts"),
        &module::generated_typescript(&declarations),
    )?;
    fs::copy(
        bindgen_output.path().join(WASM_MODULE),
        native.join(WASM_MODULE),
    )?;
    Ok(())
}

/// Render the private runtime: `loader`, [`RUNTIME_SOURCE`], then the
/// schema of each model.
#[must_use]
fn typescript_runtime(loader: &str, contract: &Contract<'_>) -> String {
    let mut source = format!("{loader}\n{RUNTIME_SOURCE}const nativeSchemas = {{\n");
    for definition in &contract.manifest.types {
        let _ = writeln!(
            source,
            "  {}: {},",
            render::typescript_property(&definition.identity().to_string()),
            render::spec_literal(&contract::named_spec(definition))
        );
    }
    source.push_str("};\n");
    source
}

/// The statement of wasm-bindgen's loader that keeps the module's exports,
/// through which its bindings make every call into the module.
const NATIVE_EXPORTS: &str = "    wasm = instance.exports;\n";

/// What replaces [`NATIVE_EXPORTS`]: the exports through the guard that the
/// runtime installs, which aborts the module at its first trap.
const GUARDED_NATIVE_EXPORTS: &str = "    wasm = __bimetal_guard(instance.exports);\n";

/// The hook, appended to the bindings, with which the runtime installs its
/// guard before it instantiates the module.
const NATIVE_GUARD_HOOK: &str = "
// Bimetal: the runtime routes every call into the module through its guard.
let __bimetal_guard = (exports) => exports;
export function __bimetal_guard_exports(guard) {
    __bimetal_guard = guard;
}
";

/// Route every call that wasm-bindgen's bindings make into the module through
/// the guard of the generated runtime: the wrappers' calls, and the host's
/// calls of the module's closures and finalizers.
fn guard_native_exports(bindings: &str) -> Result<String> {
    anyhow::ensure!(
        bindings.matches(NATIVE_EXPORTS).count() == 1,
        "wasm-bindgen's native.js does not assign the module's exports where Bimetal expects; Bimetal cannot guard the calls into the module"
    );
    Ok(format!(
        "{}{NATIVE_GUARD_HOOK}",
        bindings.replacen(NATIVE_EXPORTS, GUARDED_NATIVE_EXPORTS, 1)
    ))
}

#[cfg(test)]
mod tests {
    use bimetal_ir::typescript_names;
    use bimetal_ir::{BufferElement, discovery};

    use super::*;

    #[test]
    fn the_loader_checks_the_node_api_that_the_addon_uses() {
        let workspace =
            fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
                .expect("read the workspace manifest");
        let (_, version) = NODE_LOADER
            .split_once("napiVersion >= ")
            .expect("the loader checks the Node-API version");
        let version: String = version.chars().take_while(char::is_ascii_digit).collect();
        assert!(
            workspace.contains(&format!("features = [\"napi{version}\"]")),
            "napi-sys must build the addon for Node-API {version}, which the loader requires"
        );
    }

    #[test]
    fn the_runtime_and_the_loaders_spell_what_rust_and_the_cli_do() {
        let (_, table) = RUNTIME_SOURCE
            .split_once("const bufferConstructors = {")
            .expect("runtime.js defines `bufferConstructors`");
        let (table, _) = table.split_once("};").expect("the table ends");
        let entries: Vec<&str> = table
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .collect();
        let expected: Vec<String> = BufferElement::ALL
            .iter()
            .map(|element| format!("{}: {}", element.numpy_name(), element.typed_array()))
            .collect();
        assert_eq!(entries, expected);
        // The Node.js addon's own argument errors name a typed array alike.
        assert!(RUNTIME_SOURCE.contains("BufferType.name.startsWith(\"I\") ? \"an\" : \"a\""));
        assert!(
            RUNTIME_SOURCE.contains(&format!("error?.{}", typescript_names::ERROR_CODE_PROPERTY))
        );
        assert!(WASM_LOADER.contains(&format!("`{}${{detail}}`", typescript_names::PANIC_PREFIX)));
        assert!(WASM_LOADER.contains(&format!("\"./{NATIVE_DIRECTORY}/native.js\"")));
        assert!(WASM_LOADER.contains(&format!("\"./{NATIVE_DIRECTORY}/{WASM_MODULE}\"")));
        assert!(NODE_LOADER.contains(&format!("\"./{NATIVE_DIRECTORY}/{NODE_ADDON}\"")));
        assert!(NODE_LOADER.contains(&format!(
            "native.{} !== contract",
            discovery::CONTRACT_HASH_ATTRIBUTE
        )));
    }

    #[test]
    fn every_call_into_the_module_goes_through_the_runtime_guard() {
        let bindings = "let wasm;\nfunction __wbg_finalize_init(instance, module) {\n    wasm = instance.exports;\n    wasm.__wbindgen_start();\n}\n";

        let guarded = guard_native_exports(bindings).expect("guard the bindings");

        assert!(guarded.contains(
            "    wasm = __bimetal_guard(instance.exports);\n    wasm.__wbindgen_start();"
        ));
        assert!(guarded.ends_with(NATIVE_GUARD_HOOK));
        let error = guard_native_exports("let wasm;\n")
            .expect_err("bindings without the anchor cannot be guarded")
            .to_string();
        assert!(error.contains("cannot guard the calls"), "{error}");
    }
}
