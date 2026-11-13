//! Python syntax primitives shared by model and API generation.
//!
//! This module owns all host spelling decisions: type references, literals,
//! declaration spacing, docstrings, and the names of the import package and
//! its modules, which project setup and validation also use. Keeping those
//! decisions here prevents API and model emitters from drifting.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::iter;

use anyhow::{Result, bail};
use bimetal_ir::python_names;
use bimetal_ir::{
    ArrowKind, BufferElement, DefinitionId, Namespace, ParamDef, ScalarValue, TypeRef, TypeShape,
};
use serde_json::Value;

use crate::contract::{self, Contract, NamespaceItems};
use crate::documentation::{self, CallableDocumentation};
use crate::literal;

use super::context::PythonContext;
use super::package::{API_MODULE, MODELS_MODULE};

/// Return the generated Python alias of a numeric buffer of `element`, such
/// as `Float64Buffer`.
#[must_use]
pub(crate) fn python_buffer_alias(element: BufferElement) -> &'static str {
    match element {
        BufferElement::U8 => "UInt8Buffer",
        BufferElement::I8 => "Int8Buffer",
        BufferElement::U16 => "UInt16Buffer",
        BufferElement::I16 => "Int16Buffer",
        BufferElement::U32 => "UInt32Buffer",
        BufferElement::I32 => "Int32Buffer",
        BufferElement::U64 => "UInt64Buffer",
        BufferElement::I64 => "Int64Buffer",
        BufferElement::F32 => "Float32Buffer",
        BufferElement::F64 => "Float64Buffer",
    }
}

