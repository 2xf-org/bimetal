//! Builders of contract IR for the tests. Every declaration belongs to the
//! Cargo package `test`, in its root module `test`, except those of
//! [`declared`] and [`declared_model`].

use bimetal_ir::{
    CallableDef, CargoPackageId, DefinitionId, ErrorDef, ExternalVariantDef, FieldConstraints,
    FieldDef, FunctionDef, Manifest, MethodDef, ParamDef, ResourceDef, TypeDef, TypeRef, TypeShape,
    VariantPayload,
};

use crate::contract::Contract;
use crate::namespaces::NamespaceRule;

/// The package, and the module, of every declaration.
const PACKAGE: &str = "test";

/// Index `manifest`, naming its namespaces after the Cargo packages, as
/// `bimetal.toml` does by default.
#[must_use]
pub(crate) fn contract(manifest: &Manifest) -> Contract<'_> {
    Contract::new(manifest, NamespaceRule::Packages(manifest.namespace_rule()))
}

/// Return a contract of `test` that declares nothing.
#[must_use]
pub(crate) fn manifest() -> Manifest {
    Manifest {
        package_name: PACKAGE.to_owned(),
        package_version: "0.0.0".to_owned(),
        types: Vec::new(),
        errors: Vec::new(),
        functions: Vec::new(),
        resources: Vec::new(),
        constants: Vec::new(),
    }
}

/// Return a string parameter.
#[must_use]
pub(crate) fn param(rust_name: &str, host_name: &str) -> ParamDef {
    ParamDef {
        rust_name: rust_name.to_owned(),
        host_name: host_name.to_owned(),
        ty: TypeRef::String,
        default: None,
    }
}

/// Return a parameter of type `ty`, named `name` in Rust and in the host.
#[must_use]
pub(crate) fn typed_param(name: &str, ty: TypeRef) -> ParamDef {
    ParamDef {
        ty,
        ..param(name, name)
    }
}

/// Return a synchronous callable without documentation or a typed error.
#[must_use]
pub(crate) fn callable(rust_name: &str, host_name: &str, params: Vec<ParamDef>) -> CallableDef {
    CallableDef {
        rust_name: rust_name.to_owned(),
        host_name: host_name.to_owned(),
        docs: None,
        params,
        error: None,
        is_async: false,
        brief: false,
    }
}

/// Return a function that returns `()`, whose native name is
/// `native_<rust_name>`.
#[must_use]
pub(crate) fn function(rust_name: &str, host_name: &str, params: Vec<ParamDef>) -> FunctionDef {
    FunctionDef {
        owner: CargoPackageId::new(PACKAGE),
        rust_module: PACKAGE.to_owned(),
        native_name: format!("native_{rust_name}"),
        callable: callable(rust_name, host_name, params),
        returns: TypeRef::Unit,
    }
}

/// Return a function `name` like [`function`]'s, that the Cargo package
/// `owner` declares in `rust_module`.
#[must_use]
pub(crate) fn declared(owner: &str, rust_module: &str, name: &str) -> FunctionDef {
    FunctionDef {
        owner: CargoPackageId::new(owner),
        rust_module: rust_module.to_owned(),
        ..function(name, name, Vec::new())
    }
}

/// Return a method without parameters that returns `()`.
#[must_use]
pub(crate) fn method(rust_name: &str, host_name: &str) -> MethodDef {
    MethodDef {
        callable: callable(rust_name, host_name, Vec::new()),
        returns: TypeRef::Unit,
        protocol: None,
    }
}

/// Return a struct model.
#[must_use]
pub(crate) fn model(name: &str, fields: Vec<FieldDef>) -> TypeDef {
    TypeDef {
        owner: CargoPackageId::new(PACKAGE),
        rust_module: PACKAGE.to_owned(),
        id: format!("{PACKAGE}::{name}"),
        name: name.to_owned(),
        docs: None,
        shape: TypeShape::Struct { fields },
    }
}

