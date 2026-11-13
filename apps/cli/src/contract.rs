//! Read-only indexing and traversal helpers for the linked contract IR.
//!
//! Renderers consume declarations by generated namespace rather than by Rust
//! package. [`Contract`] builds that grouping and every definition lookup once,
//! so rendering cost stays linear in the number of references it emits.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::{fmt, iter};

use anyhow::{Context, Result};
use bimetal_ir::{
    ArrowKind, CallableDef, CargoPackageId, ConstantDef, DefinitionId, ErrorDef, FieldDef,
    FunctionDef, Manifest, Namespace, ParamDef, ResourceDef, TypeDef, TypeRef, TypeShape,
    VariantPayload,
};
use serde_json::{Value, json};

use crate::namespaces::NamespaceRule;

/// The stream of record batches that a contract type crosses as.
pub(crate) const STREAM: TypeRef = TypeRef::Arrow {
    value: ArrowKind::Stream,
};

/// Return whether `reference` holds a stream of record batches.
#[must_use]
pub(crate) fn holds_stream(reference: &TypeRef) -> bool {
    nodes(reference).any(|node| *node == STREAM)
}

/// Host spellings of one Arrow value.
pub(super) struct ArrowSpelling {
    /// The kind that the TypeScript Arrow adapters take.
    pub(super) key: &'static str,
    /// pyarrow class of a returned value.
    pub(super) pyarrow: &'static str,
    /// Path from the pyarrow module to the function that imports a returned
    /// value.
    pub(super) import: &'static [&'static str],
    /// Generated Python protocol that a parameter accepts.
    pub(super) protocol: &'static str,
    /// apache-arrow class, for parameters and returns alike.
    pub(super) typescript: &'static str,
}

/// Return every host spelling of an Arrow value.
#[must_use]
pub(super) fn arrow_spelling(kind: ArrowKind) -> ArrowSpelling {
    let (key, pyarrow, import, protocol, typescript) = match kind {
        ArrowKind::Array => (
            "array",
            "Array",
            &["array"][..],
            ARROW_ARRAY_PROTOCOL,
            "Vector",
        ),
        ArrowKind::RecordBatch => (
            "recordBatch",
            "RecordBatch",
            &["record_batch"][..],
            ARROW_ARRAY_PROTOCOL,
            "RecordBatch",
        ),
        ArrowKind::Table => (
            "table",
            "Table",
            &["table"][..],
            ARROW_STREAM_PROTOCOL,
            "Table",
        ),
        ArrowKind::Schema => (
            "schema",
            "Schema",
            &["schema"][..],
            ARROW_SCHEMA_PROTOCOL,
            "Schema",
        ),
        // TypeScript names a stream with the types that its models declare,
        // and its own adapters read it, so only the Python spellings apply.
        ArrowKind::Stream => (
            "stream",
            "RecordBatchReader",
            &["RecordBatchReader", "from_stream"][..],
            ARROW_STREAM_PROTOCOL,
            "RecordBatchReader",
        ),
    };
    ArrowSpelling {
        key,
        pyarrow,
        import,
        protocol,
        typescript,
    }
}

/// The Python protocols that Arrow parameters accept: any producer of the
/// Arrow PyCapsule interface.
pub(super) const ARROW_ARRAY_PROTOCOL: &str = "ArrowArrayExportable";
pub(super) const ARROW_STREAM_PROTOCOL: &str = "ArrowStreamExportable";
pub(super) const ARROW_SCHEMA_PROTOCOL: &str = "ArrowSchemaExportable";

/// Borrowed declarations grouped into one generated namespace.
#[derive(Default)]
pub(super) struct NamespaceItems<'manifest> {
    /// Model declarations in this namespace.
    pub(super) types: Vec<&'manifest TypeDef>,
    /// Typed-error declarations in this namespace.
    pub(super) errors: Vec<&'manifest ErrorDef>,
    /// Free functions in this namespace.
    pub(super) functions: Vec<&'manifest FunctionDef>,
    /// Stateful resources in this namespace.
    pub(super) resources: Vec<&'manifest ResourceDef>,
    /// Constants in this namespace.
    pub(super) constants: Vec<&'manifest ConstantDef>,
}

