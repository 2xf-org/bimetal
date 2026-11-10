//! The state that every renderer of one generated TypeScript module shares,
//! and the imports that they record.
//!
//! A module renders its JavaScript and declarations first. Every name that
//! they spell through the context is recorded, and
//! [`TypeScriptContext::javascript`] and [`TypeScriptContext::declarations`]
//! then write the imports of exactly those names above them.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::PathBuf;

use bimetal_ir::Namespace;

use crate::contract::Contract;
use crate::dependencies::APACHE_ARROW;
use crate::literal;
use crate::paths;

use super::package::{API_MODULE, ARROW_MODULE, MODELS_MODULE, RUNTIME_MODULE};

/// The prefixes of the aliases under which a module imports another
/// namespace's models and API, followed by the namespace's path; see
/// [`TypeScriptContext::alias`].
pub(crate) const MODELS_ALIAS_PREFIX: &str = "types_";
pub(crate) const API_ALIAS_PREFIX: &str = "api_";

/// The alias under which declarations import apache-arrow's types, which
/// keeps them apart from models named `Table` or `Schema`.
pub(crate) const ARROW_NAMESPACE: &str = "apache_arrow";

/// What one generated module imports, as its renderers spell each name.
#[derive(Default)]
struct Imports {
    /// The runtime's exports that the JavaScript calls.
    runtime: BTreeSet<&'static str>,
    /// The Arrow adapters that the JavaScript calls.
    arrow: BTreeSet<&'static str>,
    /// The other namespaces whose errors the JavaScript throws, and whose
    /// documentation the declarations link.
    errors: BTreeSet<Namespace>,
    /// The module's own models that the API's declarations name.
    models: BTreeSet<String>,
    /// The other namespaces whose models the declarations name.
    namespaces: BTreeSet<Namespace>,
    /// Whether the declarations name apache-arrow's types.
    apache_arrow: bool,
}

/// Shared contract and namespace state for one generated TypeScript module.
///
/// Everything it borrows outlives the `emit` call that renders the package.
pub(super) struct TypeScriptContext<'emit> {
    pub(super) contract: &'emit Contract<'emit>,
    /// The namespace of the module being rendered.
    pub(super) namespace: &'emit Namespace,
    /// What the module has imported so far.
    imports: RefCell<Imports>,
}

impl<'emit> TypeScriptContext<'emit> {
    /// Create the context of one module, which has imported nothing yet.
    #[must_use]
    pub(super) fn new(contract: &'emit Contract<'emit>, namespace: &'emit Namespace) -> Self {
        Self {
            contract,
            namespace,
            imports: RefCell::default(),
        }
    }

    /// Spell the runtime's export `name`. The bindings that a module
    /// introduces start with `$`, which no Rust name contains, so no export
    /// or parameter can shadow them.
    pub(super) fn runtime(&self, name: &'static str) -> String {
        self.imports.borrow_mut().runtime.insert(name);
        format!("${name}")
    }

    /// Spell the Arrow adapter `name`, as [`Self::runtime`] does.
    pub(super) fn arrow(&self, name: &'static str) -> String {
        self.imports.borrow_mut().arrow.insert(name);
        format!("${name}")
    }

    /// Spell `name`, a model of the module's own namespace.
    pub(super) fn model(&self, name: &str) -> String {
        self.imports.borrow_mut().models.insert(name.to_owned());
        name.to_owned()
    }

    /// Spell the models of another namespace.
    pub(super) fn models_namespace(&self, namespace: &Namespace) -> String {
        self.imports
            .borrow_mut()
            .namespaces
            .insert(namespace.clone());
        self.alias(MODELS_ALIAS_PREFIX, namespace)
    }

    /// Spell the API of another namespace, whose errors the module names.
    pub(super) fn errors_namespace(&self, namespace: &Namespace) -> String {
        self.imports.borrow_mut().errors.insert(namespace.clone());
        self.alias(API_ALIAS_PREFIX, namespace)
    }