/// Render a contract type as a Python annotation in the current namespace.
///
/// A date is an `AwareDatetime`, which type checkers read as `datetime` and
/// Pydantic rejects without a UTC offset, because Rust requires one.
pub(super) fn python_ref(reference: &TypeRef, context: &PythonContext<'_>) -> Result<String> {
    Ok(match reference {
        TypeRef::Unit => "None".to_owned(),
        TypeRef::Bool => context.builtin("bool"),
        TypeRef::Int { .. } => context.builtin("int"),
        TypeRef::Float { .. } => context.builtin("float"),
        TypeRef::String => context.builtin("str"),
        TypeRef::DateTime => context.import("AwareDatetime").to_owned(),
        TypeRef::Date => format!("{}.date", context.import(python_names::DATETIME_ALIAS)),
        TypeRef::Duration => context.runtime("Duration"),
        TypeRef::Json => context.runtime("JsonValue"),
        TypeRef::Option { item } => format!("{} | None", python_ref(item, context)?),
        TypeRef::List { item } => {
            format!(
                "{}[{}]",
                context.builtin("list"),
                python_ref(item, context)?
            )
        }
        TypeRef::Map { value } => format!(
            "{}[{}, {}]",
            context.builtin("dict"),
            context.builtin("str"),
            python_ref(value, context)?
        ),
        // An empty tuple, such as a `[T; 0]`, is `tuple[()]`.
        TypeRef::Tuple { items } if items.is_empty() => {
            format!("{}[()]", context.builtin("tuple"))
        }
        TypeRef::Tuple { items } => format!(
            "{}[{}]",
            context.builtin("tuple"),
            items
                .iter()
                .map(|item| python_ref(item, context))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        TypeRef::Named { identity } => python_named_ref(identity, context)?,
        TypeRef::Bytes | TypeRef::FixedBytes { .. } => context.builtin("bytes"),
        TypeRef::Buffer { element } => context.model(python_buffer_alias(*element)),
        // pyarrow's stubs make `Array` generic in its scalar type.
        TypeRef::Arrow {
            value: ArrowKind::Array,
        } => format!("{}.Array[{}]", context.import("pa"), context.import("Any")),
        TypeRef::Arrow { value } => format!(
            "{}.{}",
            context.import("pa"),
            contract::arrow_spelling(*value).pyarrow
        ),
        TypeRef::Resource { identity } => python_resource_ref(identity, context)?,
        TypeRef::Callback { signature } => {
            let (returns, params) = signature
                .split_last()
                .expect("a callback's signature ends with its return");
            format!(
                "{}[[{}], {}]",
                context.import("Callable"),
                params
                    .iter()
                    .map(|param| python_ref(param, context))
                    .collect::<Result<Vec<_>>>()?
                    .join(", "),
                python_ref(returns, context)?
            )
        }
    })
}

/// Render the annotation passed to Pydantic's runtime adapter.
pub(super) fn python_adapter_type(
    reference: &TypeRef,
    context: &PythonContext<'_>,
) -> Result<String> {
    if matches!(reference, TypeRef::Unit) {
        Ok(format!("{}(None)", context.builtin("type")))
    } else {
        python_ref(reference, context)
    }
}

/// Render a named and annotated Python parameter, with its default when the
/// caller may omit it. An Arrow parameter accepts any producer of the Arrow
/// PyCapsule interface, not only pyarrow's own classes.
pub(super) fn python_param(
    param: &ParamDef,
    name: &str,
    omissible: bool,
    context: &PythonContext<'_>,
) -> Result<String> {
    let annotation = match &param.ty {
        TypeRef::Arrow { value } => context
            .runtime_export(contract::arrow_spelling(*value).protocol)
            .to_owned(),
        TypeRef::Option { item } if matches!(item.as_ref(), TypeRef::Arrow { .. }) => {
            let TypeRef::Arrow { value } = item.as_ref() else {
                unreachable!("the guard checked the item");
            };
            format!(
                "{} | None",
                context.runtime_export(contract::arrow_spelling(*value).protocol)
            )
        }
        reference => python_ref(reference, context)?,
    };
    let default = match &param.default {
        // A unit variant's name is a value of its externally tagged enum,
        // whose union validates it into the variant's model.
        Some(default @ Value::String(_)) if is_external_enum(&param.ty, context.contract)? => {
            format!(
                " = {}({}).validate_python({})",
                context.runtime("type_adapter"),
                python_ref(&param.ty, context)?,
                python_json(default)
            )
        }
        // A string enum's default is its value, which the enum's class reads.
        Some(default @ Value::String(_)) if matches!(param.ty, TypeRef::Named { .. }) => {
            format!(
                " = {}({})",
                python_ref(&param.ty, context)?,
                python_json(default)
            )
        }
        // Serde writes a date as text, which type checkers refuse for a
        // `date` parameter.
        Some(Value::String(text)) if is_date(&param.ty) => {
            format!(" = {}", date_literal(text, context)?)
        }
        Some(default) => format!(" = {}", python_json(default)),
        None if omissible => " = None".to_owned(),
        None => String::new(),
    };
    Ok(format!("{name}: {annotation}{default}"))
}

/// Return whether `reference` is a date, or an optional one.
fn is_date(reference: &TypeRef) -> bool {
    match reference {
        TypeRef::Date => true,
        TypeRef::Option { item } => matches!(item.as_ref(), TypeRef::Date),
        _ => false,
    }
}

/// Render `text`, a date as Serde writes it, as a Python `date`.
fn date_literal(text: &str, context: &PythonContext<'_>) -> Result<String> {
    let mut parts = text.split('-').map(str::parse::<u32>);
    let (Some(Ok(year)), Some(Ok(month)), Some(Ok(day)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("the date default `{text}` is not YYYY-MM-DD");
    };
    Ok(format!(
        "{}.date({year}, {month}, {day})",
        context.import(python_names::DATETIME_ALIAS)
    ))
}

/// Return whether `reference` names an externally tagged enum, through any
/// aliases.
fn is_external_enum(reference: &TypeRef, contract: &Contract<'_>) -> Result<bool> {
    let TypeRef::Named { identity } = reference else {
        return Ok(false);
    };
    match &contract.type_definition(identity)?.shape {
        TypeShape::ExternalEnum { .. } => Ok(true),
        TypeShape::Alias { target } => is_external_enum(target, contract),
        TypeShape::Struct { .. } | TypeShape::StringEnum { .. } | TypeShape::TaggedEnum { .. } => {
            Ok(false)
        }
    }
}

/// Return the Python names of a callable's parameters, in order.
///
/// The extension names the same parameters in its argument errors, so both
/// derive them from the Rust names alone; see
/// [`python_names::parameter_names`].
#[must_use]
pub(crate) fn python_param_names(params: &[ParamDef]) -> Vec<String> {
    python_names::parameter_names(params.iter().map(|param| param.rust_name.as_str()))
}

/// Return the names of the models and buffer types that one namespace's
/// `models.py` declares.
#[must_use]
pub(crate) fn python_model_names(items: &NamespaceItems<'_>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for definition in &items.types {
        names.insert(definition.name.clone());
        names.extend(
            contract::declared_variants(definition)
                .into_iter()
                .map(|variant| contract::tagged_variant_name(&definition.name, variant)),
        );
    }
    names.extend(
        namespace_buffers(items)
            .into_iter()
            .map(|element| python_buffer_alias(element).to_owned()),
    );
    names
}

/// Return the numeric buffer types that one namespace's declarations use
/// directly, which its `models.py` names.
#[must_use]
pub(crate) fn namespace_buffers(items: &NamespaceItems<'_>) -> BTreeSet<BufferElement> {
    contract::namespace_refs(items)
        .into_iter()
        .flat_map(contract::nodes)
        .filter_map(|reference| match reference {
            TypeRef::Buffer { element } => Some(*element),
            _ => None,
        })
        .collect()
}

/// Render a local or namespace-qualified named model reference.
pub(super) fn python_named_ref(
    identity: &DefinitionId,
    context: &PythonContext<'_>,
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
            context.namespace_module(&namespace, MODELS_MODULE),
            definition.name
        ))
    }
}