impl<'manifest> NamespaceItems<'manifest> {
    /// Return every name that the namespace's API module exports, in
    /// declaration order: each error and the type of its codes, then the
    /// resources, the functions, and the constants.
    #[must_use]
    pub(super) fn api_names(&self) -> Vec<ApiName<'manifest>> {
        let same = |name: &str, item: Item<'manifest>| ApiName {
            python: name.to_owned(),
            typescript: name.to_owned(),
            item,
        };
        let mut names = Vec::new();
        for error in &self.errors {
            names.push(same(&error.name, Item::Error(error)));
            names.push(same(&error_code_alias(error), Item::ErrorCodes(error)));
        }
        for resource in &self.resources {
            names.push(same(&resource.name, Item::Resource(resource)));
        }
        for function in &self.functions {
            names.push(ApiName {
                python: function.callable.rust_name.clone(),
                typescript: function.callable.host_name.clone(),
                item: Item::Function(function),
            });
        }
        for constant in &self.constants {
            names.push(same(&constant.host_name, Item::Constant(constant)));
        }
        names
    }

    /// Return every callable of the namespace, in the order of
    /// [`Manifest::callables`]: each function's, then each resource's
    /// constructors and methods.
    #[must_use]
    pub(super) fn callables(&self) -> Vec<NamespaceCallable<'manifest>> {
        let mut callables: Vec<NamespaceCallable<'manifest>> = self
            .functions
            .iter()
            .map(|function| NamespaceCallable {
                item: Item::Function(function),
                callable: &function.callable,
                returns: Some(&function.returns),
            })
            .collect();
        for resource in &self.resources {
            for constructor in resource.constructors() {
                callables.push(NamespaceCallable {
                    item: Item::Constructor {
                        resource,
                        constructor,
                    },
                    callable: constructor,
                    returns: None,
                });
            }
            for method in &resource.methods {
                callables.push(NamespaceCallable {
                    item: Item::Method {
                        resource,
                        method: &method.callable,
                    },
                    callable: &method.callable,
                    returns: Some(&method.returns),
                });
            }
        }
        callables
    }
}

/// A function, a constructor, or a method of one namespace.
pub(super) struct NamespaceCallable<'manifest> {
    /// The Rust item that it is.
    pub(super) item: Item<'manifest>,
    pub(super) callable: &'manifest CallableDef,
    /// What it returns, which for a constructor is its resource.
    pub(super) returns: Option<&'manifest TypeRef>,
}

/// One name that a namespace's API module exports.
pub(super) struct ApiName<'manifest> {
    /// The name in Python, where functions keep their Rust names.
    pub(super) python: String,
    /// The name in TypeScript.
    pub(super) typescript: String,
    /// The Rust item that it comes from.
    pub(super) item: Item<'manifest>,
}

