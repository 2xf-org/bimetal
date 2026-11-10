//! Whether the WebAssembly module exports what the contract describes, and
//! can load at all.
//!
//! Discovery reads the contract from a build for this platform, and the
//! Python extension is built for it with the same configuration, so the two
//! agree. The Wasm module is built for `wasm32`, where a `#[cfg]` can leave
//! out an export that the generated TypeScript calls, or add one that no
//! generated package has.
//!
//! A dependency that wraps a C library can compile for `wasm32` without its C
//! code, and then the module imports the C functions from `env`, which nothing
//! provides, so it could not load.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use bimetal_ir::{ExportKind, Manifest, ResourceDef};

use crate::contract::Item;
use crate::problems;

/// The id of the import section in the WebAssembly binary format.
const IMPORT_SECTION: u8 = 2;
/// The id of the export section in the WebAssembly binary format.
const EXPORT_SECTION: u8 = 7;
/// The module that a linker imports a symbol from when nothing defines it.
const UNRESOLVED_MODULE: &str = "env";
/// How many missing symbols an error names.
const LISTED_SYMBOLS: usize = 8;

/// Check that the Wasm module exports every function, resource, and resource
/// member of the contract, and no other function or resource, and that every
/// symbol it needs is defined.
pub(super) fn check_wasm_exports(manifest: &Manifest, module: &Path) -> Result<()> {
    let bytes = fs::read(module).with_context(|| format!("failed to read {}", module.display()))?;
    let unresolved = unresolved_symbols(&bytes)
        .with_context(|| format!("failed to read the imports of {}", module.display()))?;
    if let Some(first) = unresolved.first() {
        let shown = unresolved[..unresolved.len().min(LISTED_SYMBOLS)].join(", ");
        let more = unresolved.len().saturating_sub(LISTED_SYMBOLS);
        let more = if more > 0 {
            format!(", and {more} more")
        } else {
            String::new()
        };
        bail!(
            "the WebAssembly module needs symbols that nothing defines, so it could not load: {shown}{more}\nA dependency probably wraps a C library that did not compile for WebAssembly. Run `cargo tree --target wasm32-unknown-unknown -i <crate>-sys` to find who enables it, for `{first}`: turn that feature off, compile the library with a C toolchain for Wasm, or define these symbols in Rust under `#[cfg(target_arch = \"wasm32\")]`"
        );
    }
    let exports = export_names(&bytes)
        .with_context(|| format!("failed to read the exports of {}", module.display()))?;
    let mut expected = BTreeSet::new();
    let mut missing = Vec::new();
    let mut require = |name: String, item: Item<'_>| {
        if !exports.contains(&name) {
            missing.push(item.to_string());
        }
        expected.insert(name);
    };
    for function in &manifest.functions {
        require(function.native_name.clone(), Item::Function(function));
    }
    for resource in &manifest.resources {
        // wasm-bindgen exports the members of a class as `<class>_<member>`,
        // with the class's name in lower case. Every resource has `close`.
        let class = resource.native_name.to_lowercase();
        require(
            format!("{class}_{}", ResourceDef::CLOSE),
            Item::Resource(resource),
        );
        for factory in &resource.factories {
            let item = Item::Constructor {
                resource,
                constructor: factory,
            };
            require(format!("{class}_{}", factory.host_name), item);
        }
        for method in &resource.methods {
            let method = &method.callable;
            let item = Item::Method { resource, method };
            require(format!("{class}_{}", method.host_name), item);
        }
    }
    // A function, or a class through its `close`, that the contract lacks.
    let close = format!("_{}", ResourceDef::CLOSE);
    let unexpected = exports.iter().any(|name| {
        let native = name.starts_with(ExportKind::Function.prefix())
            || name.starts_with(ExportKind::Resource.prefix()) && name.ends_with(&close);
        native && !expected.contains(name)
    });
    if missing.is_empty() && !unexpected {
        return Ok(());
    }
    let mut message = String::new();
    if !missing.is_empty() {
        message = format!(
            "the WebAssembly module lacks these exports, so the TypeScript package cannot call them:{}\n",
            problems::list(&missing)
        );
    }
    if unexpected {
        message.push_str(
            "the WebAssembly module has functions or resources that compile for it alone, so no generated package has them\n",
        );
    }
    bail!(
        "{message}Bimetal reads the contract on this platform, so each export must compile for it and for WebAssembly alike: remove a `#[cfg]` on an export that holds on one and not the other"
    )
}

