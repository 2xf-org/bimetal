//! Rendering of Python callable wrappers, resources, and constants.
//!
//! Generated wrappers validate host values with Pydantic, delegate to the
//! native extension, and restore immutable contract shapes. Direct binary
//! boundaries bypass recursive adapters and preserve native bytes or NumPy
//! arrays.

use std::fmt::Write;

use anyhow::Result;
use bimetal_ir::python_names;
use bimetal_ir::{
    CallableDef, ErrorDef, MethodDef, ParamDef, Protocol, ResourceDef, TypeRef, TypeShape,
};

use crate::contract::{self, Contract, NamespaceItems};
use crate::documentation::{CallableDocumentation, CallableReturn};
use crate::literal;

use super::context::{ARROW_CONSTANT, LINE_WIDTH, PythonContext};
use super::package::API_MODULE;
use super::render;

/// The raw value that the native call returns.
const NATIVE_RESULT: &str = "bimetal_native";
/// The validated value that a wrapper returns, typed with its annotation.
const RESULT: &str = "bimetal_result";
/// The instance that a resource factory creates.
const INSTANCE: &str = "bimetal_instance";
/// Each item of a returned list of resources.
const ITEM: &str = "bimetal_item";

/// The attribute of a generated resource class that holds its native value.
const NATIVE_RESOURCE: &str = "native_resource";

/// The members that a generated resource class defines itself, besides its
/// special methods.
pub(crate) const RESOURCE_MEMBERS: [&str; 3] =
    [ResourceDef::CLOSE, ResourceDef::CLOSED, NATIVE_RESOURCE];

/// The class method of a generated resource class that wraps a native value
/// that another call returned.
const WRAP: &str = "__bimetal_wrap__";

/// The class attribute of a generated error class that names the model of
/// each code's fields.
const DETAILS_MODELS: &str = "__bimetal_details__";

/// Render errors, functions, resources, and constants for one namespace.
pub(super) fn python_api(
    items: &NamespaceItems<'_>,
    context: &PythonContext<'_>,
) -> Result<String> {
    let mut defined = render::python_model_names(items);
    defined.extend(items.api_names().into_iter().map(|name| name.python));
    let context = &context.within(defined.iter().map(String::as_str));
    let mut source = String::new();
    // Constants come first, below the imports.
    for constant in &items.constants {
        render::begin_python_alias(&mut source);
        render::emit_python_comment_docs(&mut source, constant.docs.as_deref(), "#: ");
        let value = render::python_json(&constant.value);
        let annotation = format!(
            "{}[{}]",
            context.import("typing.Final"),
            render::python_ref(&constant.ty, context)?
        );
        if render::is_python_literal(&constant.ty) {
            let _ = writeln!(source, "{}: {annotation} = {value}", constant.host_name);
        } else {
            let start = format!("{}: {annotation} = ", constant.host_name);
            let restored = render::python_restored(&value, &constant.ty, context)?;
            emit_python_validation(&mut source, &start, "", &constant.ty, context, &restored)?;
        }
    }
    for error in &items.errors {
        let code_alias = contract::error_code_alias(error);
        render::begin_python_top_level(&mut source);
        let start = format!(
            "{code_alias}: {} = {}[",
            context.import("typing.TypeAlias"),
            context.import("typing.Literal")
        );
        // Documented codes keep a line each, below their comments.
        if error.codes.iter().all(|code| code.docs.is_none()) {
            let codes: Vec<String> = error
                .codes
                .iter()
                .map(|code| literal::quoted(&code.code))
                .collect();
            let _ = writeln!(source, "{}", render::bracketed(&start, &codes, "]", ""));
        } else {
            let _ = writeln!(source, "{start}");
            for code in &error.codes {
                render::emit_python_comment_docs(&mut source, code.docs.as_deref(), "    # ");
                let _ = writeln!(source, "    {},", literal::quoted(&code.code));
            }
            source.push_str("]\n");
        }
        render::begin_python_top_level(&mut source);
        let _ = writeln!(
            source,
            "class {}({}):",
            error.name,
            context.builtin("RuntimeError")
        );
        if render::emit_python_doc(&mut source, error.docs.as_deref(), "    ") {
            source.push('\n');
        }
        emit_python_error_members(&mut source, error, &code_alias, context)?;
    }
    for function in &items.functions {
        let call = native(&function.callable.rust_name, &function.native_name, context);
        emit_python_function(
            &mut source,
            &function.callable,
            &function.returns,
            /* method */ false,
            &call,
            context,
        )?;
    }
    for resource in &items.resources {
        emit_python_resource(&mut source, resource, context)?;
    }
    let docstring = format!(
        "The functions, classes and constants of `{}`, which call the extension.",
        render::python_namespace_module(context.package, context.namespace)
    );
    Ok(context.module(API_MODULE, &docstring, &source))
}

