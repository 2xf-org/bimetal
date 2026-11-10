//! Rendering of TypeScript interfaces, unions, enums, and aliases.
//!
//! Models are declarations, except that a string enum also has a frozen
//! object of its values in the JavaScript. Runtime restoration schemas and
//! callable wrappers belong to `api` and `render`.

use std::fmt::Write;

use anyhow::Result;
use bimetal_ir::{FieldDef, ScalarValue, TypeDef, TypeRef, TypeShape, VariantPayload};

use crate::contract::{self, NamespaceItems};
use crate::literal;

use super::context::TypeScriptContext;
use super::module::Module;
use super::package::MODELS_MODULE;
use super::render::{
    self, DISPOSABLE_LIBRARY, JSON_VALUE, RECORD_BATCH_SOURCE, RECORD_BATCH_STREAM,
};

/// Render all model declarations for one TypeScript namespace.
pub(super) fn typescript_models(
    items: &NamespaceItems<'_>,
    context: &TypeScriptContext<'_>,
) -> Result<Module> {
    let mut module = Module::default();
    // The namespace's API imports this type from here too.
    let uses_json = contract::namespace_refs(items)
        .into_iter()
        .flat_map(contract::nodes)
        .any(|reference| matches!(reference, TypeRef::Json));
    if uses_json {
        let _ = writeln!(
            module.declarations,
            "\nexport type {JSON_VALUE} = null | boolean | number | bigint | string | ReadonlyArray<{JSON_VALUE}> | {{ readonly [key: string]: {JSON_VALUE} }};"
        );
    }
    let uses_streams = contract::namespace_refs(items)
        .into_iter()
        .any(contract::holds_stream);
    if uses_streams {
        emit_stream_types(&mut module.declarations, context);
    }
    for definition in &items.types {
        emit_typescript_type(&mut module, definition, context)?;
    }
    let mut declarations = context.declarations(MODELS_MODULE, &module.declarations);
    // A stream declares `[Symbol.asyncDispose]`.
    if uses_streams {
        declarations = format!("{DISPOSABLE_LIBRARY}{declarations}");
    }
    Ok(Module {
        javascript: context.javascript(&module.javascript),
        declarations,
    })
}

/// Declare the type of a returned stream, and of what a stream parameter
/// reads, which the Arrow adapters implement.
fn emit_stream_types(declarations: &mut String, context: &TypeScriptContext<'_>) {
    let arrow = context.apache_arrow();
    let _ = write!(
        declarations,
        "\n/**\n * Record batches that Rust makes as they are read.\n */\nexport interface {RECORD_BATCH_STREAM} extends AsyncIterableIterator<{arrow}.RecordBatch> {{\n  /**\n   * The schema of every batch.\n   */\n  readonly schema: {arrow}.Schema;\n  /**\n   * Drop the stream in Rust, which stops its work.\n   */\n  close(): void;\n  [Symbol.asyncDispose](): Promise<void>;\n}}\n\n/**\n * What a stream parameter reads, one batch at a time: a `Table`, an apache-arrow\n * `RecordBatchReader`, a `{RECORD_BATCH_STREAM}`, or any iterable of record batches.\n * An iterable without a `schema` must hold at least one batch.\n */\nexport type {RECORD_BATCH_SOURCE} = {arrow}.Table | AsyncIterable<{arrow}.RecordBatch> | Iterable<{arrow}.RecordBatch>;\n"
    );
}

