//! Rendering and dependency ordering of Python models and aliases.
//!
//! Local aliases are topologically sorted before emission. Cross-namespace
//! models use explicit module aliases, while post-definition `model_rebuild`
//! calls resolve forward references deterministically.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use anyhow::{Result, bail};
use bimetal_ir::python_names;
use bimetal_ir::{
    BufferElement, DefinitionId, EnumVariantDef, ExternalVariantDef, FieldConstraints, FieldDef,
    TypeDef, TypeRef, TypeShape, VariantPayload,
};

use crate::contract::{self, NamespaceItems};
use crate::literal;

use super::context::PythonContext;
use super::package::MODELS_MODULE;
use super::render;

/// Render all generated model declarations for one Python namespace.
pub(super) fn python_models(
    items: &NamespaceItems<'_>,
    context: &PythonContext<'_>,
) -> Result<String> {
    let model_names = render::python_model_names(items);
    let context = &context.within(model_names.iter().map(String::as_str));
    let mut source = String::new();
    for element in render::namespace_buffers(items) {
        let scalar = element.numpy_name();
        let name = render::python_buffer_alias(element);
        // `tolist` returns Python scalars of the element's kind.
        let item = if matches!(element, BufferElement::F32 | BufferElement::F64) {
            context.builtin("float")
        } else {
            context.builtin("int")
        };
        render::begin_python_alias(&mut source);
        let _ = writeln!(
            source,
            "{name}: {alias} = {annotated}[\n    {array}[{np}.{scalar}],\n    {before}(lambda value: {np}.asarray(value, dtype={np}.{scalar})),\n    {serializer}(lambda value: value.tolist(), return_type={list}[{item}]),\n]",
            alias = context.import("TypeAlias"),
            annotated = context.import("Annotated"),
            array = context.import("NDArray"),
            np = context.import("np"),
            before = context.import("BeforeValidator"),
            serializer = context.import("PlainSerializer"),
            list = context.builtin("list"),
        );
    }

    for definition in ordered_python_types(items)? {
        emit_python_type(&mut source, definition, context)?;
    }
    let mut rebuilds = Vec::new();
    for definition in &items.types {
        if matches!(definition.shape, TypeShape::Struct { .. }) {
            rebuilds.push(definition.name.clone());
        }
        rebuilds.extend(
            contract::declared_variants(definition)
                .into_iter()
                .map(|variant| contract::tagged_variant_name(&definition.name, variant)),
        );
    }
    if !rebuilds.is_empty() {
        render::begin_python_top_level(&mut source);
        for name in rebuilds {
            let _ = writeln!(source, "{name}.model_rebuild()");
        }
    }
    Ok(context.module(MODELS_MODULE, &source))
}

/// Order local aliases after the local declarations on which they depend.
fn ordered_python_types<'a>(items: &NamespaceItems<'a>) -> Result<Vec<&'a TypeDef>> {
    let mut ordered: Vec<&TypeDef> = items
        .types
        .iter()
        .copied()
        .filter(|definition| !matches!(definition.shape, TypeShape::Alias { .. }))
        .collect();
    let mut pending: Vec<&TypeDef> = items
        .types
        .iter()
        .copied()
        .filter(|definition| matches!(definition.shape, TypeShape::Alias { .. }))
        .collect();
    let local_aliases: BTreeMap<DefinitionId, &str> = pending
        .iter()
        .map(|definition| (definition.identity(), definition.name.as_str()))
        .collect();
    let mut emitted: BTreeSet<DefinitionId> = BTreeSet::new();
    while !pending.is_empty() {
        let Some(index) = pending.iter().position(|definition| {
            let TypeShape::Alias { target } = &definition.shape else {
                return true;
            };
            contract::nodes(target)
                .filter_map(|reference| match reference {
                    TypeRef::Named { identity } => Some(identity),
                    _ => None,
                })
                .filter(|identity| local_aliases.contains_key(*identity))
                .all(|identity| emitted.contains(identity))
        }) else {
            let names = pending
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>()
                .join(" -> ");
            bail!("Python aliases form a dependency cycle: {names}");
        };
        let definition = pending.remove(index);
        emitted.insert(definition.identity());
        ordered.push(definition);
    }
    Ok(ordered)
}