/// A Rust item, which diagnostics describe by its kind and path, such as
/// "function `app::stats::mean`".
#[derive(Clone, Copy)]
pub(crate) enum Item<'manifest> {
    Model(&'manifest TypeDef),
    /// A variant of a tagged or string enum.
    Variant {
        model: &'manifest TypeDef,
        variant: &'manifest str,
    },
    Error(&'manifest ErrorDef),
    /// The generated type that lists an error's codes.
    ErrorCodes(&'manifest ErrorDef),
    Function(&'manifest FunctionDef),
    Resource(&'manifest ResourceDef),
    /// The class constructor or a factory of a resource.
    Constructor {
        resource: &'manifest ResourceDef,
        constructor: &'manifest CallableDef,
    },
    Method {
        resource: &'manifest ResourceDef,
        method: &'manifest CallableDef,
    },
    Constant(&'manifest ConstantDef),
}

impl fmt::Display for Item<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(model) => {
                write!(formatter, "model `{}::{}`", model.rust_module, model.name)
            }
            Self::Variant { model, variant } => write!(
                formatter,
                "variant `{}::{}::{variant}`",
                model.rust_module, model.name
            ),
            Self::Error(error) => {
                write!(formatter, "error `{}::{}`", error.rust_module, error.name)
            }
            Self::ErrorCodes(error) => write!(
                formatter,
                "the code type `{}` of {}",
                error_code_alias(error),
                Self::Error(error)
            ),
            Self::Function(function) => write!(
                formatter,
                "function `{}::{}`",
                function.rust_module, function.callable.rust_name
            ),
            Self::Resource(resource) => write!(
                formatter,
                "resource `{}::{}`",
                resource.rust_module, resource.name
            ),
            Self::Constructor {
                resource,
                constructor,
            } => write!(
                formatter,
                "constructor `{}::{}::{}`",
                resource.rust_module, resource.name, constructor.rust_name
            ),
            Self::Method { resource, method } => write!(
                formatter,
                "method `{}::{}::{}`",
                resource.rust_module, resource.name, method.rust_name
            ),
            Self::Constant(constant) => write!(
                formatter,
                "constant `{}::{}`",
                constant.rust_module, constant.host_name
            ),
        }
    }
}

/// A linked contract with its namespace grouping and definition lookups.
pub(crate) struct Contract<'manifest> {
    /// The linked contract.
    pub(crate) manifest: &'manifest Manifest,
    rule: NamespaceRule,
    namespaces: BTreeMap<Namespace, NamespaceItems<'manifest>>,
    /// The Cargo package and the Rust module of each declaration, once each.
    origins: BTreeSet<(&'manifest CargoPackageId, &'manifest str)>,
    /// Model definitions by owning package and Rust path.
    types: HashMap<(&'manifest str, &'manifest str), &'manifest TypeDef>,
    /// Error definitions by owning package and Rust path.
    errors: HashMap<(&'manifest str, &'manifest str), &'manifest ErrorDef>,
    /// Resource definitions by owning package and Rust path.
    resources: HashMap<(&'manifest str, &'manifest str), &'manifest ResourceDef>,
}

impl<'manifest> Contract<'manifest> {
    /// Index every declaration in the namespace that `rule` derives, and
    /// every namespace above one.
    #[must_use]
    pub(crate) fn new(manifest: &'manifest Manifest, rule: NamespaceRule) -> Self {
        let mut contract = Self {
            manifest,
            rule,
            namespaces: BTreeMap::new(),
            origins: BTreeSet::new(),
            types: manifest
                .types
                .iter()
                .map(|item| ((item.owner.0.as_str(), item.id.as_str()), item))
                .collect(),
            errors: manifest
                .errors
                .iter()
                .map(|item| ((item.owner.0.as_str(), item.id.as_str()), item))
                .collect(),
            resources: manifest
                .resources
                .iter()
                .map(|item| ((item.owner.0.as_str(), item.id.as_str()), item))
                .collect(),
        };
        let mut namespaces: BTreeMap<Namespace, NamespaceItems<'manifest>> = BTreeMap::new();
        namespaces.entry(Namespace::root()).or_default();
        let mut namespace = |owner: &'manifest CargoPackageId, rust_module: &'manifest str| {
            contract.origins.insert((owner, rust_module));
            contract.rule.namespace(owner, rust_module)
        };
        for item in &manifest.types {
            let items = namespaces.entry(namespace(&item.owner, &item.rust_module));
            items.or_default().types.push(item);
        }
        for item in &manifest.errors {
            let items = namespaces.entry(namespace(&item.owner, &item.rust_module));
            items.or_default().errors.push(item);
        }
        for item in &manifest.functions {
            let items = namespaces.entry(namespace(&item.owner, &item.rust_module));
            items.or_default().functions.push(item);
        }
        for item in &manifest.resources {
            let items = namespaces.entry(namespace(&item.owner, &item.rust_module));
            items.or_default().resources.push(item);
        }
        for item in &manifest.constants {
            let items = namespaces.entry(namespace(&item.owner, &item.rust_module));
            items.or_default().constants.push(item);
        }
        let declared: Vec<Namespace> = namespaces.keys().cloned().collect();
        for namespace in declared {
            for length in 1..=namespace.package.len() {
                namespaces
                    .entry(Namespace {
                        package: namespace.package[..length].to_vec(),
                        modules: Vec::new(),
                    })
                    .or_default();
            }
            for length in 1..namespace.modules.len() {
                namespaces
                    .entry(Namespace {
                        package: namespace.package.clone(),
                        modules: namespace.modules[..length].to_vec(),
                    })
                    .or_default();
            }
        }
        contract.namespaces = namespaces;
        contract
    }

    /// Return the Rust-shaped public namespace for one exported declaration.
    #[must_use]
    pub(crate) fn namespace(&self, owner: &CargoPackageId, rust_module: &str) -> Namespace {
        self.rule.namespace(owner, rust_module)
    }

    /// Return the rule that derives the namespaces.
    #[must_use]
    pub(crate) fn rule(&self) -> &NamespaceRule {
        &self.rule
    }

    /// Return every generated namespace with its declarations, ordered by
    /// their package segments, then by their Rust module segments. Under
    /// `namespaces = "folders"` that is not the order of the host paths:
    /// `sources::quotes` comes before `sources/data`.
    #[must_use]
    pub(crate) fn namespaces(&self) -> &BTreeMap<Namespace, NamespaceItems<'manifest>> {
        &self.namespaces
    }

    /// Return the Cargo package and the Rust module that declare something,
    /// once each, in order.
    pub(crate) fn origins(
        &self,
    ) -> impl Iterator<Item = (&'manifest CargoPackageId, &'manifest str)> {
        self.origins.iter().copied()
    }

    /// Return a namespace's stable position among all generated namespaces.
    #[must_use]
    pub(crate) fn namespace_index(&self, namespace: &Namespace) -> usize {
        debug_assert!(
            self.namespaces.contains_key(namespace),
            "callers pass only namespaces that this contract derived"
        );
        self.namespaces.range(..namespace).count()
    }

    /// Resolve a named type identity to its linked definition.
    pub(crate) fn type_definition(&self, identity: &DefinitionId) -> Result<&'manifest TypeDef> {
        self.types
            .get(&(identity.owner.0.as_str(), identity.id.as_str()))
            .copied()
            .with_context(|| format!("missing type `{identity}`"))
    }

    /// Resolve a named error identity to its linked definition.
    pub(crate) fn error_definition(&self, identity: &DefinitionId) -> Result<&'manifest ErrorDef> {
        self.errors
            .get(&(identity.owner.0.as_str(), identity.id.as_str()))
            .copied()
            .with_context(|| format!("missing error `{identity}`"))
    }

    /// Resolve a resource identity to its linked definition.
    pub(crate) fn resource_definition(
        &self,
        identity: &DefinitionId,
    ) -> Result<&'manifest ResourceDef> {
        self.resources
            .get(&(identity.owner.0.as_str(), identity.id.as_str()))
            .copied()
            .with_context(|| format!("missing resource `{identity}`"))
    }

    /// Return whether the contract passes Arrow values. They appear only at
    /// the top level of a parameter or return.
    #[must_use]
    pub(crate) fn uses_arrow(&self) -> bool {
        self.manifest
            .type_refs()
            .any(|reference| matches!(reference, TypeRef::Arrow { .. }))
    }

    /// Return whether the contract passes numeric buffers. The references of
    /// every model are among the contract's, so no name needs resolving.
    #[must_use]
    pub(crate) fn uses_buffer(&self) -> bool {
        self.manifest
            .type_refs()
            .flat_map(nodes)
            .any(|reference| matches!(reference, TypeRef::Buffer { .. }))
    }

    /// Return whether `reference`, or a reference inside it or inside a
    /// model that it reaches, matches `predicate`.
    pub(crate) fn reaches(
        &self,
        reference: &TypeRef,
        predicate: impl Fn(&TypeRef) -> bool,
    ) -> Result<bool> {
        let mut pending = vec![reference];
        let mut visited = BTreeSet::new();
        while let Some(reference) = pending.pop() {
            for node in nodes(reference) {
                if predicate(node) {
                    return Ok(true);
                }
                if let TypeRef::Named { identity } = node
                    && visited.insert(identity)
                {
                    pending.extend(self.type_definition(identity)?.type_refs());
                }
            }
        }
        Ok(false)
    }
}