/// Return the names of the symbols that a WebAssembly module imports from
/// [`UNRESOLVED_MODULE`], in the order of its import section.
fn unresolved_symbols(module: &[u8]) -> Result<Vec<String>> {
    let mut reader = Reader(module);
    if reader.bytes(8)? != b"\0asm\x01\0\0\0" {
        bail!("it is not a WebAssembly module");
    }
    while !reader.0.is_empty() {
        let id = reader.byte()?;
        let size = reader.number()?;
        let mut section = Reader(reader.bytes(size)?);
        if id != IMPORT_SECTION {
            continue;
        }
        let mut symbols = Vec::new();
        for _ in 0..section.number()? {
            let from = section.text()?;
            let name = section.text()?;
            section.import_description()?;
            if from == UNRESOLVED_MODULE {
                symbols.push(name);
            }
        }
        return Ok(symbols);
    }
    Ok(Vec::new())
}

/// Return the names that a WebAssembly module exports.
fn export_names(module: &[u8]) -> Result<BTreeSet<String>> {
    let mut reader = Reader(module);
    if reader.bytes(8)? != b"\0asm\x01\0\0\0" {
        bail!("it is not a WebAssembly module");
    }
    while !reader.0.is_empty() {
        let id = reader.byte()?;
        let size = reader.number()?;
        let mut section = Reader(reader.bytes(size)?);
        if id != EXPORT_SECTION {
            continue;
        }
        let mut names = BTreeSet::new();
        for _ in 0..section.number()? {
            let length = section.number()?;
            names.insert(String::from_utf8(section.bytes(length)?.to_vec())?);
            // The kind of the export, and the index of what it exports.
            section.byte()?;
            section.number()?;
        }
        return Ok(names);
    }
    Ok(BTreeSet::new())
}

/// The unread part of a WebAssembly module.
struct Reader<'module>(&'module [u8]);

impl<'module> Reader<'module> {
    /// Read the next `count` bytes.
    fn bytes(&mut self, count: usize) -> Result<&'module [u8]> {
        let (read, rest) = self
            .0
            .split_at_checked(count)
            .context("the module ends early")?;
        self.0 = rest;
        Ok(read)
    }

    /// Read one byte.
    fn byte(&mut self) -> Result<u8> {
        let (&byte, rest) = self.0.split_first().context("the module ends early")?;
        self.0 = rest;
        Ok(byte)
    }

    /// Read a name: its length, then its UTF-8 bytes.
    fn text(&mut self) -> Result<String> {
        let length = self.number()?;
        Ok(String::from_utf8(self.bytes(length)?.to_vec())?)
    }

    /// Skip the kind of an import, and the type of what it imports.
    fn import_description(&mut self) -> Result<()> {
        match self.byte()? {
            // A function's type index.
            0 => {
                self.number()?;
            }
            // A table's element type and limits.
            1 => {
                self.byte()?;
                self.limits()?;
            }
            // A memory's limits.
            2 => self.limits()?,
            // A global's value type and mutability.
            3 => {
                self.bytes(2)?;
            }
            // A tag's attribute and type index.
            4 => {
                self.byte()?;
                self.number()?;
            }
            kind => bail!("the module has an import of unknown kind {kind}"),
        }
        Ok(())
    }

    /// Skip the limits of a table or a memory: a flag that says whether a
    /// maximum follows the minimum.
    fn limits(&mut self) -> Result<()> {
        let has_maximum = self.byte()? & 1 == 1;
        self.number()?;
        if has_maximum {
            self.number()?;
        }
        Ok(())
    }

    /// Read an unsigned 32-bit LEB128 number.
    fn number(&mut self) -> Result<usize> {
        let mut value = 0;
        for shift in (0..32).step_by(7) {
            let byte = self.byte()?;
            value |= usize::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        bail!("the module has a malformed number")
    }
}

#[cfg(test)]
mod tests {
    use bimetal_ir::FunctionDef;

    use super::*;
    use crate::fixtures;

    /// Encode a module whose import section has one memory, one table, and one
    /// function, imported from `env` under `symbol` and from the bindings.
    fn module_importing(symbol: &str) -> Vec<u8> {
        let name = |text: &str| -> Vec<u8> {
            let mut bytes = vec![u8::try_from(text.len()).expect("a short name")];
            bytes.extend_from_slice(text.as_bytes());
            bytes
        };
        let mut section = vec![3];
        section.extend(name("wbg"));
        section.extend(name("memory"));
        section.extend_from_slice(&[2, 1, 1, 2]);
        section.extend(name("wbg"));
        section.extend(name("table"));
        section.extend_from_slice(&[1, 0x70, 0, 4]);
        section.extend(name("env"));
        section.extend(name(symbol));
        section.extend_from_slice(&[0, 7]);
        let mut bytes = b"\0asm\x01\0\0\0".to_vec();
        bytes.push(IMPORT_SECTION);
        bytes.push(u8::try_from(section.len()).expect("a small section"));
        bytes.extend(section);
        bytes
    }

