//! Validation of a discovered contract for every enabled host.

use std::collections::BTreeMap;
use std::{fmt, iter};

use anyhow::{Result, bail};
use bimetal_ir::Namespace;

use crate::contract::{self, Contract, Item, NamespaceItems};
use crate::namespaces::NamespaceRule;
use crate::problems::{self, Subject};
use crate::project::Project;

use super::{python, resources, types, typescript};

/// The generated packages of the enabled hosts, which validation checks
/// names for, and which its messages name.
pub(super) struct Packages<'project> {
    /// The Python import package, such as `my_app`.
    pub(super) python: Option<&'project str>,
    /// The npm package, such as `my-app`.
    pub(super) typescript: Option<&'project str>,
}

impl Packages<'_> {
    /// Spell the Python module of `namespace`, such as `my_app.stats`.
    #[must_use]
    pub(super) fn python_module(&self, namespace: &Namespace) -> String {
        let package = self.python.expect("only a Python package has Python names");
        crate::python::python_namespace_module(package, namespace)
    }

    /// Spell the npm module of `namespace`, such as `my-app/stats`.
    #[must_use]
    pub(super) fn typescript_module(&self, namespace: &Namespace) -> String {
        let package = self
            .typescript
            .expect("only an npm package has TypeScript names");
        iter::once(package.to_owned())
            .chain(namespace.typescript_segments())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// One exported name of a namespace, with the Rust item it comes from.
pub(super) struct Export {
    /// The name in the generated Python module.
    pub(super) python: String,
    /// The name in the generated TypeScript module, empty for a name that
    /// only Python has.
    pub(super) typescript: String,
    /// The Rust item, such as "function `app::stats::mean`".
    pub(super) item: String,
}

/// What a generated namespace comes from, as messages name it.
pub(super) enum Source<'manifest> {
    /// A folder, by its path relative to the workspace root, under
    /// `namespaces = "folders"`.
    Folder(String),
    /// The root module of a Cargo package, under `namespaces = "packages"`.
    Package(&'manifest str),
    /// A Rust module below its crate's root.
    Module {
        /// The module's path, such as `app::stats`.
        path: String,
        /// The Cargo package that declares it.
        package: &'manifest str,
    },
}

impl<'manifest> Source<'manifest> {
    /// Return what `namespace`, one of `contract`'s other than the root,
    /// comes from.
    #[must_use]
    pub(super) fn of(contract: &Contract<'manifest>, namespace: &Namespace) -> Self {
        if let NamespaceRule::Folders(folders) = contract.rule()
            && namespace.modules.is_empty()
        {
            return Self::Folder(folders.folder(&namespace.package));
        }
        let (owner, rust_module) = contract
            .origins()
            .find(|(owner, rust_module)| {
                let declared = contract.namespace(owner, rust_module);
                declared.package == namespace.package
                    && declared.modules.starts_with(&namespace.modules)
            })
            .expect("every namespace but the root holds a declaration or a module that does");
        if namespace.modules.is_empty() {
            return Self::Package(&owner.0);
        }
        let crate_name = rust_module.split("::").next().unwrap_or(rust_module);
        let segments: Vec<&str> = iter::once(crate_name)
            .chain(namespace.modules.iter().map(String::as_str))
            .collect();
        Self::Module {
            path: segments.join("::"),
            package: &owner.0,
        }
    }

    /// Return what to rename to move the namespace.
    #[must_use]
    pub(super) fn renamed(&self) -> &'static str {
        match self {
            Self::Folder(_) => "the folder",
            Self::Package(_) => "the package",
            Self::Module { .. } => "the module",
        }
    }
}

impl fmt::Display for Source<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Folder(path) => write!(formatter, "the folder `{path}`"),
            Self::Package(package) => write!(formatter, "Cargo package `{package}`"),
            Self::Module { path, package } => write!(
                formatter,
                "the Rust module `{path}` (Cargo package `{package}`)"
            ),
        }
    }
}

/// Validate that a discovered contract can be represented by the generators
/// of every enabled host.
///
/// The error lists every problem of the contract, one per line, so that the
/// caller can report them with the problems of the host projects.
pub(crate) fn validate_contract(project: &Project, contract: &Contract<'_>) -> Result<()> {
    let manifest = contract.manifest;
    if manifest.package_name != project.package_name
        || manifest.package_version != project.package_version
    {
        bail!("the discovered package does not match Cargo metadata");
    }
    let packages = Packages {
        python: project
            .python
            .as_ref()
            .map(|python| python.package.as_str()),
        typescript: project
            .typescript
            .as_ref()
            .map(|typescript| typescript.package.as_str()),
    };
    let mut problems = Vec::new();
    validate_namespaces(contract, &packages, &mut problems)?;
    if packages.python.is_some() {
        python::validate_model_namespace_cycles(contract, &packages, &mut problems)?;
    }
    problems::report(Subject::RustApi, &problems)
}