    /// Spell the namespace of apache-arrow's types.
    pub(super) fn apache_arrow(&self) -> &'static str {
        self.imports.borrow_mut().apache_arrow = true;
        ARROW_NAMESPACE
    }

    /// Return the JavaScript `body` with the imports that rendering it
    /// recorded.
    #[must_use]
    pub(super) fn javascript(&self, body: &str) -> String {
        let imports = self.imports.borrow();
        let mut source = String::new();
        for (module, names) in [
            (RUNTIME_MODULE, &imports.runtime),
            (ARROW_MODULE, &imports.arrow),
        ] {
            if names.is_empty() {
                continue;
            }
            let bindings: Vec<String> = names
                .iter()
                .map(|name| format!("{name} as ${name}"))
                .collect();
            let _ = writeln!(
                source,
                "import {{ {} }} from {};",
                bindings.join(", "),
                self.path(&Namespace::root(), module)
            );
        }
        for namespace in &imports.errors {
            let _ = writeln!(
                source,
                "import * as {} from {};",
                self.alias(API_ALIAS_PREFIX, namespace),
                self.path(namespace, API_MODULE)
            );
        }
        module(source, body)
    }

    /// Return the declarations `body` of the module `leaf` with the imports
    /// that rendering it recorded.
    #[must_use]
    pub(super) fn declarations(&self, leaf: &str, body: &str) -> String {
        let imports = self.imports.borrow();
        let mut source = String::new();
        if imports.apache_arrow {
            let _ = writeln!(
                source,
                "import type * as {ARROW_NAMESPACE} from {};",
                literal::quoted(APACHE_ARROW.name)
            );
        }
        // `models` declares its own namespace's models.
        if leaf != MODELS_MODULE && !imports.models.is_empty() {
            let names: Vec<&str> = imports.models.iter().map(String::as_str).collect();
            let _ = writeln!(
                source,
                "import type {{ {} }} from {};",
                names.join(", "),
                self.path(self.namespace, MODELS_MODULE)
            );
        }
        for (prefix, leaf, namespaces) in [
            (MODELS_ALIAS_PREFIX, MODELS_MODULE, &imports.namespaces),
            (API_ALIAS_PREFIX, API_MODULE, &imports.errors),
        ] {
            for namespace in namespaces {
                let _ = writeln!(
                    source,
                    "import type * as {} from {};",
                    self.alias(prefix, namespace),
                    self.path(namespace, leaf)
                );
            }
        }
        module(source, body)
    }

    /// Return the quoted relative path from this module to the module `leaf`
    /// of `namespace`, such as `"../stats/api.js"`.
    #[must_use]
    fn path(&self, namespace: &Namespace, leaf: &str) -> String {
        let from: PathBuf = self.namespace.typescript_segments().into_iter().collect();
        let mut to: PathBuf = namespace.typescript_segments().into_iter().collect();
        to.push(format!("{leaf}.js"));
        let relative = paths::portable_relative(&from, &to)
            .expect("namespace segments are UTF-8 folder, package, and module names");
        if relative.starts_with("../") {
            literal::quoted(&relative)
        } else {
            literal::quoted(&format!("./{relative}"))
        }
    }

    /// Return the alias of a namespace's import: `prefix`, then its
    /// segments, which [`readable_alias`] joins. Namespaces whose segments
    /// join the same way, such as `data/feeds` and `data-feeds`, add `$`,
    /// which joined segments never hold, and the namespace's index.
    #[must_use]
    fn alias(&self, prefix: &str, namespace: &Namespace) -> String {
        let readable = readable_alias(namespace);
        let shared = self
            .contract
            .namespaces()
            .keys()
            .any(|other| other != namespace && readable_alias(other) == readable);
        if shared {
            format!(
                "{prefix}{readable}${}",
                self.contract.namespace_index(namespace)
            )
        } else {
            format!("{prefix}{readable}")
        }
    }
}

/// Join a module's imports and its body. A module stays a module when both
/// are empty, so that `index` can re-export it.
#[must_use]
fn module(mut imports: String, body: &str) -> String {
    imports.push_str(body);
    if imports.is_empty() {
        imports.push_str("export {};\n");
    }
    imports
}

/// Return a namespace's segments joined by `_`, with `_` for each character
/// that an identifier cannot hold, or `root` for the root.
#[must_use]
fn readable_alias(namespace: &Namespace) -> String {
    let segments = namespace.typescript_segments();
    if segments.is_empty() {
        return "root".to_owned();
    }
    segments
        .join("_")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use bimetal_ir::CargoPackageId;

    use super::*;
    use crate::fixtures;

    #[test]
    fn namespaces_whose_segments_join_alike_import_under_their_index() {
        let mut manifest = fixtures::manifest();
        manifest.functions.extend([
            fixtures::declared("test-data", "test_data::feeds", "first"),
            fixtures::declared("test-data-feeds", "test_data_feeds", "second"),
        ]);
        let contract = fixtures::contract(&manifest);
        let nested = contract.namespace(&CargoPackageId::new("test-data"), "test_data::feeds");
        let hyphenated =
            contract.namespace(&CargoPackageId::new("test-data-feeds"), "test_data_feeds");
        let data = contract.namespace(&CargoPackageId::new("test-data"), "test_data");
        let root = Namespace::root();
        let context = TypeScriptContext::new(&contract, &root);

        assert_eq!(context.models_namespace(&nested), "types_data_feeds$2");
        assert_eq!(context.models_namespace(&hyphenated), "types_data_feeds$3");
        assert_eq!(context.models_namespace(&data), "types_data");
        assert_eq!(context.errors_namespace(&nested), "api_data_feeds$2");
        assert_eq!(
            context.declarations(API_MODULE, ""),
            "import type * as types_data from \"./data/models.js\";\n\
             import type * as types_data_feeds$2 from \"./data/feeds/models.js\";\n\
             import type * as types_data_feeds$3 from \"./data-feeds/models.js\";\n\
             import type * as api_data_feeds$2 from \"./data/feeds/api.js\";\n"
        );
        assert_eq!(
            context.javascript(""),
            "import * as api_data_feeds$2 from \"./data/feeds/api.js\";\n"
        );
    }
}