    #[test]
    fn symbols_that_nothing_defines_are_found() {
        assert_eq!(
            unresolved_symbols(&module_importing("ZSTD_compress")).expect("a valid module"),
            ["ZSTD_compress"]
        );
        assert!(
            unresolved_symbols(&module(&["native_greet"]))
                .expect("a valid module")
                .is_empty()
        );
    }

    #[test]
    fn a_module_with_unresolved_symbols_fails_the_build_and_names_them() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("module.wasm");
        fs::write(&path, module_importing("ZSTD_compress")).expect("write the module");

        let error = check_wasm_exports(&fixtures::manifest(), &path)
            .expect_err("nothing defines the symbol")
            .to_string();

        assert!(
            error.contains("needs symbols that nothing defines"),
            "{error}"
        );
        assert!(error.contains("ZSTD_compress"), "{error}");
    }

    /// Encode a module whose export section exports `names`.
    fn module(names: &[&str]) -> Vec<u8> {
        let mut section = vec![u8::try_from(names.len()).expect("few exports")];
        for name in names {
            section.push(u8::try_from(name.len()).expect("a short name"));
            section.extend_from_slice(name.as_bytes());
            // A function export, of function 0.
            section.extend_from_slice(&[0, 0]);
        }
        let mut bytes = b"\0asm\x01\0\0\0".to_vec();
        // A custom section first, which the reader skips.
        bytes.extend_from_slice(&[0, 2, 1, b'x']);
        bytes.push(EXPORT_SECTION);
        // The section is longer than 127 bytes, so its size takes two bytes.
        let size = u16::try_from(section.len()).expect("a small section");
        bytes.push(u8::try_from(size & 0x7f).expect("seven bits") | 0x80);
        bytes.push(u8::try_from(size >> 7).expect("a small size"));
        bytes.extend(section);
        bytes
    }

    fn manifest() -> Manifest {
        let function = |rust_name: &str, native_name: &str| FunctionDef {
            native_name: native_name.to_owned(),
            ..fixtures::function(rust_name, rust_name, Vec::new())
        };
        let mut manifest = fixtures::manifest();
        manifest.functions = vec![
            function("greet", "__bimetal_function_app_01"),
            function("file_size", "__bimetal_function_app_02"),
        ];
        manifest.resources.push(ResourceDef {
            name: "Store".to_owned(),
            native_name: "__bimetal_resource_App_03".to_owned(),
            constructor: None,
            ..fixtures::recorder(
                vec![fixtures::callable("open", "open", Vec::new())],
                vec![fixtures::method("read_all", "readAll")],
            )
        });
        manifest
    }

    fn check(names: &[&str]) -> Result<()> {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("module.wasm");
        fs::write(&path, module(names)).expect("write the module");
        check_wasm_exports(&manifest(), &path)
    }

    #[test]
    fn exports_are_read_from_the_export_section() {
        let names = [
            "memory",
            "__bimetal_function_app_01",
            "a_name_long_enough_to_need_a_section_size_of_two_bytes_in_the_encoding",
        ];

        assert_eq!(
            export_names(&module(&names)).expect("read the exports"),
            names.iter().map(|name| (*name).to_owned()).collect()
        );
        assert!(
            export_names(b"\0asm\x01\0\0\0")
                .expect("no sections")
                .is_empty()
        );
        assert!(export_names(b"not wasm").is_err());
    }

    #[test]
    fn every_export_of_the_contract_must_be_in_the_module() {
        let complete = [
            "memory",
            "__bimetal_function_app_01",
            "__bimetal_function_app_02",
            "__bimetal_resource_app_03_close",
            "__bimetal_resource_app_03_open",
            "__bimetal_resource_app_03_readAll",
            "__bimetal_resource_app_03___bimetal_constructor_new",
        ];
        check(&complete).expect("the module has every export");

        let error = check(&[
            "__bimetal_function_app_01",
            "__bimetal_resource_app_03_close",
            "__bimetal_resource_app_03_open",
        ])
        .expect_err("exports are missing")
        .to_string();
        assert!(
            error.contains("lacks these exports")
                && error.contains("\n  - function `test::file_size`\n")
                && error.contains("\n  - method `test::Store::read_all`\n")
                && !error.contains("greet"),
            "{error}"
        );

        let error = check(&[&complete[..], &["__bimetal_function_app_04"]].concat())
            .expect_err("an export is in the module alone")
            .to_string();
        assert!(
            error.contains("compile for it alone") && !error.contains("lacks"),
            "{error}"
        );
    }
}