/// Validate namespace derivation and public-name uniqueness for every
/// enabled host.
fn validate_namespaces(
    contract: &Contract<'_>,
    packages: &Packages<'_>,
    problems: &mut Vec<String>,
) -> Result<()> {
    if packages.python.is_some() {
        python::validate_modules(contract, packages, problems);
        validate_module_paths(
            contract,
            "Python",
            Namespace::python_segments,
            |namespace| packages.python_module(namespace),
            problems,
        );
    }
    if packages.typescript.is_some() {
        validate_module_paths(
            contract,
            "TypeScript",
            Namespace::typescript_segments,
            |namespace| packages.typescript_module(namespace),
            problems,
        );
    }
    for (namespace, items) in contract.namespaces() {
        let callables = items.callables();
        types::validate_type_names(items, packages, problems);
        types::validate_nullable_options(items, &callables, contract, problems)?;
        types::validate_constant_values(items, contract, problems)?;
        resources::validate_protocols(items, problems);
        resources::validate_defaults(items, contract, problems)?;
        resources::validate_error_details(items, contract, packages, problems)?;
        let exports = namespace_exports(items);
        if packages.python.is_some() {
            python::validate_exports(namespace, &exports, contract, packages, problems);
        }
        if packages.typescript.is_some() {
            typescript::validate_exports(namespace, &exports, packages, problems);
        }
        if packages.python.is_some() {
            python::validate_resource_members(items, problems);
            python::validate_parameters(items, &callables, problems);
        }
        if packages.typescript.is_some() {
            typescript::validate_resource_members(items, problems);
            typescript::validate_parameters(&callables, problems);
            types::validate_typescript_streams(&callables, problems);
        }
    }
    Ok(())
}

/// Report namespaces that become the same module of the `host` package,
/// whose generated files would be the same ones. `segments` gives a
/// namespace's path in the package, and `module` spells its module.
fn validate_module_paths(
    contract: &Contract<'_>,
    host: &str,
    segments: fn(&Namespace) -> Vec<String>,
    module: impl Fn(&Namespace) -> String,
    problems: &mut Vec<String>,
) {
    let mut paths: BTreeMap<Vec<String>, &Namespace> = BTreeMap::new();
    for namespace in contract.namespaces().keys() {
        let Some(existing) = paths.insert(segments(namespace), namespace) else {
            continue;
        };
        let (existing, source) = (
            Source::of(contract, existing),
            Source::of(contract, namespace),
        );
        let rename = if existing.renamed() == source.renamed() {
            "one".to_owned()
        } else {
            format!("{} or {}", existing.renamed(), source.renamed())
        };
        problems.push(format!(
            "{existing} and {source} both become the {host} module `{}`; rename {rename}",
            module(namespace)
        ));
    }
}

