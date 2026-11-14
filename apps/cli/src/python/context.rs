//! The state that every renderer of one generated Python module shares, and
//! the imports that they record.
//!
//! A module renders its body first. Every name that the body spells through
//! the context is recorded, and [`PythonContext::module`] then writes the
//! imports of exactly those names above it.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Write;

use bimetal_ir::Namespace;
use bimetal_ir::python_names::{BUILTINS, RUNTIME_MODULE};

use crate::contract::Contract;

use super::package::MODELS_MODULE;
use super::render;

/// The modules that generated modules import whole, in the order of a
/// module's imports: the standard library, then other packages. Each binds
/// its first segment, one of [`python_names::IMPORTED_MODULES`], through
/// which the code reaches its members, such as `typing.Literal`.
///
/// [`python_names::IMPORTED_MODULES`]: bimetal_ir::python_names::IMPORTED_MODULES
const STANDARD_MODULES: [&str; 5] = ["builtins", "collections.abc", "datetime", "enum", "typing"];
const PACKAGE_MODULES: [&str; 4] = ["numpy", "numpy.typing", "pyarrow", "pydantic"];

/// The prefixes of the module constants that hold type adapters and the
/// pyarrow importers of Arrow results, each followed by an index.
pub(super) const ADAPTER_CONSTANT: &str = "BIMETAL_ADAPTER_";
pub(super) const ARROW_CONSTANT: &str = "BIMETAL_ARROW_";

/// The width that a generated line fits in, as the formatter's line length.
pub(super) const LINE_WIDTH: usize = 120;

/// What one generated module imports, as its renderers spell each name.
#[derive(Default)]
pub(super) struct Imports {
    /// Modules from outside the package; see [`STANDARD_MODULES`].
    external: BTreeSet<&'static str>,
    /// The models and buffer types of the module's own namespace, which
    /// `api.py` imports from `models.py`.
    models: BTreeSet<String>,
    /// The other namespaces' modules, as import statements.
    namespaces: BTreeSet<String>,
    /// Whether the module uses the runtime.
    runtime: bool,
    /// The module's constants, as their expressions and names, in the order
    /// of first use.
    constants: Vec<(String, String)>,
}

/// Shared contract and namespace state for one generated Python module, in
/// one of its scopes.
///
/// Everything it borrows outlives the `emit` call that renders the package.
pub(super) struct PythonContext<'emit> {
    pub(super) contract: &'emit Contract<'emit>,
    /// The generated import package, such as `my_application`.
    pub(super) package: &'emit str,
    /// The namespace of the module being rendered.
    pub(super) namespace: &'emit Namespace,
    /// The builtins that a contract name of the current scope hides, such as
    /// a function named `list`.
    hidden_builtins: BTreeSet<&'static str>,
    /// What the module's scopes have imported so far.
    imports: &'emit RefCell<Imports>,
}

impl<'emit> PythonContext<'emit> {
    /// Create the context of one module, before its own names are known.
    #[must_use]
    pub(super) fn new(
        contract: &'emit Contract<'emit>,
        package: &'emit str,
        namespace: &'emit Namespace,
        imports: &'emit RefCell<Imports>,
    ) -> Self {
        Self {
            contract,
            package,
            namespace,
            hidden_builtins: BTreeSet::new(),
            imports,
        }
    }