/// Return `reference` and every reference inside it, depth first: the items
/// of options, lists, and tuples, and the values of maps. The models that
/// named references name are not followed; see [`Contract::reaches`].
pub(super) fn nodes(reference: &TypeRef) -> impl Iterator<Item = &TypeRef> {
    let mut pending = vec![reference];
    iter::from_fn(move || {
        let reference = pending.pop()?;
        pending.extend(reference.children().iter().rev());
        Some(reference)
    })
}

/// Return every type reference that one namespace's declarations use
/// directly: model fields and aliases, callables' parameters and returns,
/// and constants.
#[must_use]
pub(super) fn namespace_refs<'a>(items: &NamespaceItems<'a>) -> Vec<&'a TypeRef> {
    let types = items
        .types
        .iter()
        .flat_map(|definition| definition.type_refs());
    let functions = items
        .functions
        .iter()
        .flat_map(|function| function.type_refs());
    let resources = items
        .resources
        .iter()
        .flat_map(|resource| resource.type_refs());
    let constants = items.constants.iter().map(|constant| &constant.ty);
    types
        .chain(functions)
        .chain(resources)
        .chain(constants)
        .collect()
}

/// Return the public name of the type that lists an error's codes.
#[must_use]
pub(super) fn error_code_alias(error: &ErrorDef) -> String {
    format!("{}Code", error.name)
}