/// Render an error class's attributes and constructor.
fn emit_python_error_members(
    source: &mut String,
    error: &ErrorDef,
    code_alias: &str,
    context: &PythonContext<'_>,
) -> Result<()> {
    let string = context.builtin("str");
    let mut models = Vec::new();
    for code in &error.codes {
        if let Some(details) = &code.details {
            models.push((
                &code.code,
                render::python_ref(
                    &TypeRef::Named {
                        identity: details.clone(),
                    },
                    context,
                )?,
            ));
        }
    }
    let _ = writeln!(source, "    code: {code_alias}");
    if models.is_empty() {
        let _ = writeln!(
            source,
            "\n    def __init__(self, code: {code_alias}, message: {string}) -> None:\n        {}().__init__(message)\n        self.code = code",
            context.builtin("super"),
        );
        return Ok(());
    }
    let union = models
        .iter()
        .map(|(_, model)| model.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    let entries = models
        .iter()
        .map(|(code, model)| format!("{}: {model}", literal::quoted(code)))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(
        source,
        "    details: {union} | None\n    {DETAILS_MODELS}: {}[{}[{}, {}]] = {{{entries}}}\n\n    def __init__(\n        self,\n        code: {code_alias},\n        message: {string},\n        details: {union} | None = None,\n    ) -> None:\n        {}().__init__(message)\n        self.code = code\n        self.details = details",
        context.import("typing.ClassVar"),
        context.builtin("dict"),
        context.builtin("str"),
        context.import("typing.Any"),
        context.builtin("super"),
    );
    Ok(())
}

/// Return the runtime names that the package root exports: the panic
/// exception, and the protocols that Arrow parameters accept.
#[must_use]
pub(super) fn python_root_exports(contract: &Contract<'_>) -> Vec<&'static str> {
    let mut exports = vec![python_names::PANIC_EXCEPTION];
    if contract.uses_arrow() {
        exports.extend([
            contract::ARROW_ARRAY_PROTOCOL,
            contract::ARROW_SCHEMA_PROTOCOL,
            contract::ARROW_STREAM_PROTOCOL,
        ]);
    }
    exports
}

/// Render one free function, or one resource method, which returns
/// `returns`, around `call`, the native callable.
fn emit_python_function(
    source: &mut String,
    callable: &CallableDef,
    returns: &TypeRef,
    method: bool,
    call: &str,
    context: &PythonContext<'_>,
) -> Result<()> {
    emit_python_callable(
        source,
        callable,
        returns,
        method,
        &callable.rust_name,
        call,
        context,
    )
}

/// Render one free function, or one resource method, under `name`, which
/// returns `returns`, around `call`, the native callable.
fn emit_python_callable(
    source: &mut String,
    callable: &CallableDef,
    returns: &TypeRef,
    method: bool,
    name: &str,
    call: &str,
    context: &PythonContext<'_>,
) -> Result<()> {
    let (params, call_params) = python_params(&callable.params, context)?;
    let return_type = render::python_ref(returns, context)?;
    let (indent, first_param) = if method {
        ("    ", Some("self"))
    } else {
        ("", None)
    };
    if !method {
        render::begin_python_top_level(source);
        if binds_native(callable, returns, &call_params, context)? {
            let _ = writeln!(source, "@{}({call})", context.runtime("bound"));
        }
    }
    emit_python_signature(
        source,
        indent,
        callable.is_async,
        name,
        first_param,
        &params,
        &return_type,
    );
    render::emit_python_callable_doc(
        source,
        &CallableDocumentation {
            callable,
            fallback_summary: format!(
                "Call the Rust implementation of ``{}``.",
                callable.rust_name
            ),
            returns: CallableReturn::from_type(returns),
        },
        context,
        &format!("{indent}    "),
    )?;
    let unit = matches!(returns, TypeRef::Unit);
    let arrow = python_arrow_result(returns, context);
    let wraps = contract::nodes(returns).any(|node| matches!(node, TypeRef::Resource { .. }));
    // The extension makes the value as Pydantic would validate it, so only a
    // NumPy buffer is left to restore and validate.
    let validates = !python_direct_bytes(returns)
        && context
            .contract
            .reaches(returns, |item| matches!(item, TypeRef::Buffer { .. }))?;
    let direct = !unit && arrow.is_none() && !wraps && !validates;
    // The native functions return `Any`, so a typed local gives the return
    // its type, which costs nothing: Python never evaluates its annotation.
    let (target, annotation) = match (unit, direct) {
        (true, _) => (None, None),
        (false, true) => (Some(RESULT), Some(return_type.as_str())),
        (false, false) => (Some(NATIVE_RESULT), None),
    };
    emit_python_native_call(
        source,
        &format!("{indent}    "),
        target,
        annotation,
        &NativeCall {
            callable: call,
            args: &call_params,
            awaited: callable.is_async,
        },
        context,
    );
    if unit {
        let _ = writeln!(source, "{indent}    return None");
    } else if direct {
        let _ = writeln!(source, "{indent}    return {RESULT}");
    } else if let Some(arrow) = arrow {
        let _ = writeln!(
            source,
            "{indent}    {RESULT}: {return_type} = {arrow}\n{indent}    return {RESULT}"
        );
    } else if wraps {
        let wrapped = python_wrapped(NATIVE_RESULT, returns, context)?;
        let _ = writeln!(
            source,
            "{indent}    {RESULT}: {return_type} = {wrapped}\n{indent}    return {RESULT}"
        );
    } else {
        let start = format!("{indent}    {RESULT}: {return_type} = ");
        let restored = render::python_restored(NATIVE_RESULT, returns, context)?;
        emit_python_validation(
            source,
            &start,
            &format!("{indent}    "),
            returns,
            context,
            &restored,
        )?;
        let _ = writeln!(source, "{indent}    return {RESULT}");
    }
    Ok(())
}

/// Whether the module binds the native function in place of a free function:
/// a brief synchronous one whose arguments pass as they are, under their Rust
/// names, with none that a caller may omit, and whose result returns as the
/// extension makes it.
fn binds_native(
    callable: &CallableDef,
    returns: &TypeRef,
    arguments: &Arguments,
    context: &PythonContext<'_>,
) -> Result<bool> {
    if !callable.brief || callable.is_async {
        return Ok(false);
    }
    let names = render::python_param_names(&callable.params);
    let passes = callable
        .params
        .iter()
        .zip(&names)
        .zip(arguments.first.iter().zip(&arguments.dumped))
        .all(|((param, name), (first, dumped))| {
            param.default.is_none() && *name == param.rust_name && first == name && dumped == name
        });
    if !passes || contract::omissible_params(&callable.params).contains(&true) {
        return Ok(false);
    }
    let arrow = match returns {
        TypeRef::Option { item } => matches!(item.as_ref(), TypeRef::Arrow { .. }),
        returns => matches!(returns, TypeRef::Arrow { .. }),
    };
    if arrow || contract::nodes(returns).any(|node| matches!(node, TypeRef::Resource { .. })) {
        return Ok(false);
    }
    Ok(python_direct_bytes(returns)
        || !context
            .contract
            .reaches(returns, |item| matches!(item, TypeRef::Buffer { .. }))?)
}

/// Whether a call returns bytes, or an option of them, through the direct
/// binary boundary that makes Python `bytes`.
fn python_direct_bytes(returns: &TypeRef) -> bool {
    match returns {
        TypeRef::Bytes | TypeRef::FixedBytes { .. } => true,
        TypeRef::Option { item } => python_direct_bytes(item),
        _ => false,
    }
}

/// Render the pyarrow value of a call that returns an Arrow value or an option
/// of one, or `None` for any other call.
///
/// Strict type checkers report the unknown types in pyarrow's stubs, so a
/// module constant reaches the class once through `getattr`, which returns
/// `Any`.
fn python_arrow_result(returns: &TypeRef, context: &PythonContext<'_>) -> Option<String> {
    let (value, optional) = match returns {
        TypeRef::Arrow { value } => (value, false),
        TypeRef::Option { item } => match item.as_ref() {
            TypeRef::Arrow { value } => (value, true),
            _ => return None,
        },
        _ => return None,
    };
    let (attribute, methods) = contract::arrow_spelling(*value)
        .import
        .split_first()
        .expect("an import path names at least a pyarrow attribute");
    let import = context.constant(
        ARROW_CONSTANT,
        format!(
            "{}({}, {}){}",
            context.builtin("getattr"),
            context.import("pyarrow"),
            literal::quoted(attribute),
            methods
                .iter()
                .map(|method| format!(".{method}"))
                .collect::<String>(),
        ),
    );
    let converted = format!("{import}({NATIVE_RESULT})");
    Some(if optional {
        format!("None if {NATIVE_RESULT} is None else {converted}")
    } else {
        converted
    })
}

/// Render the generated resource objects of `value`, the native objects that
/// a call of `returns` returned.
fn python_wrapped(value: &str, returns: &TypeRef, context: &PythonContext<'_>) -> Result<String> {
    Ok(match returns {
        TypeRef::Resource { identity } => format!(
            "{}.{WRAP}({value})",
            render::python_resource_ref(identity, context)?
        ),
        TypeRef::Option { item } => format!(
            "None if {value} is None else {}",
            python_wrapped(value, item, context)?
        ),
        TypeRef::List { item } => format!(
            "[{} for {ITEM} in {value}]",
            python_wrapped(ITEM, item, context)?
        ),
        _ => value.to_owned(),
    })
}

/// The arguments of one native call: as the first call passes them, and as a
/// second call passes them when the extension raises `Revalidate`, with
/// Pydantic's dump of each argument that the extension reads as it is.
struct Arguments {
    first: Vec<String>,
    dumped: Vec<String>,
}

/// Render a callable's annotated parameters and the arguments that pass them
/// to the native call.
fn python_params(
    params: &[ParamDef],
    context: &PythonContext<'_>,
) -> Result<(Vec<String>, Arguments)> {
    let names = render::python_param_names(params);
    let omissible = contract::omissible_params(params);
    let mut declarations = Vec::new();
    let mut arguments = Arguments {
        first: Vec::new(),
        dumped: Vec::new(),
    };
    for ((param, name), omissible) in params.iter().zip(&names).zip(omissible) {
        declarations.push(render::python_param(param, name, omissible, context)?);
        let (first, dumped) = python_host_argument(param, name, context)?;
        arguments.first.push(first);
        arguments.dumped.push(dumped);
    }
    Ok((declarations, arguments))
}

/// Emit a Python function signature, on one line when it fits, and
/// otherwise one parameter per line.
fn emit_python_signature(
    source: &mut String,
    indent: &str,
    asynchronous: bool,
    name: &str,
    first_param: Option<&str>,
    params: &[String],
    return_type: &str,
) {
    let all_params: Vec<&str> = first_param
        .into_iter()
        .chain(params.iter().map(String::as_str))
        .collect();
    let keyword = if asynchronous { "async def" } else { "def" };
    let compact = format!(
        "{indent}{keyword} {name}({}) -> {return_type}:",
        all_params.join(", ")
    );
    if compact.chars().count() <= LINE_WIDTH {
        let _ = writeln!(source, "{compact}");
        return;
    }
    let _ = writeln!(source, "{indent}{keyword} {name}(");
    for param in all_params {
        let _ = writeln!(source, "{indent}    {param},");
    }
    let _ = writeln!(source, "{indent}) -> {return_type}:");
}

/// One call into the native extension: the callable expression, its
/// arguments, and whether the call returns an awaitable.
struct NativeCall<'call> {
    callable: &'call str,
    args: &'call Arguments,
    awaited: bool,
}

/// Emit one native call that assigns its result to `target`, if any, which
/// the first attempt declares with `annotation`, if any.
fn emit_python_native_call(
    source: &mut String,
    indent: &str,
    target: Option<&str>,
    annotation: Option<&str>,
    call: &NativeCall<'_>,
    context: &PythonContext<'_>,
) {
    let awaited = if call.awaited { "await " } else { "" };
    let head = |annotation: Option<&str>| match (target, annotation) {
        (Some(target), Some(annotation)) => format!("{target}: {annotation} = {awaited}"),
        (Some(target), None) => format!("{target} = {awaited}"),
        (None, _) => awaited.to_owned(),
    };
    // A local whose annotation leaves the call no room on its line is
    // declared on a line of its own.
    let annotation = match (target, annotation) {
        (Some(target), Some(annotation))
            if format!("{indent}{}{}(", head(Some(annotation)), call.callable)
                .chars()
                .count()
                > LINE_WIDTH =>
        {
            let _ = writeln!(
                source,
                "{}",
                render::python_annotation(&format!("{indent}{target}: "), annotation, indent)
            );
            None
        }
        _ => annotation,
    };
    emit_python_attempts(
        source,
        indent,
        &head(annotation),
        &head(None),
        call,
        context,
    );
}

/// Emit the native call, starting with `head`, and when an argument may need
/// Pydantic's dump, an `except Revalidate` retry with the dumps, starting with
/// `retry_head`.
fn emit_python_attempts(
    source: &mut String,
    indent: &str,
    head: &str,
    retry_head: &str,
    call: &NativeCall<'_>,
    context: &PythonContext<'_>,
) {
    if call.args.first == call.args.dumped {
        emit_python_call(
            source,
            &format!("{indent}{head}"),
            indent,
            call.callable,
            &call.args.first,
        );
        return;
    }
    let inner = format!("{indent}    ");
    let _ = writeln!(source, "{indent}try:");
    emit_python_call(
        source,
        &format!("{inner}{head}"),
        &inner,
        call.callable,
        &call.args.first,
    );
    let _ = writeln!(source, "{indent}except {}:", context.runtime("Revalidate"));
    emit_python_call(
        source,
        &format!("{inner}{retry_head}"),
        &inner,
        call.callable,
        &call.args.dumped,
    );
}

/// Emit a Python call, on one line when it fits, and otherwise one argument
/// per line.
fn emit_python_call(
    source: &mut String,
    prefix: &str,
    indent: &str,
    callable: &str,
    args: &[String],
) {
    let compact = format!("{prefix}{callable}({})", args.join(", "));
    if compact.chars().count() <= LINE_WIDTH {
        let _ = writeln!(source, "{compact}");
        return;
    }
    let _ = writeln!(source, "{prefix}{callable}(");
    for arg in args {
        let _ = writeln!(source, "{indent}    {arg},");
    }
    let _ = writeln!(source, "{indent})");
}

/// Emit the line that starts with `start`, indented by `indent`, and ends with
/// the validation of `value`, a restored native value, against its generated
/// annotation.
fn emit_python_validation(
    source: &mut String,
    start: &str,
    indent: &str,
    reference: &TypeRef,
    context: &PythonContext<'_>,
    value: &str,
) -> Result<()> {
    let adapter = render::python_adapter_constant(reference, context)?;
    let _ = writeln!(
        source,
        "{}",
        render::bracketed(
            &format!("{start}{adapter}.validate("),
            &[value.to_owned()],
            ")",
            indent
        )
    );
    Ok(())
}

/// Spell the native function or class `native_name`, which a constant of the
/// module holds, so a call does not look it up again.
/// The constant is `BIMETAL_`, the Python name `name` in capitals, and
/// `_NATIVE`.
fn native(name: &str, native_name: &str, context: &PythonContext<'_>) -> String {
    context.named_constant(
        format!("BIMETAL_{}_NATIVE", name.to_uppercase()),
        format!(
            "{}({}, {})",
            context.builtin("getattr"),
            context.runtime(python_names::NATIVE_MODULE),
            literal::quoted(native_name)
        ),
    )
}

/// Render one resource class, its factories, methods, and close operation.
fn emit_python_resource(
    source: &mut String,
    resource: &ResourceDef,
    context: &PythonContext<'_>,
) -> Result<()> {
    render::begin_python_top_level(source);
    let _ = writeln!(source, "class {}:", resource.name);
    // The class body defines the methods and factories, which can hide a
    // builtin in the annotations and decorators of the members after them.
    let members = resource
        .factories
        .iter()
        .map(|factory| factory.rust_name.as_str())
        .chain(
            resource
                .methods
                .iter()
                .map(|method| method.callable.rust_name.as_str()),
        );
    let context = &context.within(members);
    if render::emit_python_doc(source, resource.docs.as_deref(), "    ") {
        source.push('\n');
    }
    // Declared in the class, because a class whose constructors are all
    // async never assigns it in `__init__`.
    let _ = writeln!(
        source,
        "    {NATIVE_RESOURCE}: {}\n",
        context.import("typing.Any")
    );
    // Only a constructor or a factory calls the native class. A resource that
    // other exports return never names it, so asking for it would import
    // `native` unused.
    let native_class = if resource.constructor.is_some() || !resource.factories.is_empty() {
        native(&resource.name, &resource.native_name, context)
    } else {
        String::new()
    };
    emit_python_init(source, resource, &native_class, context)?;
    let _ = writeln!(
        source,
        "\n    @{}\n    def {WRAP}(cls, native: {}) -> {}:\n        {INSTANCE} = cls.__new__(cls)\n        {INSTANCE}.{NATIVE_RESOURCE} = native\n        return {INSTANCE}",
        context.builtin("classmethod"),
        context.import("typing.Any"),
        resource.name,
    );
    for factory in &resource.factories {
        emit_python_factory(source, resource, factory, &native_class, context)?;
    }
    for method in &resource.methods {
        emit_python_method(source, method, context)?;
    }
    source.push_str("\n    def close(self) -> None:\n");
    render::emit_python_doc(
        source,
        Some("Release the native resources owned by this instance."),
        "        ",
    );
    let _ = writeln!(
        source,
        "        self.{NATIVE_RESOURCE}.close()\n\n    @property\n    def closed(self) -> {}:",
        context.builtin("bool")
    );
    render::emit_python_doc(
        source,
        Some("Whether the native resources were released."),
        "        ",
    );
    let _ = writeln!(
        source,
        "        {RESULT}: {} = self.{NATIVE_RESOURCE}.closed()\n        return {RESULT}",
        context.builtin("bool")
    );
    emit_python_with(source, resource, context)?;
    emit_python_protocols(source, resource, context)
}

/// Render one method of a resource class: a property for a getter or a
/// setter, and otherwise a method of its Rust name.
fn emit_python_method(
    source: &mut String,
    method: &MethodDef,
    context: &PythonContext<'_>,
) -> Result<()> {
    let callable = &method.callable;
    let call = format!("self.{NATIVE_RESOURCE}.{}", callable.host_name);
    let name = match method.protocol {
        Some(Protocol::Setter) => contract::property_name(&callable.rust_name),
        _ => &callable.rust_name,
    };
    source.push('\n');
    match method.protocol {
        Some(Protocol::Getter) => source.push_str("    @property\n"),
        Some(Protocol::Setter) => {
            let _ = writeln!(source, "    @{name}.setter");
        }
        _ => {}
    }
    emit_python_callable(
        source,
        callable,
        &method.returns,
        /* method */ true,
        name,
        &call,
        context,
    )
}

/// Render a resource class's context manager: `with` and `async with` run the
/// resource's `enter` and `exit` methods, tell `exit` whether the block raised,
/// and close the resource. A method that is async cannot run in a plain `with`.
fn emit_python_with(
    source: &mut String,
    resource: &ResourceDef,
    context: &PythonContext<'_>,
) -> Result<()> {
    let name = &resource.name;
    let object = context.builtin("object");
    let enter = resource.protocol_method(Protocol::Enter);
    let exit = resource.protocol_method(Protocol::Exit);
    let needs_async = enter
        .into_iter()
        .chain(exit)
        .any(|method| method.callable.is_async);
    let entered = match enter {
        Some(method) if !matches!(method.returns, TypeRef::Unit) => {
            render::python_ref(&method.returns, context)?
        }
        _ => name.clone(),
    };
    let run = |prefix: &str, method: &MethodDef, arguments: &str| {
        format!("{prefix}self.{}({arguments})", method.callable.rust_name)
    };
    let entering = |awaited: &str| match enter {
        Some(method) if !matches!(method.returns, TypeRef::Unit) => {
            format!("        return {}\n", run(awaited, method, ""))
        }
        Some(method) => format!(
            "        {}\n        return self\n",
            run(awaited, method, "")
        ),
        None => "        return self\n".to_owned(),
    };
    let exiting = |awaited: &str| match exit {
        Some(method) => format!(
            "        if self.closed:\n            return\n        try:\n            {}\n        finally:\n            self.close()\n",
            run(awaited, method, "exception_type is not None")
        ),
        None => "        self.close()\n".to_owned(),
    };
    let _ = writeln!(source, "\n    def __enter__(self) -> {entered}:");
    if needs_async {
        let _ = writeln!(
            source,
            "        raise {}({})",
            context.builtin("TypeError"),
            literal::quoted(&format!("{name} needs `async with`"))
        );
    } else {
        source.push_str(&entering(""));
    }
    let _ = writeln!(
        source,
        "\n    def __exit__(self, exception_type: {object}, *_: {object}) -> None:"
    );
    if needs_async {
        source.push_str("        self.close()\n");
    } else {
        source.push_str(&exiting(""));
    }
    let enter_await = if enter.is_some_and(|method| method.callable.is_async) {
        "await "
    } else {
        ""
    };
    let exit_await = if exit.is_some_and(|method| method.callable.is_async) {
        "await "
    } else {
        ""
    };
    let _ = writeln!(source, "\n    async def __aenter__(self) -> {entered}:");
    source.push_str(&entering(enter_await));
    let _ = writeln!(
        source,
        "\n    async def __aexit__(self, exception_type: {object}, *_: {object}) -> None:"
    );
    source.push_str(&exiting(exit_await));
    Ok(())
}

/// Render the special methods of a resource class's protocols, other than the
/// context manager. Most are aliases of the method, which Python calls the
/// same way.
fn emit_python_protocols(
    source: &mut String,
    resource: &ResourceDef,
    context: &PythonContext<'_>,
) -> Result<()> {
    for method in &resource.methods {
        let Some(protocol) = method.protocol else {
            continue;
        };
        let rust_name = &method.callable.rust_name;
        let alias = match protocol {
            Protocol::Len => Some("__len__"),
            Protocol::Repr => Some("__repr__"),
            Protocol::Display => Some("__str__"),
            Protocol::Hash => Some("__hash__"),
            Protocol::Bool => Some("__bool__"),
            Protocol::Contains => Some("__contains__"),
            Protocol::SetItem => Some("__setitem__"),
            Protocol::DelItem => Some("__delitem__"),
            Protocol::Call => Some("__call__"),
            Protocol::GetItem if !matches!(method.returns, TypeRef::Option { .. }) => {
                Some("__getitem__")
            }
            Protocol::Enter
            | Protocol::Exit
            | Protocol::Next
            | Protocol::Getter
            | Protocol::Setter
            | Protocol::GetItem
            | Protocol::Eq => None,
        };
        if let Some(alias) = alias {
            let _ = writeln!(source, "\n    {alias} = {rust_name}");
            continue;
        }
        match protocol {
            Protocol::Next => emit_python_iteration(source, resource, method, context)?,
            Protocol::Eq => {
                let _ = writeln!(
                    source,
                    "\n    def __eq__(self, other: {}) -> {}:\n        if other is self:\n            return True\n        if not {}(other, {}):\n            return NotImplemented\n        return self.{rust_name}(other)",
                    context.builtin("object"),
                    context.builtin("bool"),
                    context.builtin("isinstance"),
                    resource.name,
                );
            }
            Protocol::GetItem => {
                let TypeRef::Option { item } = &method.returns else {
                    continue;
                };
                let (key, _) = python_params(&method.callable.params, context)?;
                let names = render::python_param_names(&method.callable.params);
                let _ = writeln!(
                    source,
                    "\n    def __getitem__(self, {}) -> {}:\n        {RESULT} = self.{rust_name}({})\n        if {RESULT} is None:\n            raise {}({})\n        return {RESULT}",
                    key.join(", "),
                    render::python_ref(item, context)?,
                    names.join(", "),
                    context.builtin("KeyError"),
                    names.join(", "),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// Render `__iter__` and `__next__`, or `__aiter__` and `__anext__` for an
/// async method: the method returns the next value, or `None` at the end.
fn emit_python_iteration(
    source: &mut String,
    resource: &ResourceDef,
    method: &MethodDef,
    context: &PythonContext<'_>,
) -> Result<()> {
    let name = &resource.name;
    let rust_name = &method.callable.rust_name;
    let TypeRef::Option { item } = &method.returns else {
        return Ok(());
    };
    let item = render::python_ref(item, context)?;
    let (iterate, next, keyword, awaited, stop) = if method.callable.is_async {
        (
            "__aiter__",
            "__anext__",
            "async def",
            "await ",
            context.builtin("StopAsyncIteration"),
        )
    } else {
        (
            "__iter__",
            "__next__",
            "def",
            "",
            context.builtin("StopIteration"),
        )
    };
    let _ = writeln!(
        source,
        "\n    def {iterate}(self) -> {name}:\n        return self\n\n    {keyword} {next}(self) -> {item}:\n        {RESULT} = {awaited}self.{rust_name}()\n        if {RESULT} is None:\n            raise {stop}\n        return {RESULT}"
    );
    Ok(())
}

/// Render a resource class's `__init__`, which calls `native_class`, the
/// native class, or which refuses to construct a resource whose constructors
/// are all async, because `__init__` cannot await.
fn emit_python_init(
    source: &mut String,
    resource: &ResourceDef,
    native_class: &str,
    context: &PythonContext<'_>,
) -> Result<()> {
    let Some(constructor) = &resource.constructor else {
        let factories = resource
            .factories
            .iter()
            .map(|factory| format!("{}.{}()", resource.name, factory.rust_name))
            .collect::<Vec<_>>()
            .join(" or ");
        let _ = writeln!(
            source,
            "    def __init__(self) -> None:\n        raise {}({})",
            context.builtin("TypeError"),
            literal::quoted(&if factories.is_empty() {
                format!(
                    "{} has no constructor; other exports return it",
                    resource.name
                )
            } else {
                format!(
                    "{} has no synchronous constructor; use {factories}",
                    resource.name
                )
            })
        );
        return Ok(());
    };
    let (params, calls) = python_params(&constructor.params, context)?;
    emit_python_signature(
        source,
        "    ",
        /* asynchronous */ false,
        "__init__",
        Some("self"),
        &params,
        "None",
    );
    render::emit_python_callable_doc(
        source,
        &CallableDocumentation {
            callable: constructor,
            fallback_summary: format!("Create a new ``{}``.", resource.name),
            returns: CallableReturn::Nothing,
        },
        context,
        "        ",
    )?;
    emit_python_native_call(
        source,
        "        ",
        Some(&format!("self.{NATIVE_RESOURCE}")),
        None,
        &NativeCall {
            callable: native_class,
            args: &calls,
            awaited: false,
        },
        context,
    );
    Ok(())
}

/// Render one factory of a resource class, a class method that calls the
/// factory of `native_class`, the native class.
fn emit_python_factory(
    source: &mut String,
    resource: &ResourceDef,
    factory: &CallableDef,
    native_class: &str,
    context: &PythonContext<'_>,
) -> Result<()> {
    let (params, calls) = python_params(&factory.params, context)?;
    let _ = writeln!(source, "\n    @{}", context.builtin("classmethod"));
    emit_python_signature(
        source,
        "    ",
        factory.is_async,
        &factory.rust_name,
        Some("cls"),
        &params,
        &resource.name,
    );
    render::emit_python_callable_doc(
        source,
        &CallableDocumentation {
            callable: factory,
            fallback_summary: format!("Create a new ``{}``.", resource.name),
            returns: CallableReturn::Value,
        },
        context,
        "        ",
    )?;
    let _ = writeln!(source, "        {INSTANCE} = cls.__new__(cls)");
    emit_python_native_call(
        source,
        "        ",
        Some(&format!("{INSTANCE}.{NATIVE_RESOURCE}")),
        None,
        &NativeCall {
            callable: &format!("{native_class}.{}", factory.host_name),
            args: &calls,
            awaited: factory.is_async,
        },
        context,
    );
    let _ = writeln!(source, "        return {INSTANCE}");
    Ok(())
}

/// Whether the extension reads a Python value of this type as it is: a
/// scalar, a container of scalars, or a value with a boundary of its own.
/// Pydantic writes every other value first, because the extension cannot read
/// a model, a date, a duration or a JSON value as Python holds it.
fn python_reads_directly(reference: &TypeRef) -> bool {
    if matches!(
        reference,
        TypeRef::Bytes
            | TypeRef::FixedBytes { .. }
            | TypeRef::Buffer { .. }
            | TypeRef::Arrow { .. }
            | TypeRef::Resource { .. }
            | TypeRef::Callback { .. }
    ) {
        return true;
    }
    !contract::nodes(reference).any(|item| {
        matches!(
            item,
            TypeRef::Named { .. }
                | TypeRef::DateTime
                | TypeRef::Date
                | TypeRef::Duration
                | TypeRef::Json
                | TypeRef::Bytes
                | TypeRef::FixedBytes { .. }
        )
    })
}

/// Render the native-call argument that passes the parameter `name`, for the
/// first call and for a second call after `Revalidate`. The extension reads
/// a model instance, a date-time, a date or a duration as it is, so the first
/// call passes such an argument itself, and the second its dump.
fn python_host_argument(
    param: &ParamDef,
    name: &str,
    context: &PythonContext<'_>,
) -> Result<(String, String)> {
    if let Some(argument) = python_object_argument(&param.ty, name, context)? {
        return Ok((argument.clone(), argument));
    }
    if python_reads_directly(&param.ty) {
        return Ok((name.to_owned(), name.to_owned()));
    }
    // The extension cannot read a buffer, or an enum with data, as Python holds it.
    let unread = context.contract.reaches(&param.ty, |item| match item {
        TypeRef::Buffer { .. } => true,
        TypeRef::Named { identity } => {
            context
                .contract
                .type_definition(identity)
                .is_ok_and(|definition| {
                    matches!(
                        definition.shape,
                        TypeShape::TaggedEnum { .. } | TypeShape::ExternalEnum { .. }
                    )
                })
        }
        _ => false,
    })?;
    let natively = !unread && render::python_reads_natively(&param.ty, context.contract)?;
    let dump = format!(
        "{}.dump({name})",
        render::python_adapter_constant(&param.ty, context)?
    );
    let dump = if natively {
        format!("{}({dump})", context.runtime("Dumped"))
    } else {
        dump
    };
    // `None` crosses as it is, so an absent argument costs no validation.
    let dump = match param.ty {
        TypeRef::Option { .. } => format!("None if {name} is None else {dump}"),
        _ => dump,
    };
    Ok(if natively {
        (name.to_owned(), dump)
    } else {
        (dump.clone(), dump)
    })
}

/// Render the argument of a parameter that crosses as a host object: a
/// resource's native object, or a callback paired with its result's adapter.
fn python_object_argument(
    reference: &TypeRef,
    name: &str,
    context: &PythonContext<'_>,
) -> Result<Option<String>> {
    Ok(match reference {
        TypeRef::Resource { identity } => Some(format!(
            "{}({name}, {}, {})",
            context.runtime("native_resource"),
            render::python_resource_ref(identity, context)?,
            literal::quoted(name)
        )),
        TypeRef::Option { item } => python_object_argument(item, name, context)?
            .map(|argument| format!("None if {name} is None else {argument}")),
        TypeRef::Callback { signature } => {
            let (returns, params) = signature
                .split_last()
                .expect("a callback's signature ends with its return");
            // The extension makes the arguments as Pydantic would, unless one
            // reaches a numeric buffer.
            let mut restored = false;
            for param in params {
                restored |= context
                    .contract
                    .reaches(param, |item| matches!(item, TypeRef::Buffer { .. }))?;
            }
            let function = if restored {
                let params = params
                    .iter()
                    .map(|param| render::python_adapter_constant(param, context))
                    .collect::<Result<Vec<_>>>()?;
                format!(
                    "{}({name}, [{}])",
                    context.runtime("host_callback"),
                    params.join(", ")
                )
            } else {
                name.to_owned()
            };
            // The extension reads the result as it is, or else this adapter's
            // dump of it.
            Some(format!(
                "({function}, {})",
                render::python_adapter_constant(returns, context)?
            ))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use bimetal_ir::{
        ArrowKind, BufferElement, FunctionDef, Manifest, MethodDef, Namespace, TypeDef,
    };

    use super::*;
    use crate::fixtures;

    /// Render the root module of `manifest`.
    fn render_api(manifest: &Manifest) -> String {
        let namespace = Namespace::root();
        let contract = fixtures::contract(manifest);
        let imports = RefCell::default();
        let context = PythonContext::new(&contract, "test", &namespace, &imports);
        python_api(&contract.namespaces()[&namespace], &context).expect("render the module")
    }

    /// A struct model named `name` with one field of type `ty`.
    fn model(name: &str, field: &str, ty: TypeRef) -> TypeDef {
        fixtures::model(name, vec![fixtures::typed_field(field, ty)])
    }

    fn function(name: &str, params: Vec<ParamDef>, returns: TypeRef) -> FunctionDef {
        FunctionDef {
            returns,
            ..fixtures::function(name, name, params)
        }
    }

    #[test]
    fn modules_import_exactly_what_their_code_uses() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        manifest.types = vec![
            model("Point", "x", TypeRef::Float { bits: 64 }),
            model("Unused", "y", TypeRef::Float { bits: 64 }),
        ];
        manifest.functions = vec![function(
            "touch",
            vec![fixtures::typed_param("point", fixtures::named("Point"))],
            TypeRef::DateTime,
        )];

        let source = render_api(&manifest);

        assert!(
            source.starts_with(concat!(
                "\"\"\"\nThe functions, classes and constants of `test`, which call the extension.\n\"\"\"\n\n",
                "from __future__ import annotations\n\n",
                "import pydantic\n\n",
                "from test import bimetal\n\n",
                "from .models import Point\n\n",
                "BIMETAL_TOUCH_NATIVE = getattr(bimetal.native, \"native_touch\")\n",
                "BIMETAL_ADAPTER_0 = bimetal.Adapter(lambda: Point)\n\n\n",
                "def touch(point: Point) -> pydantic.AwareDatetime:\n",
            )),
            "{source}"
        );
        assert!(
            source.contains(concat!(
                "    try:\n",
                "        bimetal_result: pydantic.AwareDatetime = BIMETAL_TOUCH_NATIVE(point)\n",
                "    except bimetal.Revalidate:\n",
                "        bimetal_result = BIMETAL_TOUCH_NATIVE(bimetal.Dumped(BIMETAL_ADAPTER_0.dump(point)))\n",
                "    return bimetal_result\n",
            )),
            "{source}"
        );
    }

    #[test]
    fn a_resource_that_only_exports_return_does_not_import_native() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.functions.clear();
        let resource = &mut manifest.resources[0];
        resource.constructor = None;
        resource.factories.clear();

        let source = render_api(&manifest);

        assert!(source.contains("has no constructor; other exports return it"));
        assert!(!source.contains("bimetal.native"), "{source}");
    }

    #[test]
    fn exports_named_like_builtins_or_the_package_keep_their_names() {
        let strings = || TypeRef::List {
            item: Box::new(TypeRef::String),
        };
        let mut manifest = fixtures::recorder_manifest();
        // `list` hides the builtin in the module, and `test` the package.
        manifest.functions = vec![
            function("list", Vec::new(), strings()),
            function("test", Vec::new(), TypeRef::Unit),
        ];
        // Inside the class, the method `list` hides the builtin for the
        // annotations of every member.
        manifest.resources[0].methods = ["list", "names"]
            .into_iter()
            .map(|name| MethodDef {
                returns: strings(),
                ..fixtures::method(name, name)
            })
            .collect();

        let source = render_api(&manifest);

        assert!(
            source.contains(
                "from __future__ import annotations\n\nimport builtins\nimport typing\n\nfrom test import bimetal\n"
            ),
            "{source}"
        );
        assert!(source.contains("\ndef list() -> builtins.list[str]:\n"));
        assert!(
            source.contains("    bimetal_result: builtins.list[str] = BIMETAL_LIST_NATIVE()\n")
        );
        assert!(source.contains("    def names(self) -> builtins.list[str]:\n"));
        // Other builtins stay plain.
        assert!(
            source.contains("\nBIMETAL_LIST_NATIVE = getattr(bimetal.native, \"native_list\")\n")
        );
        assert!(source.contains("\ndef test() -> None:\n"));
    }

    #[test]
    fn modules_without_hidden_builtins_do_not_import_builtins() {
        let source = render_api(&fixtures::recorder_manifest());

        assert!(!source.contains("builtins"));
        assert!(
            source
                .contains("    def __exit__(self, exception_type: object, *_: object) -> None:\n")
        );
    }

    #[test]
    fn generated_locals_have_names_that_parameters_give_up() {
        for local in [NATIVE_RESULT, RESULT, INSTANCE, ITEM] {
            assert!(python_names::is_reserved(local), "{local}");
        }
    }

    #[test]
    fn parameters_that_hide_wrapper_names_get_a_trailing_underscore() {
        let string = || TypeRef::String;
        let int = || TypeRef::Int {
            signed: false,
            bits: 32,
        };
        let mut manifest = fixtures::recorder_manifest();
        manifest.types.push(model("Point", "x", int()));
        manifest.functions = vec![
            function(
                "reverse",
                vec![fixtures::typed_param("bytes", TypeRef::Bytes)],
                TypeRef::Bytes,
            ),
            function(
                "label",
                vec![
                    fixtures::typed_param("value", string()),
                    fixtures::typed_param("int", int()),
                    fixtures::typed_param("int_", int()),
                    fixtures::typed_param("point", fixtures::named("Point")),
                    fixtures::typed_param("test", string()),
                    fixtures::typed_param("type", string()),
                    fixtures::typed_param("datetime", string()),
                    fixtures::typed_param("bimetal", string()),
                    fixtures::typed_param("bimetal_models_0", string()),
                    fixtures::typed_param("BIMETAL_ADAPTER_2", string()),
                ],
                TypeRef::String,
            ),
        ];
        manifest.functions[0].callable.docs =
            Some("Reverse bytes.\n\n# Arguments\n\n- `bytes` - The bytes to reverse.".to_owned());
        let factory = &mut manifest.resources[0].factories[0];
        factory.params = vec![
            fixtures::typed_param("cls", string()),
            fixtures::typed_param("value", int()),
        ];
        factory.docs = Some("Make one.\n\n# Arguments\n\n- `cls` - The class name.".to_owned());

        let source = render_api(&manifest);

        assert!(source.contains("def reverse(bytes_: bytes) -> bytes:"));
        // The documentation names the parameter as Python does.
        assert!(source.contains("    Args:\n        bytes_: The bytes to reverse.\n"));
        assert!(
            source
                .contains("BIMETAL_REVERSE_NATIVE = getattr(bimetal.native, \"native_reverse\")\n")
        );
        assert!(source.contains("    bimetal_result: bytes = BIMETAL_REVERSE_NATIVE(bytes_)\n"));
        // The module binds only `bimetal` of the package, so a parameter named
        // like the package hides nothing, while one named like a module that
        // the wrapper uses, or like the module's constants, gives way.
        assert!(source.contains("    value: str,\n    int__: int,\n    int_: int,\n    point: Point,\n    test: str,\n    type_: str,\n    datetime_: str,\n    bimetal_: str,\n    bimetal_models_0_: str,\n    BIMETAL_ADAPTER_2_: str,\n) -> str:"), "{source}");
        // Undocumented parameters appear only in the signature.
        assert!(!source.contains("        int__: "));
        assert!(source.contains("            value,\n            int__,\n            int_,\n            bimetal.Dumped(BIMETAL_ADAPTER_0.dump(point)),\n            test,\n            type_,\n            datetime_,\n            bimetal_,\n            bimetal_models_0_,\n            BIMETAL_ADAPTER_2_,\n"), "{source}");
        assert!(source.contains("def from_default(cls, cls_: str, value: int) -> Recorder:"));
        assert!(source.contains("        Args:\n            cls_: The class name.\n"));
    }

    #[test]
    fn a_returned_stream_becomes_a_record_batch_reader() {
        let stream = || TypeRef::Arrow {
            value: ArrowKind::Stream,
        };
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        manifest.functions = vec![function(
            "filter",
            vec![fixtures::typed_param("rows", stream())],
            stream(),
        )];

        let source = render_api(&manifest);

        assert!(
            source.contains(
                "def filter(rows: bimetal.ArrowStreamExportable) -> pyarrow.RecordBatchReader:"
            ),
            "{source}"
        );
        assert!(
            source.contains(
                "\nBIMETAL_ARROW_0 = getattr(pyarrow, \"RecordBatchReader\").from_stream\n"
            )
        );
        assert!(source.contains(
            "    bimetal_result: pyarrow.RecordBatchReader = BIMETAL_ARROW_0(bimetal_native)"
        ));
    }

    #[test]
    fn pydantic_writes_every_argument_that_rust_cannot_read_directly() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        manifest.types = vec![
            model("Point", "x", TypeRef::Float { bits: 64 }),
            model("Stamp", "at", TypeRef::DateTime),
        ];
        manifest.functions = vec![function(
            "touch",
            vec![
                fixtures::typed_param("point", fixtures::named("Point")),
                fixtures::typed_param("stamp", fixtures::named("Stamp")),
                fixtures::typed_param("at", TypeRef::DateTime),
                fixtures::typed_param("data", TypeRef::Bytes),
                fixtures::typed_param(
                    "count",
                    TypeRef::Int {
                        signed: false,
                        bits: 8,
                    },
                ),
            ],
            TypeRef::Unit,
        )];

        let source = render_api(&manifest);

        assert!(
            source.contains(concat!(
                "        BIMETAL_TOUCH_NATIVE(point, stamp, at, data, count)\n",
                "    except bimetal.Revalidate:\n",
                "        BIMETAL_TOUCH_NATIVE(\n",
                "            bimetal.Dumped(BIMETAL_ADAPTER_0.dump(point)),\n",
                "            bimetal.Dumped(BIMETAL_ADAPTER_1.dump(stamp)),\n",
                "            bimetal.Dumped(BIMETAL_ADAPTER_2.dump(at)),\n",
                "            data,\n",
                "            count,\n",
            )),
            "{source}"
        );
    }

    /// Guards the validations a call skips: an absent optional argument
    /// crosses as `None` without Pydantic, and a result of scalars, of
    /// containers of them, or of direct bytes returns as the extension made
    /// it.
    #[test]
    fn absent_arguments_and_plain_results_skip_pydantic() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        manifest.types = vec![model("Point", "x", TypeRef::Float { bits: 64 })];
        manifest.functions = vec![function(
            "count",
            vec![fixtures::typed_param(
                "near",
                TypeRef::Option {
                    item: Box::new(fixtures::named("Point")),
                },
            )],
            TypeRef::Int {
                signed: false,
                bits: 32,
            },
        )];

        let source = render_api(&manifest);

        assert!(source.contains(concat!(
            "        bimetal_result: int = BIMETAL_COUNT_NATIVE(near)\n",
            "    except bimetal.Revalidate:\n",
            "        bimetal_result = BIMETAL_COUNT_NATIVE(None if near is None else bimetal.Dumped(BIMETAL_ADAPTER_0.dump(near)))\n",
            "    return bimetal_result\n",
        )), "{source}");
        assert!(!source.contains(".validate("));

        let strings = || TypeRef::List {
            item: Box::new(TypeRef::String),
        };
        manifest.functions = vec![
            function(
                "index",
                Vec::new(),
                TypeRef::Map {
                    value: Box::new(TypeRef::Tuple {
                        items: vec![strings(), TypeRef::Json, TypeRef::Bool],
                    }),
                },
            ),
            function(
                "digest",
                Vec::new(),
                TypeRef::Option {
                    item: Box::new(TypeRef::FixedBytes { length: 32 }),
                },
            ),
            function("when", Vec::new(), strings()),
        ];
        let source = render_api(&manifest);
        assert!(!source.contains(".validate("), "{source}");
        assert!(!source.contains("restore_host"), "{source}");
    }

    /// Guards the results that the extension makes as Pydantic would
    /// validate them: models, dates, and containers of them, which the wrapper
    /// returns as they are.
    #[test]
    fn results_with_models_or_dates_return_as_the_extension_makes_them() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        manifest.types = vec![model("Point", "x", TypeRef::Float { bits: 64 })];
        manifest.functions = vec![
            function(
                "points",
                Vec::new(),
                TypeRef::List {
                    item: Box::new(fixtures::named("Point")),
                },
            ),
            function(
                "dates",
                Vec::new(),
                TypeRef::Option {
                    item: Box::new(TypeRef::Date),
                },
            ),
        ];

        let source = render_api(&manifest);

        assert!(
            source.contains("    bimetal_result: list[Point] = BIMETAL_POINTS_NATIVE()\n"),
            "{source}"
        );
        assert!(!source.contains(".validate("), "{source}");
    }

    /// Guards the one result that is restored and validated: a value that
    /// holds a NumPy buffer. A model of bytes returns as the extension makes
    /// it.
    #[test]
    fn only_buffers_are_restored_and_validated() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        manifest.types = vec![
            model("Blob", "data", TypeRef::Bytes),
            model(
                "Samples",
                "values",
                TypeRef::Buffer {
                    element: BufferElement::F32,
                },
            ),
        ];
        manifest.functions = vec![
            function("blob", Vec::new(), fixtures::named("Blob")),
            function("samples", Vec::new(), fixtures::named("Samples")),
        ];

        let source = render_api(&manifest);

        assert!(
            source.contains("    bimetal_result: Blob = BIMETAL_BLOB_NATIVE()\n"),
            "{source}"
        );
        // The validation leaves no room for the value on its line.
        assert!(source.contains(
            "    bimetal_result: Samples = BIMETAL_ADAPTER_0.validate(\n        bimetal.restore_host(bimetal_native, [\"named\", \"test::test::Samples\"]),\n    )\n"
        ), "{source}");
    }

    /// Guards which functions the module binds to the native function: a
    /// brief one that passes its arguments and its result on as they are.
    #[test]
    fn brief_functions_that_only_pass_values_on_are_the_native_function() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        manifest.types = vec![model("Point", "x", TypeRef::Float { bits: 64 })];
        let int = || TypeRef::Int {
            signed: true,
            bits: 64,
        };
        let brief = |name: &str, params: Vec<ParamDef>, returns: TypeRef| {
            let mut function = function(name, params, returns);
            function.callable.brief = true;
            function
        };
        manifest.functions = vec![
            brief(
                "add",
                vec![
                    fixtures::typed_param("a", int()),
                    fixtures::typed_param("b", int()),
                ],
                int(),
            ),
            brief("origin", Vec::new(), fixtures::named("Point")),
            brief(
                "length",
                vec![fixtures::typed_param("point", fixtures::named("Point"))],
                TypeRef::Float { bits: 64 },
            ),
            brief(
                "maybe",
                vec![fixtures::typed_param(
                    "value",
                    TypeRef::Option {
                        item: Box::new(int()),
                    },
                )],
                TypeRef::Unit,
            ),
            function("plain", vec![fixtures::typed_param("a", int())], int()),
        ];

        let source = render_api(&manifest);

        let bound = |name: &str| {
            source
                .lines()
                .zip(source.lines().skip(1))
                .any(|(line, next)| {
                    line.starts_with("@bimetal.bound(") && next.starts_with(&format!("def {name}("))
                })
        };
        assert!(bound("add"), "{source}");
        assert!(bound("origin"), "{source}");
        // A model argument may need its dump, and a caller may omit an
        // option.
        assert!(!bound("length"), "{source}");
        assert!(!bound("maybe"), "{source}");
        assert!(!bound("plain"), "{source}");
    }
}