/// Render a local or namespace-qualified generated resource class reference.
pub(super) fn python_resource_ref(
    identity: &DefinitionId,
    context: &PythonContext<'_>,
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
            context.namespace_module(&namespace, API_MODULE),
            definition.name
        ))
    }
}

/// Name a generated error class as the docs of a callable in this namespace
/// name it: by its name in its own namespace, and by its public path, such as
/// `app.geometry.GeometryError`, in another.
fn python_error_name(identity: &DefinitionId, context: &PythonContext<'_>) -> Result<String> {
    let definition = context.contract.error_definition(identity)?;
    let namespace = context
        .contract
        .namespace(&definition.owner, &definition.rust_module);
    if namespace == *context.namespace {
        Ok(definition.name.clone())
    } else {
        Ok(format!(
            "{}.{}",
            python_namespace_module(context.package, &namespace),
            definition.name
        ))
    }
}

/// Return the import package that an application named `name` defaults to,
/// which spells its hyphens as underscores, such as `my_app` for `my-app`.
#[must_use]
pub(crate) fn python_package_name(name: &str) -> String {
    name.replace('-', "_")
}

/// Return the directory of the import package `package`, relative to the
/// Python project, such as `acme/tools` for `acme.tools`.
#[must_use]
pub(crate) fn python_package_directory(package: &str) -> String {
    package.replace('.', "/")
}

/// Spell the Python module of `namespace` in the import package `package`,
/// such as `my_app.stats`.
#[must_use]
pub(crate) fn python_namespace_module(package: &str, namespace: &Namespace) -> String {
    iter::once(package.to_owned())
        .chain(namespace.python_segments())
        .collect::<Vec<_>>()
        .join(".")
}

/// Separate the next Python top-level declaration with two blank lines.
pub(super) fn begin_python_top_level(source: &mut String) {
    end_with_newlines(source, 3);
}

/// Separate the next Python alias from the preceding declaration.
pub(super) fn begin_python_alias(source: &mut String) {
    end_with_newlines(source, 2);
}

/// End `source`, a module's body, with `count` newlines: the one that ends
/// its last line, then blank lines. An empty body follows the module's
/// imports, whose last line has its newline.
fn end_with_newlines(source: &mut String, count: usize) {
    let present = if source.is_empty() {
        1
    } else {
        source.len() - source.trim_end_matches('\n').len()
    };
    for _ in present..count {
        source.push('\n');
    }
}

/// Render a scalar default or literal constraint as Python source.
#[must_use]
pub(super) fn python_scalar(value: &ScalarValue) -> String {
    match value {
        ScalarValue::Bool(value) => if *value { "True" } else { "False" }.to_owned(),
        ScalarValue::I64(value) => value.to_string(),
        ScalarValue::String(value) => literal::quoted(value),
    }
}