/// Return the generated class/interface name for a tagged-enum variant.
#[must_use]
pub(super) fn tagged_variant_name(type_name: &str, variant_name: &str) -> String {
    format!("{type_name}{variant_name}")
}

/// Return the Rust names of the variants that the hosts declare as types of
/// their own: those of tagged and externally tagged enums.
#[must_use]
pub(super) fn declared_variants(definition: &TypeDef) -> Vec<&str> {
    match &definition.shape {
        TypeShape::TaggedEnum { variants, .. } => variants
            .iter()
            .map(|variant| variant.rust_name.as_str())
            .collect(),
        TypeShape::ExternalEnum { variants } => variants
            .iter()
            .map(|variant| variant.rust_name.as_str())
            .collect(),
        TypeShape::Struct { .. } | TypeShape::StringEnum { .. } | TypeShape::Alias { .. } => {
            Vec::new()
        }
    }
}

/// Return the schema with which both runtimes restore a value of
/// `reference`, and the TypeScript runtime prepares one: `null` for a value
/// that crosses as it is, and otherwise `[kind, ...]`. A `named` schema
/// names a model by its [`DefinitionId`], whose own schema, [`named_spec`],
/// each runtime holds once.
#[must_use]
pub(super) fn spec(reference: &TypeRef) -> Value {
    match reference {
        TypeRef::Option { item } => spec(item),
        TypeRef::List { item } => json!(["list", spec(item)]),
        TypeRef::Map { value } => json!(["map", spec(value)]),
        TypeRef::Tuple { items } => {
            let items: Vec<Value> = items.iter().map(spec).collect();
            json!(["tuple", items])
        }
        TypeRef::Named { identity } => json!(["named", identity.to_string()]),
        TypeRef::Bytes | TypeRef::FixedBytes { .. } => json!(["bytes"]),
        TypeRef::Duration => json!(["duration"]),
        TypeRef::Buffer { element } => json!(["buffer", element.numpy_name()]),
        TypeRef::Json => json!(["json"]),
        // A date-time argument may be a JavaScript `Date`.
        TypeRef::DateTime => json!(["date"]),
        TypeRef::Unit
        | TypeRef::Bool
        | TypeRef::Int { .. }
        | TypeRef::Float { .. }
        | TypeRef::String
        | TypeRef::Date
        | TypeRef::Arrow { .. }
        | TypeRef::Resource { .. }
        | TypeRef::Callback { .. } => Value::Null,
    }
}