/// Return every name that one namespace exports, with its Rust item, in
/// declaration order: the models and their variants, the API module's
/// names, and the buffer types that only Python models have.
fn namespace_exports(items: &NamespaceItems<'_>) -> Vec<Export> {
    let same = |name: &str, item: Item<'_>| Export {
        python: name.to_owned(),
        typescript: name.to_owned(),
        item: item.to_string(),
    };
    let mut exports = Vec::new();
    for model in &items.types {
        exports.push(same(&model.name, Item::Model(model)));
        for variant in contract::declared_variants(model) {
            exports.push(same(
                &contract::tagged_variant_name(&model.name, variant),
                Item::Variant { model, variant },
            ));
        }
    }
    for name in items.api_names() {
        exports.push(Export {
            python: name.python,
            typescript: name.typescript,
            item: name.item.to_string(),
        });
    }
    for element in crate::python::namespace_buffers(items) {
        let name = crate::python::python_buffer_alias(element);
        exports.push(Export {
            python: name.to_owned(),
            typescript: String::new(),
            item: format!("the generated buffer type `{name}`"),
        });
    }
    exports
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use bimetal_ir::{
        ArrowKind, CallableDef, CargoPackageId, ConstantDef, EnumVariantDef, ErrorCode,
        ExternalVariantDef, FieldDef, FunctionDef, Manifest, MethodDef, ParamDef, TypeRef,
        TypeShape, VariantPayload,
    };
    use serde_json::Value;

    use super::*;
    use crate::cargo::Package as CargoPackage;
    use crate::fixtures;
    use crate::project::packages;

    /// Return every problem that namespace validation reports for these
    /// packages.
    fn problems_for(
        manifest: &Manifest,
        python: Option<&str>,
        typescript: Option<&str>,
    ) -> Vec<String> {
        let packages = Packages { python, typescript };
        let mut problems = Vec::new();
        validate_namespaces(&fixtures::contract(manifest), &packages, &mut problems)
            .expect("the contract resolves");
        problems
    }

    /// Return every problem that namespace validation reports for a Python
    /// and an npm package.
    fn problems(manifest: &Manifest) -> Vec<String> {
        problems_for(manifest, Some("test_app"), Some("test-app"))
    }

    /// Return the library `name`, whose folder is `folder` in the workspace
    /// `/work`, as Cargo reports it.
    fn cargo_package(name: &str, folder: &str) -> CargoPackage {
        CargoPackage {
            id: name.to_owned(),
            name: name.to_owned(),
            version: "0.0.0".to_owned(),
            manifest_path: Path::new("/work").join(folder).join("Cargo.toml"),
            source: None,
            targets: Vec::new(),
        }
    }

    /// Index `manifest` as `namespaces = "folders"` names it: after the
    /// folders in `/work` of the application `test`, in `app`, and of the
    /// packages of `layout`, by name and folder.
    fn folders_contract<'manifest>(
        manifest: &'manifest Manifest,
        layout: &[(&str, &str)],
    ) -> Contract<'manifest> {
        let packages: Vec<CargoPackage> = iter::once(("test", "app"))
            .chain(layout.iter().copied())
            .map(|(name, folder)| cargo_package(name, folder))
            .collect();
        let selected: Vec<&CargoPackage> = packages.iter().collect();
        let folders = packages::folders(Path::new("/work"), &selected).unwrap();
        Contract::new(
            manifest,
            NamespaceRule::new(manifest, Some(&folders)).unwrap(),
        )
    }

    /// Return every problem that namespace validation reports for these
    /// packages, when the folders of `layout` name the namespaces, as
    /// [`folders_contract`] does.
    fn folder_problems_for(
        manifest: &Manifest,
        layout: &[(&str, &str)],
        python: Option<&str>,
        typescript: Option<&str>,
    ) -> Vec<String> {
        let packages = Packages { python, typescript };
        let mut problems = Vec::new();
        validate_namespaces(
            &folders_contract(manifest, layout),
            &packages,
            &mut problems,
        )
        .expect("the contract resolves");
        problems
    }

    /// Return every problem that namespace validation reports for a Python
    /// and an npm package, when the folders of `layout` name the namespaces.
    fn folder_problems(manifest: &Manifest, layout: &[(&str, &str)]) -> Vec<String> {
        folder_problems_for(manifest, layout, Some("test_app"), Some("test-app"))
    }

    fn validation_error(manifest: &Manifest) -> String {
        let problems = problems(manifest);
        assert_eq!(problems.len(), 1, "{problems:?}");
        problems[0].clone()
    }

    /// Return a contract with one `Recorder` resource that has these members.
    fn resource_manifest(factories: Vec<CallableDef>, methods: Vec<MethodDef>) -> Manifest {
        let mut manifest = fixtures::manifest();
        manifest
            .resources
            .push(fixtures::recorder(factories, methods));
        manifest
    }

    #[test]
    fn each_host_checks_only_its_own_names() {
        let mut manifest = fixtures::manifest();
        manifest
            .functions
            .push(fixtures::function("lambda", "lambda", Vec::new()));
        manifest.types.push(fixtures::model(
            "Node",
            vec![fixtures::field("parent", "__proto__")],
        ));
        let python = "function `test::lambda` cannot be exported to Python, because `lambda` is a Python keyword; rename it";
        let typescript = "field `parent` of `Node` crosses as `__proto__`, which would set the prototype of a JavaScript object; rename it";

        let both = problems(&manifest);
        assert!(both.iter().any(|problem| problem == python), "{both:?}");
        assert!(both.iter().any(|problem| problem == typescript), "{both:?}");
        assert_eq!(problems_for(&manifest, Some("test_app"), None), [python]);
        assert_eq!(
            problems_for(&manifest, None, Some("test-app")),
            [typescript]
        );
        assert!(problems_for(&manifest, None, None).is_empty());
    }

    #[test]
    fn typescript_takes_a_record_batch_stream_only_in_an_async_call() {
        let stream = || TypeRef::Arrow {
            value: ArrowKind::Stream,
        };
        let mut manifest = fixtures::manifest();
        manifest.functions.push(fixtures::function(
            "write",
            "write",
            vec![fixtures::typed_param("rows", stream())],
        ));
        let mut write_later = fixtures::function(
            "write_later",
            "writeLater",
            vec![fixtures::typed_param("rows", stream())],
        );
        write_later.callable.is_async = true;
        manifest.functions.push(write_later);
        manifest.functions.push(FunctionDef {
            returns: TypeRef::Option {
                item: Box::new(stream()),
            },
            ..fixtures::function("scan", "scan", Vec::new())
        });
        manifest
            .types
            .push(fixtures::model("RecordBatchSource", Vec::new()));

        assert!(problems_for(&manifest, Some("test_app"), None).is_empty());
        assert_eq!(
            problems_for(&manifest, None, Some("test-app")),
            [
                "model `test::RecordBatchSource` becomes the TypeScript name `RecordBatchSource` in `test-app`, which the generated module defines for Arrow streams; rename it",
                "parameter `rows` of function `test::write` is a `RecordBatchStream`, which JavaScript writes asynchronously, so TypeScript needs the call to be async; make it async, or remove `[typescript]` from bimetal.toml",
            ]
        );
    }

    #[test]
    fn a_folder_that_cannot_be_a_python_module_is_named_once() {
        let mut manifest = fixtures::manifest();
        manifest.functions.extend([
            fixtures::declared("xf-quotes", "xf_quotes", "quote"),
            fixtures::declared("xf-quotes", "xf_quotes::live", "stream"),
            fixtures::declared("xf-odds", "xf_odds", "odds"),
        ]);
        let layout = [
            ("xf-quotes", "sources/2024-feeds/quotes"),
            ("xf-odds", "sources/2024-feeds/odds"),
        ];

        assert_eq!(
            folder_problems(&manifest, &layout),
            [
                "the folder `sources/2024-feeds` becomes the Python module `test_app.sources.2024_feeds`, but `2024_feeds` is not a valid Python name; rename the folder"
            ]
        );
        // TypeScript takes any folder name.
        assert!(folder_problems_for(&manifest, &layout, None, Some("test-app")).is_empty());
    }

    #[test]
    fn a_module_and_a_folder_cannot_become_one_module() {
        let mut manifest = fixtures::manifest();
        manifest.functions.extend([
            fixtures::declared("xf-sources", "xf_sources::venues", "venue"),
            fixtures::declared("xf-polymarket", "xf_polymarket", "market"),
        ]);
        let layout = [
            ("xf-sources", "sources"),
            ("xf-polymarket", "sources/venues/polymarket"),
        ];
        let python = "the Rust module `xf_sources::venues` (Cargo package `xf-sources`) and the folder `sources/venues` both become the Python module `test_app.sources.venues`; rename the module or the folder";
        let typescript = "the Rust module `xf_sources::venues` (Cargo package `xf-sources`) and the folder `sources/venues` both become the TypeScript module `test-app/sources/venues`; rename the module or the folder";

        assert_eq!(folder_problems(&manifest, &layout), [python, typescript]);
        // Without Python, the two would still write the same files.
        assert_eq!(
            folder_problems_for(&manifest, &layout, None, Some("test-app")),
            [typescript]
        );
    }

    #[test]
    fn the_application_module_and_a_folder_cannot_become_one_module() {
        let mut manifest = fixtures::manifest();
        manifest.functions.extend([
            fixtures::declared("test", "test::core", "price"),
            fixtures::declared("xf-core", "xf_core", "quantity"),
        ]);

        assert_eq!(
            folder_problems_for(&manifest, &[("xf-core", "core")], Some("test_app"), None),
            [
                "the Rust module `test::core` (Cargo package `test`) and the folder `core` both become the Python module `test_app.core`; rename the module or the folder"
            ]
        );
    }

    #[test]
    fn a_module_and_a_package_cannot_become_one_module() {
        let mut manifest = fixtures::manifest();
        manifest.functions.extend([
            fixtures::declared("test", "test::geometry", "area"),
            fixtures::declared("test-geometry", "test_geometry", "perimeter"),
        ]);

        assert_eq!(
            problems(&manifest),
            [
                "the Rust module `test::geometry` (Cargo package `test`) and Cargo package `test-geometry` both become the Python module `test_app.geometry`; rename the module or the package",
                "the Rust module `test::geometry` (Cargo package `test`) and Cargo package `test-geometry` both become the TypeScript module `test-app/geometry`; rename the module or the package",
            ]
        );
    }

    #[test]
    fn folders_and_modules_named_like_generated_modules_stop_only_python() {
        let mut manifest = fixtures::manifest();
        manifest.functions.extend([
            fixtures::function("sources", "sources", Vec::new()),
            fixtures::declared("xf-odds", "xf_odds", "odds"),
            fixtures::declared("xf-quotes", "xf_quotes::api", "quote"),
        ]);
        let layout = [("xf-odds", "sources/models/odds"), ("xf-quotes", "quotes")];

        assert_eq!(
            folder_problems(&manifest, &layout),
            [
                "function `test::sources` and the folder `sources` both become `test_app.sources`; rename one",
                "the Rust module `xf_quotes::api` (Cargo package `xf-quotes`) becomes the Python module `test_app.quotes.api`, which Bimetal generates itself; rename the module",
                "the folder `sources/models` becomes the Python module `test_app.sources.models`, which Bimetal generates itself; rename the folder",
            ]
        );
        // The npm package keeps `models.js` beside the folder `models/`.
        assert!(folder_problems_for(&manifest, &layout, None, Some("test-app")).is_empty());
        let stage = tempfile::tempdir().unwrap();
        let native = crate::typescript::Native::Node {
            contract_hash: "hash",
        };
        let contract = folders_contract(&manifest, &layout);
        crate::typescript::emit("test-app", &contract, &native, stage.path()).unwrap();
        for file in [
            "sources/models.js",
            "sources/models/index.js",
            "sources/models/odds/api.js",
            "quotes/api.js",
            "quotes/api/api.js",
        ] {
            let path = stage.path().join("test-app").join(file);
            assert!(path.is_file(), "{} is missing", path.display());
        }
    }

    #[test]
    fn folders_name_nested_namespaces_from_cargo_metadata_to_the_generated_files() {
        let markets = "xf_polymarket::markets";
        let feed = fixtures::declared_model("xf-feeds", "xf_feeds", "Feed");
        let other_feed = fixtures::declared_model("xf-data-feeds", "xf_data_feeds", "Feed");
        let market = fixtures::declared_model("xf-polymarket", markets, "Market");
        let mut quote = fixtures::declared("xf-polymarket", markets, "quote");
        quote.callable.params = vec![fixtures::typed_param(
            "feed",
            TypeRef::Named {
                identity: feed.identity(),
            },
        )];
        quote.returns = TypeRef::Named {
            identity: other_feed.identity(),
        };
        let mut open = fixtures::function("open", "open", Vec::new());
        open.returns = TypeRef::Named {
            identity: market.identity(),
        };
        let mut manifest = fixtures::manifest();
        manifest.types.extend([feed, other_feed, market]);
        manifest.functions.extend([quote, open]);
        // `data/feeds` and `data-feeds` are distinct folders, whose
        // TypeScript aliases would otherwise be the same.
        let layout = [
            ("xf-polymarket", "sources/venues/polymarket"),
            ("xf-feeds", "sources/data/feeds"),
            ("xf-data-feeds", "sources/data-feeds"),
        ];
        let contract = folders_contract(&manifest, &layout);
        let packages = Packages {
            python: Some("test_app"),
            typescript: Some("test-app"),
        };
        let mut problems = Vec::new();
        validate_namespaces(&contract, &packages, &mut problems).expect("the contract resolves");
        assert!(problems.is_empty(), "{problems:?}");

        let stage = tempfile::tempdir().unwrap();
        crate::python::emit("test_app", &contract, "hash", stage.path()).unwrap();
        let native = crate::typescript::Native::Node {
            contract_hash: "hash",
        };
        crate::typescript::emit("test-app", &contract, &native, stage.path()).unwrap();
        let read = |path: &str| fs::read_to_string(stage.path().join(path)).unwrap();

        let python_markets = read("test_app/sources/venues/polymarket/markets/api.py");
        for import in [
            "import test_app.sources.data.feeds.models as _bimetal_models_3",
            "import test_app.sources.data_feeds.models as _bimetal_models_4",
        ] {
            assert!(python_markets.contains(import), "{python_markets}");
        }
        assert!(read("test_app/api.py").contains(
            "import test_app.sources.venues.polymarket.markets.models as _bimetal_models_7"
        ));
        // A folder that holds only other namespaces exports nothing.
        assert!(read("test_app/sources/__init__.py").ends_with("\n__all__: list[str] = []\n"));

        let typescript_markets = read("test-app/sources/venues/polymarket/markets/api.d.ts");
        for import in [
            "import type * as types_sources_data_feeds$3 from \"../../../data/feeds/models.js\";",
            "import type * as types_sources_data_feeds$4 from \"../../../data-feeds/models.js\";",
        ] {
            assert!(typescript_markets.contains(import), "{typescript_markets}");
        }
        assert!(read("test-app/api.d.ts").contains(
            "import type * as types_sources_venues_polymarket_markets from \"./sources/venues/polymarket/markets/models.js\";"
        ));
        assert!(read("test-app/sources/index.js").contains("export * from \"./api.js\";"));
    }

    #[test]
    fn resource_methods_cannot_shadow_generated_members() {
        for (rust_name, host_name, message) in [
            (
                "constructor",
                "constructor",
                "Rust method `constructor` of resource `Recorder` becomes the TypeScript member `constructor`, which the generated class defines; rename the method",
            ),
            (
                "native_resource",
                "nativeResource",
                "Rust method `native_resource` of resource `Recorder` collides with the member `native_resource` of the generated Python class; rename the method",
            ),
            (
                "nativeResource",
                "nativeResource",
                "Rust method `nativeResource` of resource `Recorder` becomes the TypeScript member `nativeResource`, which the generated class defines; rename the method",
            ),
            (
                "__enter__",
                "enter",
                "Rust method `__enter__` of resource `Recorder` has a Python special method name, which the generated Python class reserves for its own protocols; rename the method",
            ),
            (
                "__secret",
                "secret",
                "Rust method `__secret` of resource `Recorder` starts with `__`, which Python mangles inside a class; rename the method",
            ),
            (
                "from",
                "from",
                "Rust method `from` of resource `Recorder` is a Python keyword, so the generated Python class cannot define it; rename the method",
            ),
            (
                "then",
                "then",
                "Rust method `then` of resource `Recorder` becomes the TypeScript method `then`, which makes every instance a thenable that `await` calls; rename the method",
            ),
        ] {
            let manifest =
                resource_manifest(Vec::new(), vec![fixtures::method(rust_name, host_name)]);
            let problems = problems(&manifest);
            assert!(
                problems.iter().any(|problem| problem == message),
                "{problems:?}"
            );
        }

        let manifest = resource_manifest(
            Vec::new(),
            vec![
                fixtures::method("get_name", "getName"),
                fixtures::method("getName", "getName"),
            ],
        );
        assert_eq!(
            validation_error(&manifest),
            "methods of resource `Recorder` collide at the TypeScript name `getName`; rename one"
        );

        let manifest = resource_manifest(
            vec![fixtures::callable("prototype", "prototype", Vec::new())],
            Vec::new(),
        );
        assert_eq!(
            validation_error(&manifest),
            "Rust constructor `prototype` of resource `Recorder` becomes the TypeScript static method `prototype`, which a class cannot define; rename the constructor"
        );

        // Python keeps factories and methods in one class namespace.
        let manifest = resource_manifest(
            vec![fixtures::callable("delete", "delete", Vec::new())],
            vec![fixtures::method("delete", "delete")],
        );
        assert_eq!(
            validation_error(&manifest),
            "methods and constructors of resource `Recorder` collide at the Python name `delete`; rename one"
        );

        // JavaScript reserved words are fine as member names, and TypeScript
        // keeps static factories apart from instance methods.
        let manifest = resource_manifest(
            vec![
                fixtures::callable("default", "default", Vec::new()),
                fixtures::callable("from_name", "fromName", Vec::new()),
            ],
            vec![
                fixtures::method("delete", "delete"),
                fixtures::method("fromName", "fromName"),
            ],
        );
        assert!(problems(&manifest).is_empty());
    }

    #[test]
    fn parameters_keep_distinct_names_in_both_hosts() {
        let load = |params: Vec<ParamDef>| {
            let mut manifest = fixtures::manifest();
            manifest
                .functions
                .push(fixtures::function("load", "load", params));
            manifest
        };
        assert_eq!(
            validation_error(&load(vec![
                fixtures::param("package", "package"),
                fixtures::param("package_", "package_"),
            ])),
            "parameters of function `test::load` collide at the TypeScript name `package_`; rename one"
        );
        assert_eq!(
            validation_error(&load(vec![
                fixtures::param("class", "class"),
                fixtures::param("class_value", "classValue"),
            ])),
            "parameters of function `test::load` collide at the Python name `class_value`; rename one"
        );

        // The wrapper passes models and errors of its module to Pydantic.
        let mut manifest = load(vec![fixtures::param("Point", "point")]);
        manifest.types.push(fixtures::model("Point", Vec::new()));
        assert_eq!(
            validation_error(&manifest),
            "parameter `Point` of function `test::load` hides the Python model or error `Point`, which the generated wrapper uses; rename the parameter"
        );

        let manifest = load(
            ["package", "default", "value", "bytes", "test"]
                .into_iter()
                .map(|name| fixtures::param(name, name))
                .collect(),
        );
        assert!(problems(&manifest).is_empty());
    }

    #[test]
    fn generated_root_and_import_names_are_reserved() {
        for (rust_name, host_name, message) in [
            (
                "PanicException",
                "panicException",
                "function `test::PanicException` becomes `test_app.PanicException`, which the generated module defines itself; rename it",
            ),
            (
                "Field",
                "Field",
                "function `test::Field` becomes `test_app.Field`, which the generated module defines itself; rename it",
            ),
            (
                "api_root",
                "api_root",
                "function `test::api_root` becomes the TypeScript name `api_root` in `test-app`, which the generated module uses for its imports; rename it",
            ),
        ] {
            let mut manifest = fixtures::manifest();
            manifest
                .functions
                .push(fixtures::function(rust_name, host_name, Vec::new()));
            assert_eq!(validation_error(&manifest), message);
        }
    }

    #[test]
    fn typescript_names_that_break_a_module_are_rejected() {
        let mut manifest = fixtures::manifest();
        for name in ["eval", "arguments", "then"] {
            manifest
                .functions
                .push(fixtures::function(name, name, Vec::new()));
        }
        manifest
            .types
            .push(fixtures::model("JsonValue", Vec::new()));
        let problems = problems(&manifest);

        assert_eq!(problems.len(), 4, "{problems:?}");
        for expected in [
            "function `test::eval` cannot be exported to TypeScript, because `eval` is a reserved word there; rename it",
            "function `test::arguments` cannot be exported to TypeScript, because `arguments` is a reserved word there; rename it",
            "function `test::then` becomes the export `then` of `test-app`, which makes the module a thenable that `await import(...)` calls; rename it",
            "model `test::JsonValue` becomes the TypeScript name `JsonValue` in `test-app`, which the generated module defines for JSON values; rename it",
        ] {
            assert!(
                problems.iter().any(|problem| problem == expected),
                "{problems:?}"
            );
        }
    }

    #[test]
    fn exports_may_take_the_names_of_builtins() {
        let mut manifest = fixtures::manifest();
        for name in [
            "list",
            "str",
            "getattr",
            "object",
            "classmethod",
            "native",
            "type_adapter",
        ] {
            manifest
                .functions
                .push(fixtures::function(name, name, Vec::new()));
        }
        manifest.types.push(fixtures::model(
            "Listing",
            ["list", "dict", "json", "model_name"]
                .into_iter()
                .map(|name| fixtures::field(name, name))
                .collect(),
        ));

        assert!(problems(&manifest).is_empty(), "{:?}", problems(&manifest));
    }

    #[test]
    fn every_problem_is_reported_with_its_rust_items() {
        let mut manifest = fixtures::manifest();
        for (rust_name, host_name) in [
            ("PanicException", "panicException"),
            ("native_ready", "nativeReady"),
            ("add_later", "addLater"),
            ("addLater", "addLater"),
        ] {
            manifest
                .functions
                .push(fixtures::function(rust_name, host_name, Vec::new()));
        }
        manifest.errors.push(fixtures::error("Clash"));
        manifest.types.push(fixtures::model("Clash", Vec::new()));

        assert_eq!(
            problems(&manifest),
            [
                "model `test::Clash` and error `test::Clash` both become `test_app.Clash`; rename one",
                "function `test::PanicException` becomes `test_app.PanicException`, which the generated module defines itself; rename it",
                "model `test::Clash` and error `test::Clash` both become the TypeScript export `Clash` of `test-app`; rename one",
                "function `test::native_ready` becomes the export `nativeReady` of `test-app`, which the package root defines to load the Rust code; rename it",
                "function `test::add_later` and function `test::addLater` both become the TypeScript export `addLater` of `test-app`; rename one",
            ]
        );
        let error = problems::error(Subject::RustApi, &problems(&manifest)).to_string();
        assert!(error.starts_with("the Rust API has 5 problems:\n  - model `test::Clash`"));
    }

    #[test]
    fn fields_cannot_cross_as_proto() {
        let mut manifest = fixtures::manifest();
        manifest.types.push(fixtures::model(
            "Node",
            vec![fixtures::field("parent", "__proto__")],
        ));

        assert_eq!(
            validation_error(&manifest),
            "field `parent` of `Node` crosses as `__proto__`, which would set the prototype of a JavaScript object; rename it"
        );
    }

    #[test]
    fn wire_names_must_be_unique_and_leave_the_tag_alone() {
        let mut manifest = fixtures::manifest();
        manifest.types.push(fixtures::model(
            "Duplicate",
            vec![
                fixtures::field("first", "value"),
                fixtures::field("second", "value"),
            ],
        ));
        let mut empty = fixtures::model("Empty", Vec::new());
        empty.shape = TypeShape::TaggedEnum {
            tag: "kind".to_owned(),
            variants: Vec::new(),
        };
        manifest.types.push(empty);
        let mut collision = fixtures::model("Collision", Vec::new());
        collision.shape = TypeShape::TaggedEnum {
            tag: "kind".to_owned(),
            variants: vec![EnumVariantDef {
                rust_name: "Value".to_owned(),
                wire_name: "value".to_owned(),
                docs: None,
                fields: vec![fixtures::field("kind", "kind")],
            }],
        };
        manifest.types.push(collision);

        assert_eq!(
            problems(&manifest),
            [
                "fields of `Duplicate` collide at the wire name `value`; rename one",
                "tagged enum `Empty` has no variants, so no value can cross; add a variant",
                "field `kind` of `Collision::Value` crosses as `kind`, which is the tag of `Collision`; rename the field or the tag",
                "members of `Collision::Value` collide at the Python name `kind`; rename one",
            ]
        );
    }

    #[test]
    fn external_variants_need_unique_names_and_keys_hosts_can_read() {
        let mut manifest = fixtures::manifest();
        let mut shape = fixtures::external_enum();
        let TypeShape::ExternalEnum { variants } = &mut shape.shape else {
            panic!("the fixture is an externally tagged enum");
        };
        variants[0].wire_name = "label".to_owned();
        variants[2].wire_name = "__proto__".to_owned();
        let optional = |name: &str| FieldDef {
            required: false,
            ..fixtures::field(name, name)
        };
        let fields = |rust_name: &str, fields: Vec<FieldDef>| ExternalVariantDef {
            rust_name: rust_name.to_owned(),
            wire_name: rust_name.to_lowercase(),
            docs: None,
            payload: VariantPayload::Fields { fields },
        };
        // `ShapeRing(ring="x")` reaches Pydantic as `{"ring": "x"}`, and
        // `ShapeDisc(disc="x")` as `{"disc": "x"}`.
        variants.push(fields(
            "Ring",
            vec![fixtures::field("ring", "ring"), optional("note")],
        ));
        variants.push(fields("Disc", vec![fixtures::field("disc", "r")]));
        manifest.types.push(shape);

        assert_eq!(
            problems(&manifest),
            [
                "variants of `Shape` collide at the wire name `label`; rename one",
                "`Shape::Circle` crosses under the key `__proto__`, which would set the prototype of a JavaScript object; rename the variant",
                "field `ring` of `Shape::Ring` has the name of its variant, `ring`, so Python cannot tell a model built by keyword from the variant's wire form; rename the field or the variant",
                "field `disc` of `Shape::Disc` has the name of its variant, `disc`, so Python cannot tell a model built by keyword from the variant's wire form; rename the field or the variant",
            ]
        );
    }

    #[test]
    fn an_optional_duration_and_its_nanoseconds_as_a_default_cross() {
        let mut manifest = fixtures::manifest();
        manifest.types.push(fixtures::model(
            "Wait",
            vec![FieldDef {
                required: false,
                ..fixtures::typed_field(
                    "idle",
                    TypeRef::Option {
                        item: Box::new(TypeRef::Duration),
                    },
                )
            }],
        ));
        let duration = |name: &str, default: Value| ParamDef {
            default: Some(default),
            ..fixtures::typed_param(name, TypeRef::Duration)
        };
        manifest.functions.push(fixtures::function(
            "wait",
            "wait",
            vec![
                duration("nanoseconds", Value::from(1_500_000_000_u64)),
                duration("words", Value::from("a second")),
            ],
        ));

        assert_eq!(
            problems(&manifest),
            [
                "the default of parameter `words` of function `test::wait` does not fit the parameter's type"
            ]
        );
    }

    #[test]
    fn typescript_reads_error_details_that_hold_no_duration() {
        let mut manifest = fixtures::manifest();
        let details = fixtures::model(
            "TimedOutDetails",
            vec![fixtures::typed_field("after", TypeRef::Duration)],
        );
        let mut error = fixtures::error("WaitError");
        error.codes.push(ErrorCode {
            code: "timed_out".to_owned(),
            docs: None,
            details: Some(details.identity()),
        });
        manifest.types.push(details);
        manifest.errors.push(error);

        assert_eq!(
            problems(&manifest),
            [
                "the details of `WaitError` code `timed_out` hold a `Duration`, which JSON carries to TypeScript as Serde's object, not as the `bigint` its declarations say; use a count of a unit"
            ]
        );
        assert!(problems_for(&manifest, Some("test_app"), None).is_empty());
    }

    #[test]
    fn python_names_cannot_be_the_wire_names_of_other_fields() {
        let mut manifest = fixtures::manifest();
        // Pydantic would read `Swapped(a=1)` as the field `b`.
        manifest.types.push(fixtures::model(
            "Swapped",
            vec![
                fixtures::field("a", "b"),
                fixtures::field("b", "a"),
                fixtures::field("_id", "_id"),
            ],
        ));

        assert_eq!(
            problems(&manifest),
            [
                "field `a` of `Swapped` has the Python name `a`, which field `b` crosses as, so Pydantic would read one for the other; rename one",
                "field `b` of `Swapped` has the Python name `b`, which field `a` crosses as, so Pydantic would read one for the other; rename one",
            ]
        );
    }

    #[test]
    fn options_must_tell_none_from_some() {
        let option = |item: TypeRef| TypeRef::Option {
            item: Box::new(item),
        };
        let mut manifest = fixtures::manifest();
        let mut nullable = fixtures::model("Nullable", Vec::new());
        nullable.shape = TypeShape::Alias {
            target: option(TypeRef::String),
        };
        manifest.types.push(nullable);
        let outer = fixtures::typed_field(
            "outer",
            TypeRef::List {
                item: Box::new(option(option(TypeRef::String))),
            },
        );
        let plain = fixtures::typed_field("plain", option(TypeRef::String));
        manifest
            .types
            .push(fixtures::model("Holder", vec![outer, plain]));
        let mut function = fixtures::function(
            "lookup",
            "lookup",
            vec![fixtures::typed_param(
                "key",
                option(fixtures::named("Nullable")),
            )],
        );
        function.returns = option(TypeRef::Unit);
        manifest.functions.push(function);

        assert_eq!(
            problems(&manifest),
            [
                "field `outer` of `Holder` holds an `Option` whose value can itself be null, such as `Option<Option<T>>` or `Option<()>`, so the hosts cannot tell `None` from `Some`; remove one `Option`",
                "parameter `key` of function `test::lookup` holds an `Option` whose value can itself be null, such as `Option<Option<T>>` or `Option<()>`, so the hosts cannot tell `None` from `Some`; remove one `Option`",
                "the return type of function `test::lookup` holds an `Option` whose value can itself be null, such as `Option<Option<T>>` or `Option<()>`, so the hosts cannot tell `None` from `Some`; remove one `Option`",
            ]
        );
    }

    #[test]
    fn constants_cannot_hold_nan_or_infinity() {
        let float = TypeRef::Float { bits: 64 };
        let mut manifest = fixtures::manifest();
        manifest.types.push(fixtures::model(
            "Point",
            vec![fixtures::typed_field("x", float.clone())],
        ));
        let constant = |host_name: &str, ty: TypeRef, value: Value| ConstantDef {
            owner: CargoPackageId::new("test"),
            rust_module: "test".to_owned(),
            host_name: host_name.to_owned(),
            docs: None,
            ty,
            value,
        };
        manifest.constants.extend([
            constant("RATIO", float.clone(), Value::Null),
            constant(
                "ORIGINS",
                TypeRef::List {
                    item: Box::new(fixtures::named("Point")),
                },
                serde_json::json!([{"x": 0.0}, {"x": null}]),
            ),
            constant(
                "LIMIT",
                TypeRef::Option {
                    item: Box::new(float),
                },
                Value::Null,
            ),
        ]);

        assert_eq!(
            problems(&manifest),
            [
                "constant `test::RATIO` holds a NaN or an infinity, which the generated packages cannot represent; use a finite value",
                "constant `test::ORIGINS` holds a NaN or an infinity, which the generated packages cannot represent; use a finite value",
            ]
        );
    }
}