    /// Return this context inside a scope that also defines `names`: the
    /// module's top level, or the body of a class.
    #[must_use]
    pub(super) fn within<'name>(&self, names: impl IntoIterator<Item = &'name str>) -> Self {
        let mut hidden_builtins = self.hidden_builtins.clone();
        for name in names {
            if let Some(builtin) = BUILTINS.iter().copied().find(|builtin| *builtin == name) {
                hidden_builtins.insert(builtin);
            }
        }
        Self {
            hidden_builtins,
            ..*self
        }
    }

    /// Return this context for documentation, whose prose names every
    /// builtin plainly.
    #[must_use]
    pub(super) fn prose(&self) -> Self {
        Self {
            hidden_builtins: BTreeSet::new(),
            ..*self
        }
    }

    /// Spell the builtin `name`, through `builtins` when the scope hides it.
    #[must_use]
    pub(super) fn builtin(&self, name: &'static str) -> String {
        debug_assert!(
            BUILTINS.contains(&name),
            "`{name}` must be listed in BUILTINS, so that a name that hides it is detected"
        );
        if self.hidden_builtins.contains(name) {
            self.import("builtins");
            format!("builtins.{name}")
        } else {
            name.to_owned()
        }
    }

    /// Spell `path`, a module from outside the package or a member of one,
    /// such as `typing.Literal`, and import the module.
    pub(super) fn import(&self, path: &'static str) -> &'static str {
        let module = STANDARD_MODULES
            .iter()
            .chain(&PACKAGE_MODULES)
            .copied()
            .filter(|module| {
                path.strip_prefix(module)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
            })
            .max_by_key(|module| module.len());
        debug_assert!(module.is_some(), "`{path}` must be in a listed module");
        if let Some(module) = module {
            self.imports.borrow_mut().external.insert(module);
        }
        path
    }

    /// Spell `name`, a model or buffer type of the module's own namespace.
    pub(super) fn model(&self, name: &str) -> String {
        self.imports.borrow_mut().models.insert(name.to_owned());
        name.to_owned()
    }

    /// Spell the module `leaf` of another namespace, which the module
    /// imports under an alias that keeps the import from binding the
    /// package's first segment, which an export of the same name would
    /// replace. The alias is `bimetal_`, the leaf, and the namespace's index.
    pub(super) fn namespace_module(&self, namespace: &Namespace, leaf: &str) -> String {
        let alias = format!(
            "bimetal_{leaf}_{}",
            self.contract.namespace_index(namespace)
        );
        self.imports.borrow_mut().namespaces.insert(format!(
            "import {}.{leaf} as {alias}",
            render::python_namespace_module(self.package, namespace)
        ));
        alias
    }

    /// Spell `name`, a member of the runtime, and import the runtime.
    pub(super) fn runtime(&self, name: &str) -> String {
        self.imports.borrow_mut().runtime = true;
        format!("{RUNTIME_MODULE}.{name}")
    }

    /// Spell a constant of the module whose value is `expression`, which the
    /// module defines once, below its imports, so that a call does not
    /// evaluate it again. Its name is `prefix`, [`ADAPTER_CONSTANT`] or
    /// [`ARROW_CONSTANT`], and its index among the constants of that prefix.
    pub(super) fn constant(&self, prefix: &str, expression: String) -> String {
        let index = self
            .imports
            .borrow()
            .constants
            .iter()
            .filter(|(_, name)| {
                name.strip_prefix(prefix)
                    .is_some_and(|index| index.bytes().all(|byte| byte.is_ascii_digit()))
            })
            .count();
        self.named_constant(format!("{prefix}{index}"), expression)
    }

    /// Spell a constant of the module named `name`, or `name` and a number
    /// when another constant has that name, whose value is `expression`, as
    /// [`Self::constant`] does, or the constant that already holds it.
    pub(super) fn named_constant(&self, name: String, expression: String) -> String {
        let mut imports = self.imports.borrow_mut();
        if let Some((_, existing)) = imports
            .constants
            .iter()
            .find(|(existing, _)| *existing == expression)
        {
            return existing.clone();
        }
        let taken = |name: &str| imports.constants.iter().any(|(_, other)| other == name);
        let mut unique = name.clone();
        let mut number = 1;
        while taken(&unique) {
            number += 1;
            unique = format!("{name}_{number}");
        }
        imports.constants.push((expression, unique.clone()));
        unique
    }

    /// Return the module `leaf`, whose docstring is `docstring` and whose body
    /// is `body`, with the imports and the constants that rendering it
    /// recorded.
    #[must_use]
    pub(super) fn module(&self, leaf: &str, docstring: &str, body: &str) -> String {
        let imports = self.imports.borrow();
        let mut local: Vec<String> = imports.namespaces.iter().cloned().collect();
        if imports.runtime {
            local.push(format!("from {} import {RUNTIME_MODULE}", self.package));
        }
        let mut sibling = Vec::new();
        // `models.py` defines its own namespace's models.
        if leaf != MODELS_MODULE && !imports.models.is_empty() {
            sibling.push(import_list(".models", &imports.models));
        }
        let mut source =
            format!("\"\"\"\n{docstring}\n\"\"\"\n\nfrom __future__ import annotations\n");
        let constants = imports
            .constants
            .iter()
            .map(|(expression, name)| format!("{name} = {expression}"))
            .collect();
        for block in [
            statements(&STANDARD_MODULES, &imports.external),
            statements(&PACKAGE_MODULES, &imports.external),
            local,
            sibling,
            constants,
        ] {
            if !block.is_empty() {
                let _ = write!(source, "\n{}\n", block.join("\n"));
            }
        }
        source.push_str(body);
        source
    }
}

/// Render the statements that import the `used` modules of `table`, in its
/// order.
#[must_use]
fn statements(table: &[&'static str], used: &BTreeSet<&str>) -> Vec<String> {
    table
        .iter()
        .filter(|module| used.contains(*module))
        .map(|module| format!("import {module}"))
        .collect()
}

/// Sort `names` as ruff does: constants, then classes, then the rest, each
/// group in order, ignoring case when `fold_case` holds, as isort does in an
/// import and not in `__all__`.
#[must_use]
pub(super) fn ordered<'name>(
    names: impl IntoIterator<Item = &'name String>,
    fold_case: bool,
) -> Vec<&'name str> {
    let mut names: Vec<&str> = names.into_iter().map(String::as_str).collect();
    names.sort_by_key(|name| {
        let constant = name.len() > 1
            && name.chars().any(char::is_alphabetic)
            && !name.chars().any(char::is_lowercase);
        let class = name.starts_with(|character: char| character.is_uppercase());
        let folded = if fold_case {
            name.to_lowercase()
        } else {
            String::new()
        };
        (!constant, !class, folded, *name)
    });
    names.dedup();
    names
}

/// Render an import of several names from `module`, on one line when it fits
/// and otherwise one name per line, in isort's order.
#[must_use]
pub(super) fn import_list<'name>(
    module: &str,
    names: impl IntoIterator<Item = &'name String>,
) -> String {
    let names = ordered(names, /* fold_case */ true);
    let line = format!("from {module} import {}", names.join(", "));
    if line.len() <= LINE_WIDTH {
        return line;
    }
    let mut statement = format!("from {module} import (\n");
    for name in names {
        let _ = writeln!(statement, "    {name},");
    }
    statement.push(')');
    statement
}