/// Return, for each of `params`, whether callers may omit it: a parameter
/// with a default, or an option, when every parameter after it may be omitted
/// too. Hosts cannot skip a parameter that has a required one after it.
#[must_use]
pub(super) fn omissible_params(params: &[ParamDef]) -> Vec<bool> {
    let mut omissible = vec![false; params.len()];
    for (slot, param) in omissible.iter_mut().zip(params).rev() {
        if param.default.is_none() && !matches!(param.ty, TypeRef::Option { .. }) {
            break;
        }
        *slot = param.default.is_none();
    }
    omissible
}

/// Return the name of the property that a setter method assigns: its name
/// without the `set_` that Rust's naming asks for.
#[must_use]
pub(super) fn property_name(setter: &str) -> &str {
    setter.strip_prefix("set_").unwrap_or(setter)
}

/// Return the schema of a model: its fields' schemas by wire name, by
/// variant for a tagged enum, the schema of what each variant of an
/// externally tagged enum carries, or the schema of an alias's target.
///
/// A struct's schema, and a tagged enum's by variant, also lists the
/// `required` fields of an `Option` type, if any, which the TypeScript
/// runtime checks are present, because Serde reads a missing `Option` as
/// `None`. Serde requires every other required field itself.
#[must_use]
pub(super) fn named_spec(definition: &TypeDef) -> Value {
    let fields = |fields: &[FieldDef]| -> Value {
        fields
            .iter()
            .map(|field| (field.wire_name.clone(), spec(&field.ty)))
            .collect()
    };
    let structure = |members: &[FieldDef]| -> Value {
        let required = required_options(members);
        if required.is_empty() {
            json!(["struct", fields(members)])
        } else {
            json!(["struct", fields(members), required])
        }
    };
    match &definition.shape {
        TypeShape::Struct { fields: members } => structure(members),
        TypeShape::TaggedEnum { tag, variants } => {
            let required: serde_json::Map<String, Value> = variants
                .iter()
                .filter_map(|variant| {
                    let required = required_options(&variant.fields);
                    (!required.is_empty()).then(|| (variant.wire_name.clone(), json!(required)))
                })
                .collect();
            let variants: Value = variants
                .iter()
                .map(|variant| (variant.wire_name.clone(), fields(&variant.fields)))
                .collect();
            if required.is_empty() {
                json!(["tagged", tag, variants])
            } else {
                json!(["tagged", tag, variants, required])
            }
        }
        TypeShape::ExternalEnum { variants } => {
            let variants: Value = variants
                .iter()
                .map(|variant| {
                    let payload = match &variant.payload {
                        VariantPayload::Unit => Value::Null,
                        VariantPayload::Value { ty } => spec(ty),
                        VariantPayload::Fields { fields: members } => structure(members),
                    };
                    (variant.wire_name.clone(), payload)
                })
                .collect();
            json!(["external", variants])
        }
        TypeShape::Alias { target } => json!(["alias", spec(target)]),
        TypeShape::StringEnum { .. } => Value::Null,
    }
}

