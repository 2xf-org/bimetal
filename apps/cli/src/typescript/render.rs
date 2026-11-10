//! TypeScript syntax primitives shared by declarations and API generation.
//!
//! Type spelling, runtime schemas, JSON literals, identifier quoting, and
//! documentation comments are centralized here so all generated modules
//! follow one deterministic policy.

use std::fmt::Write;

use anyhow::{Context, Result, bail};
use bimetal_ir::typescript_names;
use bimetal_ir::{
    ArrowKind, DefinitionId, FieldConstraints, FieldDef, ParamDef, TypeDef, TypeRef, TypeShape,
    VariantPayload,
};
use serde_json::Value;

use crate::contract::{self, Contract};
use crate::documentation::{self, CallableDocumentation};
use crate::literal;

use super::context::TypeScriptContext;

/// The type of untyped JSON values, which a namespace's models declare when
/// the namespace uses them.
pub(crate) const JSON_VALUE: &str = "JsonValue";

/// The types of a returned stream and of what a stream parameter reads,
/// which a namespace's models declare when the namespace uses streams.
pub(crate) const RECORD_BATCH_STREAM: &str = "RecordBatchStream";
pub(crate) const RECORD_BATCH_SOURCE: &str = "RecordBatchSource";

/// The reference that a module's declarations start with when they name
/// `[Symbol.dispose]` or `[Symbol.asyncDispose]`, which the ES2022 and DOM
/// libraries of a browser project lack.
pub(super) const DISPOSABLE_LIBRARY: &str = "/// <reference lib=\"esnext.disposable\" />\n";

