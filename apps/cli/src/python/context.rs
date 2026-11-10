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
use bimetal_ir::python_names::{BUILTINS, BUILTINS_ALIAS, DATETIME_ALIAS};

use crate::contract::Contract;

use super::package::{MODELS_MODULE, RUNTIME_MODULE};
use super::render;

/// The names that generated modules import from the standard library, and
/// then from other packages, in the order of a module's imports. Each starts
/// a statement: `from typing` imports the name, and `import numpy` imports
/// the module under it.
const STANDARD_IMPORTS: [(&str, &str); 9] = [
    ("import builtins", BUILTINS_ALIAS),
    ("import datetime", DATETIME_ALIAS),
    ("from collections.abc", "Callable"),
    ("from enum", "StrEnum"),
    ("from typing", "Annotated"),
    ("from typing", "Any"),
    ("from typing", "Final"),
    ("from typing", "Literal"),
    ("from typing", "TypeAlias"),
];
const PACKAGE_IMPORTS: [(&str, &str); 9] = [
    ("import numpy", "np"),
    ("from numpy.typing", "NDArray"),
    ("import pyarrow", "pa"),
    ("from pydantic", "AwareDatetime"),
    ("from pydantic", "BaseModel"),
    ("from pydantic", "ConfigDict"),
    ("from pydantic", "Field"),
    ("from pydantic.functional_serializers", "PlainSerializer"),
    ("from pydantic.functional_validators", "BeforeValidator"),
];

/// The prefix of the aliases under which generated modules import the
/// runtime's helpers, other namespaces' modules, and `builtins`. No export
/// may start with it, so that each module may define the helper's name.
pub(crate) const ALIAS_PREFIX: &str = "_bimetal_";

/// Return the names that generated modules may import from outside the
/// package. An export of the same name would replace the import.
pub(crate) fn imported_names() -> impl Iterator<Item = &'static str> {
    STANDARD_IMPORTS
        .iter()
        .chain(&PACKAGE_IMPORTS)
        .map(|(_, name)| *name)
}

/// What one generated module imports, as its renderers spell each name.
#[derive(Default)]
pub(super) struct Imports {
    /// Names from outside the package; see [`STANDARD_IMPORTS`].
    external: BTreeSet<&'static str>,
    /// The models and buffer types of the module's own namespace, which
    /// `api.py` imports from `models.py`.
    models: BTreeSet<String>,
    /// The other namespaces' modules, as import statements.
    namespaces: BTreeSet<String>,
    /// The runtime's names, as entries of the import from it.
    runtime: BTreeSet<String>,
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
            format!("{}.{name}", self.import(BUILTINS_ALIAS))
        } else {
            name.to_owned()
        }
    }

    /// Spell `name`, which the module imports from outside the package.
    pub(super) fn import(&self, name: &'static str) -> &'static str {
        debug_assert!(
            imported_names().any(|imported| imported == name),
            "`{name}` must be listed with the statement that imports it"
        );
        self.imports.borrow_mut().external.insert(name);
        name
    }

    /// Spell `name`, a model or buffer type of the module's own namespace.
    pub(super) fn model(&self, name: &str) -> String {
        self.imports.borrow_mut().models.insert(name.to_owned());
        name.to_owned()
    }

    /// Spell the module `leaf` of another namespace, which the module
    /// imports under an alias that keeps the import from binding the
    /// package's first segment, which an export of the same name would
    /// replace. The alias is one of [`python_names::MODULE_ALIAS_PREFIXES`],
    /// then the namespace's index.
    ///
    /// [`python_names::MODULE_ALIAS_PREFIXES`]: bimetal_ir::python_names::MODULE_ALIAS_PREFIXES
    pub(super) fn namespace_module(&self, namespace: &Namespace, leaf: &str) -> String {
        let alias = format!(
            "{ALIAS_PREFIX}{leaf}_{}",
            self.contract.namespace_index(namespace)
        );
        self.imports.borrow_mut().namespaces.insert(format!(
            "import {}.{leaf} as {alias}",
            render::python_namespace_module(self.package, namespace)
        ));
        alias
    }

    /// Spell the runtime's helper `name`, which modules import under an
    /// alias that starts with [`ALIAS_PREFIX`].
    pub(super) fn runtime(&self, name: &'static str) -> String {
        let alias = format!("{ALIAS_PREFIX}{name}");
        self.imports
            .borrow_mut()
            .runtime
            .insert(format!("{name} as {alias}"));
        alias
    }

    /// Spell `name`, a public name of the runtime, which the module imports
    /// as it is.
    pub(super) fn runtime_export(&self, name: &'static str) -> &'static str {
        self.imports.borrow_mut().runtime.insert(name.to_owned());
        name
    }

    /// Return the module `leaf`, whose body is `body`, with the imports that
    /// rendering it recorded.
    #[must_use]
    pub(super) fn module(&self, leaf: &str, body: &str) -> String {
        let imports = self.imports.borrow();
        let mut local = Vec::new();
        // `models.py` defines its own namespace's models.
        if leaf != MODELS_MODULE && !imports.models.is_empty() {
            local.push(import_list(".models", &imports.models));
        }
        local.extend(imports.namespaces.iter().cloned());
        if !imports.runtime.is_empty() {
            let runtime = format!("{}.{RUNTIME_MODULE}", self.package);
            local.push(import_list(&runtime, &imports.runtime));
        }
        let mut source = String::from("from __future__ import annotations\n");
        for block in [
            statements(&STANDARD_IMPORTS, &imports.external),
            statements(&PACKAGE_IMPORTS, &imports.external),
            local,
        ] {
            if !block.is_empty() {
                let _ = write!(source, "\n{}\n", block.join("\n"));
            }
        }
        source.push_str(body);
        source
    }
}

/// Render the statements of `table` that import the `used` names, one per
/// module.
#[must_use]
fn statements(table: &[(&str, &'static str)], used: &BTreeSet<&str>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut previous = "";
    for &(statement, name) in table.iter().filter(|(_, name)| used.contains(name)) {
        match lines.last_mut() {
            Some(line) if statement == previous => {
                let _ = write!(line, ", {name}");
            }
            _ if statement.starts_with("import ") => lines.push(format!("{statement} as {name}")),
            _ => lines.push(format!("{statement} import {name}")),
        }
        previous = statement;
    }
    lines
}

/// Render an import of several names from `module`, one per line.
#[must_use]
fn import_list(module: &str, names: &BTreeSet<String>) -> String {
    let mut statement = format!("from {module} import (\n");
    for name in names {
        let _ = writeln!(statement, "    {name},");
    }
    statement.push(')');
    statement
}