/// Return the wire names of the `required` fields of an `Option` type.
fn required_options(fields: &[FieldDef]) -> Vec<&str> {
    fields
        .iter()
        .filter(|field| field.required && matches!(field.ty, TypeRef::Option { .. }))
        .map(|field| field.wire_name.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use bimetal_ir::{BufferElement, EnumVariantDef};

    use super::*;
    use crate::fixtures;
    use crate::namespaces::Folders;

    /// Return the Python path of every namespace of `contract`, in order.
    fn python_paths(contract: &Contract<'_>) -> Vec<String> {
        contract
            .namespaces()
            .keys()
            .map(|namespace| namespace.python_segments().join("."))
            .collect()
    }

    #[test]
    fn packages_name_their_namespaces_without_the_shared_prefix() {
        let mut manifest = fixtures::manifest();
        manifest.functions.push(fixtures::declared(
            "test-geometry",
            "test_geometry::shapes",
            "area",
        ));

        let contract = fixtures::contract(&manifest);

        assert_eq!(python_paths(&contract), ["", "geometry", "geometry.shapes"]);
    }

    #[test]
    fn every_folder_above_a_package_is_a_namespace() {
        let mut manifest = fixtures::manifest();
        manifest.functions.extend([
            fixtures::declared("xf-polymarket", "xf_polymarket::markets::fees", "fee"),
            fixtures::declared("xf-sports", "xf_sports", "score"),
        ]);
        let folders = Folders::new(
            Path::new("/work"),
            ("test", Path::new("/work/packages/bindings")),
            &[
                (
                    "xf-polymarket",
                    Path::new("/work/packages/sources/venues/polymarket"),
                ),
                ("xf-sports", Path::new("/work/packages/sources/data/sports")),
            ],
        )
        .unwrap();

        let contract = Contract::new(&manifest, folders.rule(&manifest).unwrap());

        assert_eq!(
            python_paths(&contract),
            [
                "",
                "sources",
                "sources.data",
                "sources.data.sports",
                "sources.venues",
                "sources.venues.polymarket",
                "sources.venues.polymarket.markets",
                "sources.venues.polymarket.markets.fees",
            ]
        );
        let fees = contract.namespace(
            &CargoPackageId::new("xf-polymarket"),
            "xf_polymarket::markets::fees",
        );
        assert_eq!(contract.namespaces()[&fees].functions.len(), 1);
        let sources = Namespace {
            package: vec!["sources".to_owned()],
            modules: Vec::new(),
        };
        assert!(contract.namespaces()[&sources].functions.is_empty());
    }

    #[test]
    fn external_enums_restore_what_each_variant_carries() {
        assert_eq!(
            named_spec(&fixtures::external_enum()),
            json!([
                "external",
                {"empty": null, "label": null, "circle": ["struct", {"radius": null}]}
            ])
        );
    }

    #[test]
    fn schemas_list_the_required_options_that_serde_would_read_as_none() {
        let optional = |name: &str| FieldDef {
            required: false,
            ..fixtures::typed_field(
                name,
                TypeRef::Option {
                    item: Box::new(TypeRef::String),
                },
            )
        };
        let required = |name: &str| FieldDef {
            required: true,
            ..optional(name)
        };
        let fields = vec![
            required("note"),
            optional("hint"),
            fixtures::typed_field("label", TypeRef::String),
        ];
        let review = fixtures::model("Review", fields.clone());
        assert_eq!(
            named_spec(&review),
            json!(["struct", {"note": null, "hint": null, "label": null}, ["note"]])
        );

        let variant = |name: &str, fields: Vec<FieldDef>| EnumVariantDef {
            rust_name: name.to_owned(),
            wire_name: name.to_lowercase(),
            docs: None,
            fields,
        };
        let event = TypeDef {
            shape: TypeShape::TaggedEnum {
                tag: "kind".to_owned(),
                variants: vec![variant("Noted", fields), variant("Plain", Vec::new())],
            },
            ..fixtures::model("Event", Vec::new())
        };
        assert_eq!(
            named_spec(&event),
            json!([
                "tagged",
                "kind",
                {"noted": {"note": null, "hint": null, "label": null}, "plain": {}},
                {"noted": ["note"]}
            ])
        );
        assert_eq!(
            named_spec(&fixtures::model("Plain", vec![optional("hint")])),
            json!(["struct", {"hint": null}])
        );
    }

    #[test]
    fn reaching_follows_named_aliases() {
        let mut manifest = fixtures::manifest();
        manifest.types.push(TypeDef {
            shape: TypeShape::Alias {
                target: TypeRef::Buffer {
                    element: BufferElement::F64,
                },
            },
            ..fixtures::model("Samples", Vec::new())
        });
        let reference = TypeRef::List {
            item: Box::new(fixtures::named("Samples")),
        };
        let contract = fixtures::contract(&manifest);
        let buffer = |reference: &TypeRef| matches!(reference, TypeRef::Buffer { .. });

        assert!(
            contract
                .reaches(&reference, buffer)
                .expect("resolve named alias")
        );
        assert!(!nodes(&reference).any(buffer));
        assert!(
            !contract
                .reaches(&reference, |reference| matches!(reference, TypeRef::Bytes))
                .expect("resolve named alias")
        );
    }
}