/// Render one contract type as an interface, union, enum object, or alias.
fn emit_typescript_type(
    module: &mut Module,
    definition: &TypeDef,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    let declarations = &mut module.declarations;
    match &definition.shape {
        TypeShape::Struct { fields } => {
            render::emit_typescript_doc(declarations, definition.docs.as_deref(), "");
            let _ = writeln!(declarations, "export interface {} {{", definition.name);
            for field in fields {
                emit_field(declarations, field, context, "  ")?;
            }
            declarations.push_str("}\n");
        }
        TypeShape::StringEnum { variants } => {
            render::emit_typescript_doc(declarations, definition.docs.as_deref(), "");
            let union = variants
                .iter()
                .map(|variant| literal::quoted(&variant.wire_name))
                .collect::<Vec<_>>()
                .join(" | ");
            let _ = writeln!(
                declarations,
                "export type {} = {};\nexport declare const {}: {{",
                definition.name,
                if union.is_empty() { "never" } else { &union },
                definition.name
            );
            // A model of this module may be named `Object`.
            let _ = writeln!(
                module.javascript,
                "export const {} = globalThis.Object.freeze({{",
                definition.name
            );
            for variant in variants {
                render::emit_typescript_doc(declarations, variant.docs.as_deref(), "  ");
                let value = literal::quoted(&variant.wire_name);
                let _ = writeln!(
                    declarations,
                    "  readonly {}: {value};",
                    render::typescript_property(&variant.rust_name)
                );
                let _ = writeln!(
                    module.javascript,
                    "  {}: {value},",
                    render::javascript_property(&variant.rust_name)
                );
            }
            declarations.push_str("};\n");
            module.javascript.push_str("});\n");
        }
        TypeShape::TaggedEnum { tag, variants } => {
            for variant in variants {
                let name = contract::tagged_variant_name(&definition.name, &variant.rust_name);
                render::emit_typescript_doc(declarations, variant.docs.as_deref(), "");
                let _ = writeln!(
                    declarations,
                    "export interface {name} {{\n  readonly {}: {};",
                    render::typescript_property(tag),
                    literal::quoted(&variant.wire_name)
                );
                for field in &variant.fields {
                    emit_field(declarations, field, context, "  ")?;
                }
                declarations.push_str("}\n");
            }
            render::emit_typescript_doc(declarations, definition.docs.as_deref(), "");
            let _ = writeln!(
                declarations,
                "export type {} = {};",
                definition.name,
                variants
                    .iter()
                    .map(|variant| contract::tagged_variant_name(
                        &definition.name,
                        &variant.rust_name
                    ))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
        TypeShape::ExternalEnum { variants } => {
            // Rust reads one variant per object, so each interface rules out
            // the keys of the others.
            let keys: Vec<&str> = variants
                .iter()
                .filter(|variant| variant.payload != VariantPayload::Unit)
                .map(|variant| variant.wire_name.as_str())
                .collect();
            for variant in variants {
                let name = contract::tagged_variant_name(&definition.name, &variant.rust_name);
                let key = render::typescript_property(&variant.wire_name);
                let others: String = keys
                    .iter()
                    .filter(|other| **other != variant.wire_name)
                    .map(|other| {
                        format!(
                            "  readonly {}?: never;\n",
                            render::typescript_property(other)
                        )
                    })
                    .collect();
                render::emit_typescript_doc(declarations, variant.docs.as_deref(), "");
                match &variant.payload {
                    VariantPayload::Unit => {
                        let _ = writeln!(
                            declarations,
                            "export type {name} = {};",
                            literal::quoted(&variant.wire_name)
                        );
                    }
                    VariantPayload::Value { ty } => {
                        let _ = writeln!(
                            declarations,
                            "export interface {name} {{\n  readonly {key}: {};\n{others}}}",
                            render::type_ref(ty, context)?
                        );
                    }
                    VariantPayload::Fields { fields } => {
                        let _ = writeln!(
                            declarations,
                            "export interface {name} {{\n  readonly {key}: {{"
                        );
                        for field in fields {
                            emit_field(declarations, field, context, "    ")?;
                        }
                        let _ = writeln!(declarations, "  }};\n{others}}}");
                    }
                }
            }
            render::emit_typescript_doc(declarations, definition.docs.as_deref(), "");
            let _ = writeln!(
                declarations,
                "export type {} = {};",
                definition.name,
                variants
                    .iter()
                    .map(|variant| contract::tagged_variant_name(
                        &definition.name,
                        &variant.rust_name
                    ))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
        TypeShape::Alias { target } => {
            render::emit_typescript_doc(declarations, definition.docs.as_deref(), "");
            let _ = writeln!(
                declarations,
                "export type {} = {};",
                definition.name,
                render::type_ref(target, context)?
            );
        }
    }
    Ok(())
}

/// Declare one field of a model interface, indented by `indent`. A field with a `literal` rule has
/// that value as its type, as `Literal[...]` gives it in Python.
fn emit_field(
    declarations: &mut String,
    field: &FieldDef,
    context: &TypeScriptContext<'_>,
    indent: &str,
) -> Result<()> {
    render::emit_typescript_doc(declarations, field.docs.as_deref(), indent);
    let ty = match &field.constraints.literal {
        Some(ScalarValue::Bool(value)) => value.to_string(),
        // A 64-bit integer is a bigint.
        Some(ScalarValue::I64(value)) if matches!(field.ty, TypeRef::Int { bits: 64, .. }) => {
            format!("{value}n")
        }
        Some(ScalarValue::I64(value)) => value.to_string(),
        Some(ScalarValue::String(value)) => literal::quoted(value),
        None => render::type_ref(&field.ty, context)?,
    };
    let _ = writeln!(
        declarations,
        "{indent}readonly {}{}: {ty};",
        render::typescript_property(&field.wire_name),
        if field.required { "" } else { "?" },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use bimetal_ir::{Manifest, Namespace};

    use super::*;
    use crate::fixtures;

    #[test]
    fn namespaces_without_models_still_declare_a_module() {
        let manifest = fixtures::manifest();
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let context = TypeScriptContext::new(&contract, &namespace);

        let models =
            typescript_models(&NamespaceItems::default(), &context).expect("render models");

        assert_eq!(models.javascript, "export {};\n");
        assert_eq!(models.declarations, "export {};\n");
    }

    #[test]
    fn externally_tagged_enums_are_a_union_of_names_and_one_key_objects() {
        let manifest = Manifest {
            types: vec![fixtures::external_enum()],
            ..fixtures::manifest()
        };
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let context = TypeScriptContext::new(&contract, &namespace);

        let models =
            typescript_models(&contract.namespaces()[&namespace], &context).expect("render models");

        assert!(models.declarations.contains(
            "export type ShapeEmpty = \"empty\";\nexport interface ShapeLabel {\n  readonly label: string;\n  readonly circle?: never;\n}\n/**\n * A circle.\n */\nexport interface ShapeCircle {\n  readonly circle: {\n    readonly radius: number;\n  };\n  readonly label?: never;\n}\nexport type Shape = ShapeEmpty | ShapeLabel | ShapeCircle;\n"
        ));
    }

    #[test]
    fn literal_fields_have_their_value_as_type() {
        let manifest = fixtures::manifest();
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let context = TypeScriptContext::new(&contract, &namespace);
        let field = |name: &str, ty: TypeRef, literal: ScalarValue| {
            let mut field = fixtures::typed_field(name, ty);
            field.constraints.literal = Some(literal);
            field
        };
        let mut declarations = String::new();

        for field in [
            field(
                "version",
                TypeRef::String,
                ScalarValue::String("v1".to_owned()),
            ),
            field(
                "limit",
                TypeRef::Int {
                    signed: false,
                    bits: 64,
                },
                ScalarValue::I64(5),
            ),
            field(
                "level",
                TypeRef::Int {
                    signed: true,
                    bits: 32,
                },
                ScalarValue::I64(-1),
            ),
            field("enabled", TypeRef::Bool, ScalarValue::Bool(true)),
        ] {
            emit_field(&mut declarations, &field, &context, "  ").expect("render field");
        }

        assert_eq!(
            declarations,
            "  readonly version: \"v1\";\n  readonly limit: 5n;\n  readonly level: -1;\n  readonly enabled: true;\n"
        );
    }
}