/// Render a contract type in the current TypeScript namespace.
pub(super) fn type_ref(reference: &TypeRef, context: &TypeScriptContext<'_>) -> Result<String> {
    Ok(match reference {
        TypeRef::Unit => "null".to_owned(),
        TypeRef::Bool => "boolean".to_owned(),
        TypeRef::Int { bits: 64, .. } => "bigint".to_owned(),
        TypeRef::Int { .. } | TypeRef::Float { .. } => "number".to_owned(),
        TypeRef::String | TypeRef::DateTime | TypeRef::Date => "string".to_owned(),
        TypeRef::Duration => "bigint".to_owned(),
        TypeRef::Json => context.model(JSON_VALUE),
        TypeRef::Option { item } => format!("{} | null", type_ref(item, context)?),
        TypeRef::List { item } => format!("ReadonlyArray<{}>", type_ref(item, context)?),
        // An index signature cannot collide with a model named `Record`.
        TypeRef::Map { value } => {
            format!(
                "{{ readonly [key: string]: {} }}",
                type_ref(value, context)?
            )
        }
        TypeRef::Tuple { items } => format!(
            "readonly [{}]",
            items
                .iter()
                .map(|item| type_ref(item, context))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        TypeRef::Named { identity } => typescript_named_ref(identity, context)?,
        TypeRef::Bytes | TypeRef::FixedBytes { .. } => "Uint8Array".to_owned(),
        TypeRef::Buffer { element } => element.typed_array().to_owned(),
        TypeRef::Resource { identity } => typescript_resource_ref(identity, context)?,
        TypeRef::Callback { signature } => {
            let (returns, params) = signature
                .split_last()
                .expect("a callback's signature ends with its return");
            let params = params
                .iter()
                .enumerate()
                .map(|(index, param)| Ok(format!("arg{index}: {}", type_ref(param, context)?)))
                .collect::<Result<Vec<_>>>()?;
            format!(
                "(({}) => {})",
                params.join(", "),
                return_type_ref(returns, context)?
            )
        }
        TypeRef::Arrow {
            value: ArrowKind::Stream,
        } => context.model(RECORD_BATCH_STREAM),
        TypeRef::Arrow { value } => format!(
            "{}.{}",
            context.apache_arrow(),
            contract::arrow_spelling(*value).typescript
        ),
    })
}

/// Render the type of a parameter, which for a stream is what it reads.
fn param_type_ref(reference: &TypeRef, context: &TypeScriptContext<'_>) -> Result<String> {
    if *reference == contract::STREAM {
        return Ok(context.model(RECORD_BATCH_SOURCE));
    }
    type_ref(reference, context)
}

/// Render a callable return type, spelling contract unit as `void`.
pub(super) fn return_type_ref(
    reference: &TypeRef,
    context: &TypeScriptContext<'_>,
) -> Result<String> {
    if matches!(reference, TypeRef::Unit) {
        return Ok("void".to_owned());
    }
    type_ref(reference, context)
}

/// Render a local or namespace-qualified named model reference.
pub(super) fn typescript_named_ref(
    identity: &DefinitionId,
    context: &TypeScriptContext<'_>,
) -> Result<String> {
    let definition = context.contract.type_definition(identity)?;
    let namespace = context
        .contract
        .namespace(&definition.owner, &definition.rust_module);
    if namespace == *context.namespace {
        Ok(context.model(&definition.name))
    } else {
        Ok(format!(
            "{}.{}",
            context.models_namespace(&namespace),
            definition.name
        ))
    }
}

/// Render a local or namespace-qualified generated resource class reference.
pub(super) fn typescript_resource_ref(
    identity: &DefinitionId,
    context: &TypeScriptContext<'_>,
) -> Result<String> {
    let definition = context.contract.resource_definition(identity)?;
    let namespace = context
        .contract
        .namespace(&definition.owner, &definition.rust_module);
    if namespace == *context.namespace {
        Ok(definition.name.clone())
    } else {
        Ok(format!(
            "{}.{}",
            context.errors_namespace(&namespace),
            definition.name
        ))
    }
}

/// Render a local or namespace-qualified generated error reference.
pub(super) fn typescript_error_ref(
    identity: &DefinitionId,
    context: &TypeScriptContext<'_>,
) -> Result<String> {
    let definition = context.contract.error_definition(identity)?;
    let namespace = context
        .contract
        .namespace(&definition.owner, &definition.rust_module);
    if namespace == *context.namespace {
        Ok(definition.name.clone())
    } else {
        Ok(format!(
            "{}.{}",
            context.errors_namespace(&namespace),
            definition.name
        ))
    }
}

/// Render a comma-separated TypeScript parameter list. A caller may omit a
/// parameter with a default, or an option with nothing required after it.
pub(super) fn typescript_params(
    params: &[ParamDef],
    context: &TypeScriptContext<'_>,
) -> Result<String> {
    let omissible = contract::omissible_params(params);
    params
        .iter()
        .zip(omissible)
        .map(|(param, omissible)| {
            let mark = if omissible || param.default.is_some() {
                "?"
            } else {
                ""
            };
            Ok(format!(
                "{}{mark}: {}",
                parameter_name(param),
                param_type_ref(&param.ty, context)?
            ))
        })
        .collect::<Result<Vec<_>>>()
        .map(|items| items.join(", "))
}

/// Return the name of a parameter in the generated JavaScript, its
/// declarations, and its documentation; see
/// [`typescript_names::parameter_name`].
///
/// No other generated binding can collide with it: those start with `$`.
#[must_use]
pub(crate) fn parameter_name(param: &ParamDef) -> String {
    typescript_names::parameter_name(&param.host_name).into_owned()
}

/// Render a schema of [`contract::spec`] or [`contract::named_spec`] as
/// JavaScript. Its objects have no prototype, so the runtime's lookup of a
/// key such as `constructor` or `toString` finds no spec rather than a
/// member of `Object.prototype`.
#[must_use]
pub(super) fn spec_literal(spec: &Value) -> String {
    match spec {
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(spec_literal)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(entries) => {
            let entries: String = entries
                .iter()
                .map(|(key, spec)| {
                    format!(", {}: {}", javascript_property(key), spec_literal(spec))
                })
                .collect();
            format!("{{__proto__: null{entries}}}")
        }
        // Schemas hold no numbers or Booleans, only strings and nulls.
        other => other.to_string(),
    }
}

/// Render a JSON value according to its declared contract type.
pub(super) fn typescript_value(
    value: &Value,
    reference: &TypeRef,
    contract: &Contract<'_>,
) -> Result<String> {
    if value.is_null() {
        return Ok("null".to_owned());
    }
    Ok(match reference {
        TypeRef::Int { bits: 64, .. } => format!(
            "{}n",
            value
                .as_u64()
                .map(|item| item.to_string())
                .or_else(|| value.as_i64().map(|item| item.to_string()))
                .context("invalid 64-bit constant")?
        ),
        TypeRef::Option { item } => typescript_value(value, item, contract)?,
        TypeRef::List { item } => format!(
            "[{}]",
            value
                .as_array()
                .context("invalid list constant")?
                .iter()
                .map(|value| typescript_value(value, item, contract))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        TypeRef::Map { value: item } => format!(
            "{{{}}}",
            value
                .as_object()
                .context("invalid map constant")?
                .iter()
                .map(|(key, value)| Ok(format!(
                    "{}: {}",
                    javascript_property(key),
                    typescript_value(value, item, contract)?
                )))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        TypeRef::Tuple { items } => format!(
            "[{}]",
            value
                .as_array()
                .context("invalid tuple constant")?
                .iter()
                .zip(items)
                .map(|(value, item)| typescript_value(value, item, contract))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        TypeRef::Named { identity } => {
            let definition = contract.type_definition(identity)?;
            typescript_named_value(value, definition, contract)?
        }
        TypeRef::Json => typescript_json_value(value),
        // A default is the nanoseconds a host passes, and a constant Serde's
        // `{"secs": ..., "nanos": ...}`, which the runtime restores.
        TypeRef::Duration => match value.as_u64() {
            Some(nanoseconds) => format!("{nanoseconds}n"),
            None => {
                let part = |name: &str| {
                    value
                        .get(name)
                        .and_then(Value::as_u64)
                        .with_context(|| format!("a duration constant has no `{name}`"))
                };
                format!("{{secs: {}n, nanos: {}}}", part("secs")?, part("nanos")?)
            }
        },
        _ => value.to_string(),
    })
}

/// Render an untyped JSON value, preserving integers outside JS's safe range.
#[must_use]
fn typescript_json_value(value: &Value) -> String {
    match value {
        Value::Number(number) => {
            const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
            if let Some(value) = number.as_u64()
                && value > MAX_SAFE_INTEGER
            {
                format!("{value}n")
            } else if let Some(value) = number.as_i64()
                && value.unsigned_abs() > MAX_SAFE_INTEGER
            {
                format!("{value}n")
            } else {
                number.to_string()
            }
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(typescript_json_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(items) => format!(
            "{{{}}}",
            items
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    javascript_property(key),
                    typescript_json_value(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Null | Value::Bool(_) | Value::String(_) => value.to_string(),
    }
}

/// Render a value through the shape of a resolved named definition.
fn typescript_named_value(
    value: &Value,
    definition: &TypeDef,
    contract: &Contract<'_>,
) -> Result<String> {
    match &definition.shape {
        TypeShape::Alias { target } => typescript_value(value, target, contract),
        TypeShape::StringEnum { .. } => Ok(value.to_string()),
        TypeShape::Struct { fields } => typescript_object_value(value, fields, contract),
        TypeShape::TaggedEnum { tag, variants } => {
            let object = value.as_object().context("invalid tagged enum constant")?;
            let tag_value = object
                .get(tag)
                .and_then(Value::as_str)
                .context("tagged enum constant has no tag")?;
            let variant = variants
                .iter()
                .find(|variant| variant.wire_name == tag_value)
                .context("unknown tagged enum constant variant")?;
            let mut fields = variant.fields.clone();
            fields.push(FieldDef {
                rust_name: tag.clone(),
                wire_name: tag.clone(),
                docs: None,
                ty: TypeRef::String,
                required: true,
                default: None,
                constraints: FieldConstraints::default(),
            });
            typescript_object_value(value, &fields, contract)
        }
        TypeShape::ExternalEnum { variants } => {
            // A variant without data is its name.
            if value.is_string() {
                return Ok(value.to_string());
            }
            let (name, payload) = value
                .as_object()
                .filter(|object| object.len() == 1)
                .and_then(|object| object.iter().next())
                .context("invalid externally tagged enum constant")?;
            let variant = variants
                .iter()
                .find(|variant| variant.wire_name == *name)
                .context("unknown externally tagged enum constant variant")?;
            let payload = match &variant.payload {
                VariantPayload::Unit => bail!("a constant of unit variant `{name}` is its name"),
                VariantPayload::Value { ty } => typescript_value(payload, ty, contract)?,
                VariantPayload::Fields { fields } => {
                    typescript_object_value(payload, fields, contract)?
                }
            };
            Ok(format!("{{{}: {payload}}}", javascript_property(name)))
        }
    }
}

/// Render an object with deterministic property order and field typing.
fn typescript_object_value(
    value: &Value,
    fields: &[FieldDef],
    contract: &Contract<'_>,
) -> Result<String> {
    let object = value.as_object().context("invalid object constant")?;
    Ok(format!(
        "{{{}}}",
        object
            .iter()
            .map(|(key, value)| {
                let field = fields
                    .iter()
                    .find(|field| field.wire_name == *key)
                    .context("constant has an unknown field")?;
                Ok(format!(
                    "{}: {}",
                    javascript_property(key),
                    typescript_value(value, &field.ty, contract)?
                ))
            })
            .collect::<Result<Vec<_>>>()?
            .join(", ")
    ))
}

/// Emit an escaped, multiline TSDoc comment.
pub(super) fn emit_typescript_doc(source: &mut String, docs: Option<&str>, indent: &str) {
    if let Some(docs) = docs.filter(|docs| !docs.trim().is_empty()) {
        let docs = documentation::reflow(docs);
        emit_typescript_doc_block(
            source,
            &documentation::remove_rustdoc_link_brackets(&docs),
            indent,
        );
    }
}

/// Emit TSDoc for a generated function, constructor, or resource method.
pub(super) fn emit_typescript_callable_doc(
    source: &mut String,
    documented: &CallableDocumentation<'_>,
    context: &TypeScriptContext<'_>,
    indent: &str,
) -> Result<()> {
    let callable = documented.callable;
    let sections = documented.sections();
    let text = |text: &str| typescript_documentation_text(text, context);
    let mut prose_blocks = vec![text(&sections.summary)];
    if !sections.notes.is_empty() {
        prose_blocks.push(format!("@remarks\n{}", text(&sections.notes)));
    }

    // The signature shows every parameter's and the result's type, so only
    // what the Rust docs say about them becomes a tag.
    let mut tags = Vec::new();
    for (param, description) in callable.params.iter().zip(&sections.parameters) {
        if let Some(description) = description {
            tags.push(format!(
                "@param {} - {}",
                parameter_name(param),
                indent_typescript_tag_continuation(&text(description))
            ));
        }
    }
    if !sections.returns.is_empty() {
        tags.push(format!(
            "@returns {}",
            indent_typescript_tag_continuation(&text(&sections.returns))
        ));
    }
    // TypeScript has no checked exceptions, so the tag names the error class
    // even without a description.
    if let Some(error) = &callable.error {
        let error_name = typescript_error_ref(error, context)?;
        let mut tag = format!("@throws {{@link {error_name}}}");
        if !sections.errors.is_empty() {
            let description =
                documentation::contextualize_error_description(&text(&sections.errors));
            let _ = write!(
                tag,
                " - {}",
                indent_typescript_tag_continuation(&description)
            );
        }
        tags.push(tag);
    }
    if !tags.is_empty() {
        prose_blocks.push(tags.join("\n"));
    }
    if !sections.examples.is_empty() {
        prose_blocks.push(format!("@example\n{}", text(&sections.examples)));
    }

    emit_typescript_doc_block(source, &prose_blocks.join("\n\n"), indent);
    Ok(())
}

/// Emit one TSDoc block without ever collapsing its delimiters onto prose,
/// in which a block tag's text continues two columns deeper.
fn emit_typescript_doc_block(source: &mut String, docs: &str, indent: &str) {
    let escaped = docs.replace("*/", "* /");
    let width = 97usize.saturating_sub(indent.len());
    let hanging = |content: &str, _| if content.starts_with('@') { 2 } else { 0 };
    let _ = writeln!(source, "{indent}/**");
    for line in documentation::wrap(&escaped, width, hanging) {
        if line.is_empty() {
            let _ = writeln!(source, "{indent} *");
        } else {
            let _ = writeln!(source, "{indent} * {line}");
        }
    }
    let _ = writeln!(source, "{indent} */");
}

/// Translate inline Rust callable links to the names exposed by TypeScript.
#[must_use]
pub(super) fn typescript_documentation_text(
    value: &str,
    context: &TypeScriptContext<'_>,
) -> String {
    let mut rendered = documentation::remove_rustdoc_link_brackets(value);
    let callables = context.contract.manifest.callables();
    for callable in callables {
        rendered = rendered.replace(
            &format!("`{}`", callable.rust_name),
            &format!("`{}`", callable.host_name),
        );
    }
    rendered
}

/// Keep authored continuation lines visually inside their TSDoc block tag.
#[must_use]
fn indent_typescript_tag_continuation(value: &str) -> String {
    value.replace('\n', "\n  ")
}

/// Render an object property as an identifier or quoted key.
#[must_use]
pub(super) fn typescript_property(value: &str) -> String {
    if literal::is_identifier(value) {
        value.to_owned()
    } else {
        literal::quoted(value)
    }
}

/// Render the key of a property in a JavaScript object literal. A literal's
/// `__proto__: value`, quoted or not, sets the object's prototype, so that
/// key is computed, which defines an own property instead.
#[must_use]
pub(super) fn javascript_property(value: &str) -> String {
    if value == "__proto__" {
        format!("[{}]", literal::quoted(value))
    } else {
        typescript_property(value)
    }
}

#[cfg(test)]
mod tests {
    use bimetal_ir::{CallableDef, EnumVariantDef, Namespace};

    use super::*;
    use crate::documentation::CallableReturn;
    use crate::fixtures;

    #[test]
    fn wide_json_integer_constants_render_as_bigints() {
        assert_eq!(
            typescript_json_value(&serde_json::json!(u64::MAX)),
            format!("{}n", u64::MAX)
        );
        assert_eq!(
            typescript_json_value(&serde_json::json!({"id": u64::MAX})),
            format!("{{id: {}n}}", u64::MAX)
        );
    }

    #[test]
    fn parameters_that_modules_reserve_get_a_trailing_underscore() {
        let param = |name: &str| fixtures::param(name, name);

        for reserved in [
            "package",
            "default",
            "new",
            "class",
            "public",
            "private",
            "interface",
            "function",
            "delete",
            "arguments",
            "eval",
            "await",
            "yield",
            "let",
            "static",
            "enum",
            "implements",
            "protected",
            "this",
        ] {
            assert_eq!(parameter_name(&param(reserved)), format!("{reserved}_"));
        }
        for allowed in [
            "value",
            "native",
            "type",
            "async",
            "undefined",
            "constructor",
        ] {
            assert_eq!(parameter_name(&param(allowed)), allowed);
        }
    }

    #[test]
    fn object_literals_define_a_proto_key_as_an_own_property() {
        let manifest = fixtures::manifest();
        let contract = fixtures::contract(&manifest);
        let map = TypeRef::Map {
            value: Box::new(TypeRef::Json),
        };

        assert_eq!(javascript_property("__proto__"), "[\"__proto__\"]");
        assert_eq!(javascript_property("label"), "label");
        assert_eq!(javascript_property("a-b"), "\"a-b\"");
        assert_eq!(typescript_property("__proto__"), "__proto__");
        assert_eq!(
            typescript_value(
                &serde_json::json!({"__proto__": {"__proto__": null}}),
                &map,
                &contract,
            )
            .expect("render map constant"),
            "{[\"__proto__\"]: {[\"__proto__\"]: null}}"
        );
    }

    #[test]
    fn tsdoc_wraps_each_paragraph_once() {
        let mut source = String::new();
        emit_typescript_doc(
            &mut source,
            Some(
                "A table of rows, which a plan reads and a writer fills, one batch at a time, as the\nreader asks for each.\n\nRows keep their order.",
            ),
            "",
        );

        assert_eq!(
            source,
            "/**\n * A table of rows, which a plan reads and a writer fills, one batch at a time, as the reader asks\n * for each.\n *\n * Rows keep their order.\n */\n"
        );
    }

    #[test]
    fn callable_tsdoc_uses_standard_block_tags() {
        let mut manifest = fixtures::manifest();
        manifest.errors.push(fixtures::error("ExampleError"));
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let context = TypeScriptContext::new(&contract, &namespace);
        let mut callable = CallableDef {
            docs: Some(
                "Process a value.\n\nRetains ordering.\n\n# Arguments\n\n- `value` - Value to process.\n\n# Returns\n\nThe processed value.\n\n# Errors\n\nReturns `ExampleError` when validation fails.".to_owned(),
            ),
            error: Some(DefinitionId::new("test", "test::ExampleError")),
            ..fixtures::callable("process", "process", vec![fixtures::param("value", "value")])
        };
        let document = |callable: &CallableDef| {
            let mut source = String::new();
            emit_typescript_callable_doc(
                &mut source,
                &CallableDocumentation {
                    callable,
                    fallback_summary: "Fallback.".to_owned(),
                    returns: CallableReturn::Value,
                },
                &context,
                "",
            )
            .expect("render callable TSDoc");
            source
        };

        let source = document(&callable);

        assert!(source.starts_with("/**\n * Process a value.\n"));
        assert!(source.contains(" * @remarks\n * Retains ordering."));
        assert!(source.contains(" * @param value - Value to process."));
        assert!(source.contains(" * @returns The processed value."));
        assert!(source.contains(
            " * @throws {@link ExampleError} - The Rust implementation returns `ExampleError` when"
        ));
        assert!(!source.contains("# Errors"));

        // Without Rust docs, nothing repeats what the signature says.
        callable.docs = None;
        assert_eq!(
            document(&callable),
            "/**\n * Fallback.\n *\n * @throws {@link ExampleError}\n */\n"
        );
    }

    #[test]
    fn specs_of_fields_and_variants_have_no_prototype() {
        let field = FieldDef {
            required: false,
            ..fixtures::typed_field("toString", TypeRef::Bytes)
        };
        let definition = TypeDef {
            shape: TypeShape::TaggedEnum {
                tag: "kind".to_owned(),
                variants: vec![EnumVariantDef {
                    rust_name: "Constructor".to_owned(),
                    wire_name: "constructor".to_owned(),
                    docs: None,
                    fields: vec![field],
                }],
            },
            ..fixtures::model("Event", Vec::new())
        };

        assert_eq!(
            spec_literal(&contract::named_spec(&definition)),
            "[\"tagged\", \"kind\", {__proto__: null, constructor: {__proto__: null, toString: [\"bytes\"]}}]"
        );
    }

    #[test]
    fn typescript_docs_use_public_callable_names() {
        let mut manifest = fixtures::manifest();
        manifest
            .functions
            .push(fixtures::function("load_value", "loadValue", Vec::new()));
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let context = TypeScriptContext::new(&contract, &namespace);

        assert_eq!(
            typescript_documentation_text("Call [`load_value`] next.", &context),
            "Call `loadValue` next."
        );
    }
}