/// Render one contract type as a Python model, enum, union, or alias.
fn emit_python_type(
    source: &mut String,
    definition: &TypeDef,
    context: &PythonContext<'_>,
) -> Result<()> {
    match &definition.shape {
        TypeShape::Struct { fields } => {
            render::begin_python_top_level(source);
            let _ = writeln!(
                source,
                "class {}({}):",
                definition.name,
                context.import("BaseModel")
            );
            if render::emit_python_doc(source, definition.docs.as_deref(), "    ") {
                source.push('\n');
            }
            emit_model_config(source, context);
            let names: Vec<String> = fields
                .iter()
                .map(|field| python_field_name(&field.rust_name))
                .collect();
            let context = &context.within(names.iter().map(String::as_str));
            for field in fields {
                emit_python_field(source, field, context, "    ")?;
            }
        }
        TypeShape::StringEnum { variants } => {
            render::begin_python_top_level(source);
            let _ = writeln!(
                source,
                "class {}({}):",
                definition.name,
                context.import("StrEnum")
            );
            if render::emit_python_doc(source, definition.docs.as_deref(), "    ")
                && !variants.is_empty()
            {
                source.push('\n');
            }
            if variants.is_empty() {
                source.push_str("    pass\n");
            }
            for variant in variants {
                render::emit_python_comment_docs(source, variant.docs.as_deref(), "    #: ");
                let _ = writeln!(
                    source,
                    "    {} = {}",
                    python_names::identifier(&variant.rust_name),
                    literal::quoted(&variant.wire_name)
                );
            }
        }
        TypeShape::TaggedEnum { tag, variants } => {
            emit_python_tagged_enum(source, definition, tag, variants, context)?;
        }
        TypeShape::ExternalEnum { variants } => {
            emit_python_external_enum(source, definition, variants, context)?;
        }
        TypeShape::Alias { target } => {
            render::begin_python_alias(source);
            render::emit_python_comment_docs(source, definition.docs.as_deref(), "#: ");
            let _ = writeln!(
                source,
                "{}: {} = {}",
                definition.name,
                context.import("TypeAlias"),
                render::python_ref(target, context)?
            );
        }
    }
    Ok(())
}

/// Render a tagged enum as a model for each variant, whose `tag` field holds
/// the variant's wire name, and the union of those models.
fn emit_python_tagged_enum(
    source: &mut String,
    definition: &TypeDef,
    tag: &str,
    variants: &[EnumVariantDef],
    context: &PythonContext<'_>,
) -> Result<()> {
    let tag_name = python_field_name(tag);
    for variant in variants {
        let name = contract::tagged_variant_name(&definition.name, &variant.rust_name);
        render::begin_python_top_level(source);
        let _ = writeln!(source, "class {name}({}):", context.import("BaseModel"));
        if render::emit_python_doc(source, variant.docs.as_deref(), "    ") {
            source.push('\n');
        }
        emit_model_config(source, context);
        let names: Vec<String> = variant
            .fields
            .iter()
            .map(|field| python_field_name(&field.rust_name))
            .collect();
        let context = &context.within(names.iter().map(String::as_str).chain([tag_name.as_str()]));
        let mut options = vec![format!("default={}", literal::quoted(&variant.wire_name))];
        options.extend(wire_name_options(&tag_name, tag));
        let _ = writeln!(
            source,
            "    {tag_name}: {}[{}] = {}({})",
            context.import("Literal"),
            literal::quoted(&variant.wire_name),
            context.import("Field"),
            options.join(", "),
        );
        for field in &variant.fields {
            emit_python_field(source, field, context, "    ")?;
        }
    }
    let names = variants
        .iter()
        .map(|variant| contract::tagged_variant_name(&definition.name, &variant.rust_name))
        .collect::<Vec<_>>()
        .join(" | ");
    render::begin_python_top_level(source);
    render::emit_python_comment_docs(source, definition.docs.as_deref(), "#: ");
    let _ = writeln!(
        source,
        "{}: {} = {names}",
        definition.name,
        context.import("TypeAlias")
    );
    Ok(())
}