/// Render arbitrary JSON as deterministic Python source.
#[must_use]
pub(super) fn python_json(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(value) => if *value { "True" } else { "False" }.to_owned(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => literal::quoted(value),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_json)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{}: {}", literal::quoted(key), python_json(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Return whether Python's literal of a value that Serde wrote is already
/// the host value, which needs no restoration or validation: a scalar, or a
/// list, a map or an `Option` of such.
#[must_use]
pub(super) fn is_python_literal(reference: &TypeRef) -> bool {
    match reference {
        TypeRef::Option { item } | TypeRef::List { item } => is_python_literal(item),
        TypeRef::Map { value } => is_python_literal(value),
        TypeRef::Unit
        | TypeRef::Bool
        | TypeRef::Int { .. }
        | TypeRef::Float { .. }
        | TypeRef::String
        | TypeRef::Json => true,
        _ => false,
    }
}

/// Render the adapter that validates a value of `reference`.
pub(super) fn python_adapter(reference: &TypeRef, context: &PythonContext<'_>) -> Result<String> {
    let adapter = context.runtime("type_adapter");
    let annotation = python_adapter_type(reference, context)?;
    let buffers = context
        .contract
        .reaches(reference, |item| matches!(item, TypeRef::Buffer { .. }))?;
    let options = if buffers {
        ", arbitrary_types=True"
    } else {
        ""
    };
    Ok(format!("{adapter}({annotation}{options})"))
}

/// Spell the module constant that validates and writes values of
/// `reference`: a runtime `Adapter`, which builds Pydantic's adapter on first
/// use, so a call neither builds the annotation nor looks the adapter up.
pub(super) fn python_adapter_constant(
    reference: &TypeRef,
    context: &PythonContext<'_>,
) -> Result<String> {
    let annotation = python_adapter_type(reference, context)?;
    let buffers = context
        .contract
        .reaches(reference, |item| matches!(item, TypeRef::Buffer { .. }))?;
    let options = if buffers {
        ", arbitrary_types=True"
    } else {
        ""
    };
    let expression = format!(
        "{}(lambda: {annotation}{options})",
        context.runtime("Adapter")
    );
    Ok(context.constant(python_names::ADAPTER_CONSTANT_PREFIX, expression))
}

/// Whether the extension reads an argument of `reference` as Python holds it
/// after validation, with no dump: a model, an enum, a date-time, a date, a
/// duration or bytes, or a container of them. Every other argument is plain,
/// and the extension reads its dump.
pub(super) fn python_reads_natively(reference: &TypeRef, contract: &Contract<'_>) -> Result<bool> {
    Ok(match reference {
        TypeRef::DateTime
        | TypeRef::Date
        | TypeRef::Duration
        | TypeRef::Bytes
        | TypeRef::FixedBytes { .. } => true,
        TypeRef::Option { item } | TypeRef::List { item } | TypeRef::Map { value: item } => {
            python_reads_natively(item, contract)?
        }
        TypeRef::Tuple { items } => {
            let mut any = false;
            for item in items {
                any |= python_reads_natively(item, contract)?;
            }
            any
        }
        TypeRef::Named { identity } => match &contract.type_definition(identity)?.shape {
            TypeShape::Alias { target } => python_reads_natively(target, contract)?,
            _ => true,
        },
        _ => false,
    })
}

/// Render the expression that gives `value`, which Serde wrote, as the host
/// value of `reference`.
pub(super) fn python_value(
    value: &Value,
    reference: &TypeRef,
    context: &PythonContext<'_>,
) -> Result<String> {
    let value = python_json(value);
    if is_python_literal(reference) {
        return Ok(value);
    }
    let restored = python_restored(&value, reference, context)?;
    Ok(format!(
        "{}.validate_python({restored}, strict=False)",
        python_adapter(reference, context)?
    ))
}

/// Render a native value, restored when its type needs it.
///
/// Only bytes and buffers change shape. Pydantic validates everything else as
/// the extension returns it.
pub(super) fn python_restored(
    value: &str,
    reference: &TypeRef,
    context: &PythonContext<'_>,
) -> Result<String> {
    let restores = context.contract.reaches(reference, |item| {
        matches!(
            item,
            TypeRef::Bytes | TypeRef::FixedBytes { .. } | TypeRef::Buffer { .. }
        )
    })?;
    Ok(if restores {
        format!(
            "{}({value}, {})",
            context.runtime("restore_host"),
            python_json(&contract::spec(reference))
        )
    } else {
        value.to_owned()
    })
}

/// Emit a multiline Python docstring with stable wrapping and indentation,
/// unless `docs` is blank, and return whether it emitted one.
pub(super) fn emit_python_doc(source: &mut String, docs: Option<&str>, indent: &str) -> bool {
    let Some(docs) = docs.filter(|docs| !docs.trim().is_empty()) else {
        return false;
    };
    let docs = documentation::reflow(docs);
    emit_python_docstring(
        source,
        &documentation::remove_rustdoc_link_brackets(&docs),
        indent,
    );
    true
}

/// Emit documentation as comments, each line after `prefix`, such as `#: `
/// for the comments that Sphinx reads for the declaration below them.
pub(super) fn emit_python_comment_docs(source: &mut String, docs: Option<&str>, prefix: &str) {
    let Some(docs) = docs else {
        return;
    };
    for line in documentation::remove_rustdoc_link_brackets(docs).lines() {
        let _ = writeln!(source, "{prefix}{line}");
    }
}

/// Emit a Google-style docstring for a generated callable.
///
/// The parameters and the return value appear only as the Rust
/// documentation describes them: the signature already gives their types.
pub(super) fn emit_python_callable_doc(
    source: &mut String,
    documented: &CallableDocumentation<'_>,
    context: &PythonContext<'_>,
    indent: &str,
) -> Result<()> {
    // Prose names builtins plainly, even where code must qualify them.
    let context = &context.prose();
    let callable = documented.callable;
    let sections = documented.sections();
    let text = documentation::remove_rustdoc_link_brackets;
    let mut blocks = vec![text(&sections.summary)];
    if !sections.notes.is_empty() {
        blocks.push(python_doc_section("Notes", &text(&sections.notes)));
    }
    let names = python_param_names(&callable.params);
    let entries: Vec<String> = names
        .iter()
        .zip(&sections.parameters)
        .filter_map(|(name, description)| {
            let description = text(description.as_deref()?);
            Some(format!(
                "    {name}: {}",
                indent_python_continuation(&description)
            ))
        })
        .collect();
    if !entries.is_empty() {
        blocks.push(format!("Args:\n{}", entries.join("\n")));
    }
    if !sections.returns.is_empty() {
        blocks.push(python_doc_section("Returns", &text(&sections.returns)));
    }
    // The entry names the error class even without a description, as the
    // TypeScript `@throws` tag does.
    if let Some(error) = &callable.error {
        let error_name = python_error_name(error, context)?;
        let mut entry = format!("Raises:\n    {error_name}");
        if !sections.errors.is_empty() {
            let description =
                documentation::contextualize_error_description(&text(&sections.errors));
            let _ = write!(entry, ": {}", indent_python_continuation(&description));
        }
        blocks.push(entry);
    }
    if !sections.examples.is_empty() {
        blocks.push(python_doc_section("Examples", &text(&sections.examples)));
    }

    emit_python_docstring(source, &blocks.join("\n\n"), indent);
    Ok(())
}

/// Emit the delimiters and the escaped, wrapped body of a Python docstring,
/// in which a Google-style entry, an indented `name: description`, continues
/// four columns deeper.
fn emit_python_docstring(source: &mut String, docs: &str, indent: &str) {
    let escaped: Vec<String> = docs
        .lines()
        .map(|line| {
            let quoted = literal::quoted(line);
            quoted[1..quoted.len() - 1].to_owned()
        })
        .collect();
    let width = 84usize.saturating_sub(indent.len());
    let hanging = |content: &str, indent| {
        if indent > 0 && documentation::entry_name(content).is_some() {
            4
        } else {
            0
        }
    };
    let _ = writeln!(source, "{indent}\"\"\"");
    for line in documentation::wrap(&escaped.join("\n"), width, hanging) {
        if line.is_empty() {
            source.push('\n');
        } else {
            let _ = writeln!(source, "{indent}{line}");
        }
    }
    let _ = writeln!(source, "{indent}\"\"\"");
}

/// Indent every line in one Google-style documentation section.
#[must_use]
fn python_doc_section(name: &str, value: &str) -> String {
    format!(
        "{name}:\n{}",
        value
            .lines()
            .map(|line| format!("    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Align continuation lines beneath a Google-style entry description.
#[must_use]
fn indent_python_continuation(value: &str) -> String {
    value.replace('\n', "\n        ")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use bimetal_ir::{CallableDef, Manifest};

    use super::*;
    use crate::documentation::CallableReturn;
    use crate::fixtures;

    fn manifest() -> Manifest {
        let mut manifest = fixtures::manifest();
        manifest.errors.push(fixtures::error("ExampleError"));
        manifest
    }

    #[test]
    fn ordinary_docstrings_always_use_multiline_delimiters() {
        let mut source = String::new();
        emit_python_doc(&mut source, Some("Short \"documentation\"."), "    ");

        assert_eq!(
            source,
            "    \"\"\"\n    Short \\\"documentation\\\".\n    \"\"\"\n"
        );
    }

    #[test]
    fn docstrings_wrap_each_paragraph_once() {
        let mut source = String::new();
        emit_python_doc(
            &mut source,
            Some(
                "A table of rows, which a plan reads and a writer fills, one batch at a time\nas they arrive.\n\n- `rows` - The rows.",
            ),
            "    ",
        );

        assert_eq!(
            source,
            "    \"\"\"\n    A table of rows, which a plan reads and a writer fills, one batch at a time as\n    they arrive.\n\n    - `rows` - The rows.\n    \"\"\"\n"
        );
    }

    #[test]
    fn only_an_indented_entry_hangs() {
        let mut source = String::new();
        emit_python_doc(
            &mut source,
            Some(
                "Add up the `n` column, reading one batch at a time: every batch, or the first `limit`.",
            ),
            "    ",
        );

        assert_eq!(
            source,
            "    \"\"\"\n    Add up the `n` column, reading one batch at a time: every batch, or the first\n    `limit`.\n    \"\"\"\n"
        );
    }

    #[test]
    fn blank_docs_emit_nothing_and_comment_docs_drop_link_brackets() {
        let mut source = String::new();

        assert!(!emit_python_doc(&mut source, Some(" \n "), "    "));
        assert!(source.is_empty());
        emit_python_comment_docs(&mut source, Some("A [`Point`].\nMore."), "#: ");
        assert_eq!(source, "#: A `Point`.\n#: More.\n");
    }

    #[test]
    fn callable_docstrings_use_google_sections() {
        let manifest = manifest();
        let namespace = Namespace::root();
        let contract = fixtures::contract(&manifest);
        let imports = RefCell::default();
        let context = PythonContext::new(&contract, "test", &namespace, &imports);
        let mut callable = CallableDef {
            docs: Some(
                "Process a value.\n\nRetains ordering.\n\n# Arguments\n\n- `value` - Value to process.\n\n# Returns\n\nThe processed value.\n\n# Errors\n\nReturns `ExampleError` when validation fails.".to_owned(),
            ),
            error: Some(DefinitionId::new("test", "test::ExampleError")),
            ..fixtures::callable("process", "process", vec![fixtures::param("value", "value")])
        };
        let document = |callable: &CallableDef| {
            let mut source = String::new();
            emit_python_callable_doc(
                &mut source,
                &CallableDocumentation {
                    callable,
                    fallback_summary: "Fallback.".to_owned(),
                    returns: CallableReturn::Value,
                },
                &context,
                "    ",
            )
            .expect("render callable docstring");
            source
        };

        let source = document(&callable);

        assert!(source.starts_with("    \"\"\"\n    Process a value.\n"));
        assert!(source.contains("    Notes:\n        Retains ordering."));
        assert!(source.contains("    Args:\n        value: Value to process."));
        assert!(source.contains("    Returns:\n        The processed value."));
        assert!(
            source.contains(
                "    Raises:\n        ExampleError: The Rust implementation returns `ExampleError` when validation\n            fails.\n"
            ),
            "{source}"
        );
        assert!(!source.contains("# Errors"));
        assert!(source.lines().all(|line| !line.ends_with(' ')));

        // Without Rust docs, nothing repeats what the signature says.
        callable.docs = None;
        assert_eq!(
            document(&callable),
            "    \"\"\"\n    Fallback.\n\n    Raises:\n        ExampleError\n    \"\"\"\n"
        );
    }
}