/// Return a struct model `name` without fields, that the Cargo package
/// `owner` declares in `rust_module`.
#[must_use]
pub(crate) fn declared_model(owner: &str, rust_module: &str, name: &str) -> TypeDef {
    TypeDef {
        owner: CargoPackageId::new(owner),
        rust_module: rust_module.to_owned(),
        id: format!("{rust_module}::{name}"),
        ..model(name, Vec::new())
    }
}

/// Return the externally tagged enum `Shape`, with a variant of each kind:
/// `Empty`, `Label(String)`, and `Circle { radius: f64 }`, documented.
#[must_use]
pub(crate) fn external_enum() -> TypeDef {
    let variant = |rust_name: &str, payload: VariantPayload| ExternalVariantDef {
        rust_name: rust_name.to_owned(),
        wire_name: rust_name.to_lowercase(),
        docs: None,
        payload,
    };
    TypeDef {
        shape: TypeShape::ExternalEnum {
            variants: vec![
                variant("Empty", VariantPayload::Unit),
                variant(
                    "Label",
                    VariantPayload::Value {
                        ty: TypeRef::String,
                    },
                ),
                ExternalVariantDef {
                    docs: Some("A circle.".to_owned()),
                    ..variant(
                        "Circle",
                        VariantPayload::Fields {
                            fields: vec![typed_field("radius", TypeRef::Float { bits: 64 })],
                        },
                    )
                },
            ],
        },
        ..model("Shape", Vec::new())
    }
}

/// Return a required string field.
#[must_use]
pub(crate) fn field(rust_name: &str, wire_name: &str) -> FieldDef {
    FieldDef {
        rust_name: rust_name.to_owned(),
        wire_name: wire_name.to_owned(),
        docs: None,
        ty: TypeRef::String,
        required: true,
        default: None,
        constraints: FieldConstraints::default(),
    }
}

/// Return a required field of type `ty`, named `name` in Rust and on the
/// wire.
#[must_use]
pub(crate) fn typed_field(name: &str, ty: TypeRef) -> FieldDef {
    FieldDef {
        ty,
        ..field(name, name)
    }
}

/// Return a typed error without codes.
#[must_use]
pub(crate) fn error(name: &str) -> ErrorDef {
    ErrorDef {
        owner: CargoPackageId::new(PACKAGE),
        rust_module: PACKAGE.to_owned(),
        id: format!("{PACKAGE}::{name}"),
        name: name.to_owned(),
        docs: None,
        codes: Vec::new(),
    }
}

/// Return a reference to the model `name`.
#[must_use]
pub(crate) fn named(name: &str) -> TypeRef {
    TypeRef::Named {
        identity: DefinitionId::new(PACKAGE, format!("{PACKAGE}::{name}")),
    }
}

/// Return the resource `Recorder`, whose class constructor is `new`.
#[must_use]
pub(crate) fn recorder(factories: Vec<CallableDef>, methods: Vec<MethodDef>) -> ResourceDef {
    ResourceDef {
        owner: CargoPackageId::new(PACKAGE),
        rust_module: PACKAGE.to_owned(),
        id: format!("{PACKAGE}::Recorder"),
        name: "Recorder".to_owned(),
        native_name: "native_recorder".to_owned(),
        docs: None,
        constructor: Some(callable("new", "new", Vec::new())),
        factories,
        methods,
    }
}

/// Return a contract that declares a documented `Recorder`, with the
/// factory `from_default`.
#[must_use]
pub(crate) fn recorder_manifest() -> Manifest {
    let constructor = |rust_name: &str, host_name: &str| CallableDef {
        docs: Some(format!("Create a recorder with `{rust_name}`.")),
        ..callable(rust_name, host_name, Vec::new())
    };
    let mut manifest = manifest();
    manifest.resources.push(ResourceDef {
        docs: Some("A stateful recorder.".to_owned()),
        constructor: Some(constructor("new", "new")),
        ..recorder(vec![constructor("from_default", "fromDefault")], Vec::new())
    });
    manifest
}