/// Render an externally tagged enum as a model for each variant, whose
/// runtime base class reads and writes the variant's wire form, and the
/// union of those models, which picks a variant by its wire name.
fn emit_python_external_enum(
    source: &mut String,
    definition: &TypeDef,
    variants: &[ExternalVariantDef],
    context: &PythonContext<'_>,
) -> Result<()> {
    let base = context.runtime("ExternalVariant");
    for variant in variants {
        let name = contract::tagged_variant_name(&definition.name, &variant.rust_name);
        render::begin_python_top_level(source);
        let _ = writeln!(source, "class {name}({base}):");
        if render::emit_python_doc(source, variant.docs.as_deref(), "    ") {
            source.push('\n');
        }
        let (form, fields) = match &variant.payload {
            VariantPayload::Unit => ("unit", Vec::new()),
            VariantPayload::Value { ty } => ("value", vec![value_field(ty)]),
            VariantPayload::Fields { fields } => ("fields", fields.clone()),
        };
        let _ = writeln!(
            source,
            "    __bimetal_variant__ = {}\n    __bimetal_form__ = {}",
            literal::quoted(&variant.wire_name),
            literal::quoted(form)
        );
        emit_model_config(source, context);
        let names: Vec<String> = fields
            .iter()
            .map(|field| python_field_name(&field.rust_name))
            .collect();
        let context = &context.within(names.iter().map(String::as_str));
        for field in &fields {
            emit_python_field(source, field, context, "    ")?;
        }
    }
    let annotated = context.import("Annotated");
    let tag = context.runtime("variant_tag");
    let members = variants
        .iter()
        .map(|variant| {
            format!(
                "{annotated}[{}, {tag}({})]",
                contract::tagged_variant_name(&definition.name, &variant.rust_name),
                literal::quoted(&variant.wire_name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n    | ");
    render::begin_python_top_level(source);
    render::emit_python_comment_docs(source, definition.docs.as_deref(), "#: ");
    let _ = writeln!(
        source,
        "{}: {} = {annotated}[\n    {members},\n    {},\n]",
        definition.name,
        context.import("TypeAlias"),
        context.runtime("external_discriminator")
    );
    Ok(())
}

/// Return the one field that holds the value of a one-field variant.
fn value_field(ty: &TypeRef) -> FieldDef {
    FieldDef {
        rust_name: "value".to_owned(),
        wire_name: "value".to_owned(),
        docs: None,
        ty: ty.clone(),
        required: true,
        default: None,
        constraints: FieldConstraints::default(),
    }
}

/// Emit the immutable, strict Pydantic configuration shared by generated models.
///
/// Fields may start with `model_`, such as `model_name`: those that would
/// hide a member of `BaseModel` are renamed instead, by [`python_field_name`].
fn emit_model_config(source: &mut String, context: &PythonContext<'_>) {
    let _ = writeln!(
        source,
        "    model_config = {}(\n        frozen=True,\n        populate_by_name=True,\n        extra=\"forbid\",\n        arbitrary_types_allowed=True,\n        protected_namespaces=(),\n    )",
        context.import("ConfigDict")
    );
}

/// The members of Pydantic's `BaseModel` that a field of the same name would
/// hide, which Pydantic warns about or rejects.
const BASE_MODEL_MEMBERS: [&str; 28] = [
    "construct",
    "copy",
    "dict",
    "from_orm",
    "json",
    "model_computed_fields",
    "model_config",
    "model_construct",
    "model_copy",
    "model_dump",
    "model_dump_json",
    "model_extra",
    "model_fields",
    "model_fields_set",
    "model_json_schema",
    "model_parametrized_name",
    "model_post_init",
    "model_rebuild",
    "model_validate",
    "model_validate_json",
    "model_validate_strings",
    "parse_file",
    "parse_obj",
    "parse_raw",
    "schema",
    "schema_json",
    "update_forward_refs",
    "validate",
];

/// Return the Python name of a model field, or of a tagged union's tag.
///
/// As a keyword does, the name of a `BaseModel` member gains `_value`
/// (`json_value`), and so does a name with leading underscores, without
/// them (`_id` becomes `id_value`), which Pydantic would take for a private
/// attribute rather than a field. The wire name stays an alias of the field.
#[must_use]
pub(crate) fn python_field_name(rust_name: &str) -> String {
    let public_name = rust_name.trim_start_matches('_');
    let mut name = python_names::identifier(public_name);
    if BASE_MODEL_MEMBERS.contains(&name.as_str()) || public_name.len() < rust_name.len() {
        name.push_str("_value");
    }
    name
}

/// Render what an absent field holds: nothing when callers must supply it,
/// its Rust `Default` or what its default function returns when it has
/// either, and `None` otherwise. Pydantic copies
/// a list or a map for each model, and a value that it must validate is
/// built for each.
fn python_field_default(field: &FieldDef, context: &PythonContext<'_>) -> Result<String> {
    if field.required {
        return Ok("default=...".to_owned());
    }
    let Some(value) = &field.default else {
        return Ok("default=None".to_owned());
    };
    let rendered = render::python_value(value, &field.ty, context)?;
    Ok(if render::is_python_literal(&field.ty) {
        format!("default={rendered}")
    } else {
        format!("default_factory=lambda: {rendered}")
    })
}

/// Render one Pydantic field with its wire name, default, and constraints.
fn emit_python_field(
    source: &mut String,
    field: &FieldDef,
    context: &PythonContext<'_>,
    indent: &str,
) -> Result<()> {
    render::emit_python_comment_docs(source, field.docs.as_deref(), &format!("{indent}#: "));
    let annotation = if let Some(value) = &field.constraints.literal {
        format!(
            "{}[{}]",
            context.import("Literal"),
            render::python_scalar(value)
        )
    } else {
        render::python_ref(&field.ty, context)?
    };
    let name = python_field_name(&field.rust_name);
    let mut options = vec![python_field_default(field, context)?];
    if let Some(docs) = field.docs.as_deref() {
        options.push(format!("description={}", literal::quoted(docs)));
    }
    options.extend(wire_name_options(&name, &field.wire_name));
    if let Some(value) = field.constraints.min_length {
        options.push(format!("min_length={value}"));
    }
    if let Some(value) = field.constraints.max_length {
        options.push(format!("max_length={value}"));
    }
    if let Some(value) = field.constraints.ge {
        options.push(format!("ge={value}"));
    }
    if let Some(value) = field.constraints.le {
        options.push(format!("le={value}"));
    }
    let _ = writeln!(
        source,
        "{indent}{name}: {annotation} = {}({})",
        context.import("Field"),
        options.join(", ")
    );
    Ok(())
}

/// Render the `Field` options that map a field's Python name to its wire
/// name, or none when they are the same.
///
/// The wire name is a validation and serialization alias rather than an
/// `alias`, because type checkers read `alias` as the name of the
/// constructor's keyword. The model's `populate_by_name` accepts both names.
#[must_use]
fn wire_name_options(name: &str, wire_name: &str) -> Vec<String> {
    if name == wire_name {
        return Vec::new();
    }
    let wire_name = literal::quoted(wire_name);
    vec![
        format!("validation_alias={wire_name}"),
        format!("serialization_alias={wire_name}"),
    ]
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use bimetal_ir::{Manifest, Namespace};
    use serde_json::{Value, json};

    use super::*;
    use crate::fixtures;

    fn definition(name: &str, shape: TypeShape) -> TypeDef {
        TypeDef {
            shape,
            ..fixtures::model(name, Vec::new())
        }
    }

    #[test]
    fn an_absent_field_holds_the_default_of_its_type() {
        let absent = |name: &str, ty: TypeRef, default: Value| FieldDef {
            required: false,
            default: Some(default),
            ..fixtures::typed_field(name, ty)
        };
        let retry = fixtures::model(
            "Retry",
            vec![fixtures::typed_field(
                "attempts",
                TypeRef::Int {
                    signed: false,
                    bits: 32,
                },
            )],
        );
        let settings = fixtures::model(
            "Settings",
            vec![
                absent("share", TypeRef::Float { bits: 64 }, json!(0.5)),
                absent(
                    "tags",
                    TypeRef::List {
                        item: Box::new(TypeRef::String),
                    },
                    json!([]),
                ),
                absent("retry", fixtures::named("Retry"), json!({"attempts": 3})),
                absent(
                    "fallback",
                    TypeRef::Option {
                        item: Box::new(TypeRef::String),
                    },
                    json!("dead-letter"),
                ),
            ],
        );
        let manifest = Manifest {
            types: vec![retry, settings],
            ..fixtures::manifest()
        };
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let imports = RefCell::default();
        let context = PythonContext::new(&contract, "test", &namespace, &imports);
        let source =
            python_models(&contract.namespaces()[&namespace], &context).expect("render the models");

        // Pydantic copies a literal; a model is validated for each model.
        assert!(source.contains("    share: float = Field(default=0.5)\n"));
        assert!(source.contains("    tags: list[str] = Field(default=[])\n"));
        assert!(source.contains(
            "    retry: Retry = Field(default_factory=lambda: _bimetal_type_adapter(Retry).validate_python({\"attempts\": 3}, strict=False))\n"
        ));
        // A default function can give an `Option` a value other than `None`.
        assert!(source.contains("    fallback: str | None = Field(default=\"dead-letter\")\n"));
    }

    #[test]
    fn fields_named_like_builtins_or_model_members_stay_usable() {
        let list = |item: TypeRef| TypeRef::List {
            item: Box::new(item),
        };
        let int = || TypeRef::Int {
            signed: true,
            bits: 32,
        };
        let listing = definition(
            "Listing",
            TypeShape::Struct {
                fields: vec![
                    fixtures::typed_field("list", list(TypeRef::String)),
                    fixtures::typed_field("counts", list(int())),
                    fixtures::typed_field("json", TypeRef::String),
                    fixtures::typed_field("model_dump", TypeRef::String),
                    fixtures::typed_field("model_name", TypeRef::String),
                ],
            },
        );
        let plain = definition(
            "Plain",
            TypeShape::Struct {
                fields: vec![fixtures::typed_field("names", list(TypeRef::String))],
            },
        );
        let manifest = Manifest {
            types: vec![listing, plain],
            ..fixtures::manifest()
        };
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let imports = RefCell::default();
        let context = PythonContext::new(&contract, "test", &namespace, &imports);

        let source =
            python_models(&contract.namespaces()[&namespace], &context).expect("render the models");

        // Only a module whose code reaches a hidden builtin imports
        // `builtins`, beside what the models use.
        assert!(source.starts_with(
            "from __future__ import annotations\n\nimport builtins as _bimetal_builtins\n\nfrom pydantic import BaseModel, ConfigDict, Field\n\n\nclass Listing(BaseModel):\n"
        ));
        // The field `list` hides the builtin in its class, before and after.
        assert!(source.contains("    list: _bimetal_builtins.list[str] = Field(default=...)\n"));
        assert!(source.contains("    counts: _bimetal_builtins.list[int] = Field(default=...)\n"));
        // `BaseModel` members are renamed and keep their wire names.
        assert!(source.contains("    json_value: str = Field(default=..., validation_alias=\"json\", serialization_alias=\"json\")\n"));
        assert!(source.contains("    model_dump_value: str = Field(default=..., validation_alias=\"model_dump\", serialization_alias=\"model_dump\")\n"));
        assert!(source.contains("    model_name: str = Field(default=...)\n"));
        // Other classes keep the builtin.
        assert!(source.contains("    names: list[str] = Field(default=...)\n"));
    }

    #[test]
    fn externally_tagged_enums_are_a_model_per_variant_and_a_union_by_wire_name() {
        let manifest = Manifest {
            types: vec![fixtures::external_enum()],
            ..fixtures::manifest()
        };
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let imports = RefCell::default();
        let context = PythonContext::new(&contract, "test", &namespace, &imports);

        let source =
            python_models(&contract.namespaces()[&namespace], &context).expect("render the models");

        assert!(source.contains("from test._runtime import (\n    ExternalVariant as _bimetal_ExternalVariant,\n    external_discriminator as _bimetal_external_discriminator,\n    variant_tag as _bimetal_variant_tag,\n)\n"));
        assert!(source.contains("class ShapeEmpty(_bimetal_ExternalVariant):\n    __bimetal_variant__ = \"empty\"\n    __bimetal_form__ = \"unit\"\n    model_config = ConfigDict("));
        assert!(
            source.contains(
                "    __bimetal_variant__ = \"label\"\n    __bimetal_form__ = \"value\"\n"
            )
        );
        assert!(source.contains("    value: str = Field(default=...)\n"));
        assert!(source.contains("class ShapeCircle(_bimetal_ExternalVariant):\n    \"\"\"\n    A circle.\n    \"\"\"\n\n    __bimetal_variant__ = \"circle\"\n    __bimetal_form__ = \"fields\"\n"));
        assert!(source.contains("    radius: float = Field(default=...)\n"));
        assert!(source.contains("Shape: TypeAlias = Annotated[\n    Annotated[ShapeEmpty, _bimetal_variant_tag(\"empty\")]\n    | Annotated[ShapeLabel, _bimetal_variant_tag(\"label\")]\n    | Annotated[ShapeCircle, _bimetal_variant_tag(\"circle\")],\n    _bimetal_external_discriminator,\n]\n"));
        assert!(source.contains(
            "ShapeEmpty.model_rebuild()\nShapeLabel.model_rebuild()\nShapeCircle.model_rebuild()\n"
        ));
    }

    #[test]
    fn aliases_are_emitted_after_their_local_dependencies() {
        let target = definition("ZTarget", TypeShape::Struct { fields: Vec::new() });
        let alias = definition(
            "AAlias",
            TypeShape::Alias {
                target: TypeRef::Named {
                    identity: target.identity(),
                },
            },
        );
        let items = NamespaceItems {
            types: vec![&alias, &target],
            ..NamespaceItems::default()
        };

        let ordered = ordered_python_types(&items).expect("order aliases");
        assert_eq!(
            ordered
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            ["ZTarget", "AAlias"]
        );
    }

    #[test]
    fn field_documentation_is_preserved_as_pydantic_schema_metadata() {
        let manifest = fixtures::manifest();
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let imports = RefCell::default();
        let context = PythonContext::new(&contract, "test", &namespace, &imports);
        let field = FieldDef {
            docs: Some("The \"size\" in bytes.\nMust be positive.".to_owned()),
            ..fixtures::field("byte_size", "byte_size")
        };
        let mut source = String::new();

        emit_python_field(&mut source, &field, &context, "    ").expect("render field");

        assert!(source.contains(r#"description="The \"size\" in bytes.\nMust be positive.""#));
    }

    #[test]
    fn wire_names_are_aliases_that_keep_the_python_keyword() {
        let manifest = fixtures::manifest();
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let imports = RefCell::default();
        let context = PythonContext::new(&contract, "test", &namespace, &imports);
        let mut source = String::new();

        for field in [
            FieldDef {
                ty: TypeRef::DateTime,
                ..fixtures::field("created_at", "createdAt")
            },
            fixtures::field("count", "count"),
            // The Python name differs from both the Rust and the wire name.
            fixtures::field("class", "class"),
            // Pydantic would take `_id` for a private attribute.
            fixtures::field("_id", "_id"),
        ] {
            emit_python_field(&mut source, &field, &context, "    ").expect("render field");
        }

        // `alias` would rename the constructor's keyword for type checkers.
        assert_eq!(
            source,
            "    created_at: AwareDatetime = Field(default=..., validation_alias=\"createdAt\", serialization_alias=\"createdAt\")\n    count: str = Field(default=...)\n    class_value: str = Field(default=..., validation_alias=\"class\", serialization_alias=\"class\")\n    id_value: str = Field(default=..., validation_alias=\"_id\", serialization_alias=\"_id\")\n"
        );
    }
}
