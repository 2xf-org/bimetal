//! Rendering of TypeScript callables, resources, and constants.
//!
//! Wrappers are JavaScript; their typed signatures and documentation are in
//! the `.d.ts` beside it. The runtime recursively prepares Serde-shaped values
//! and restores immutable results. Typed arrays cross as they are, and Arrow
//! values through the Arrow adapters. The wrappers are the same for the Wasm
//! module and the Node.js addon, whose loader the runtime starts with.
//!
//! Before a wrapper enters the module, the runtime's `checkNative` throws when
//! the Wasm module is not ready, failed to load, or was aborted by a trap, and
//! `checkArgument` throws a `TypeError` for an argument of the wrong type.
//! Every wrapper sends its failures through the runtime's `nativeError`, which
//! throws a typed contract error as its class, and every async wrapper awaits
//! the runtime's `nativePromise`, which a trap rejects. The runtime's guard
//! turns a trap into an `Error` that names it, or the Rust panic behind it.
//! The wrappers name JavaScript's globals through `globalThis`, which no
//! export can shadow.

use std::fmt::Write;

use anyhow::Result;
use bimetal_ir::typescript_names::{self, Callable};
use bimetal_ir::{
    ArrowKind, BufferElement, CallableDef, DefinitionId, ErrorDef, MethodDef, ParamDef, Protocol,
    ResourceDef, TypeRef, TypeShape, VariantPayload,
};

use crate::contract::{self, Contract, NamespaceItems};
use crate::documentation::{CallableDocumentation, CallableReturn};
use crate::literal;

use super::context::TypeScriptContext;
use super::module::Module;
use super::package::API_MODULE;
use super::render;

/// The property of a generated resource that holds its native value.
const NATIVE_RESOURCE: &str = "nativeResource";

/// The members that a generated resource class defines itself.
pub(crate) const RESOURCE_MEMBERS: [&str; 4] = [
    ResourceDef::CLOSE,
    ResourceDef::CLOSED,
    "constructor",
    NATIVE_RESOURCE,
];

/// The static method of a generated resource class that wraps a native value
/// that another call returned.
const WRAP: &str = "$wrap";

/// Render errors, functions, resources, and constants for one namespace.
/// `close` documents each resource's `close()`, which depends on the backend.
pub(super) fn typescript_api(
    items: &NamespaceItems<'_>,
    close: &str,
    context: &TypeScriptContext<'_>,
) -> Result<Module> {
    let mut module = Module::default();
    for error in &items.errors {
        emit_typescript_error(&mut module, error, context)?;
    }
    for function in &items.functions {
        let callee = format!(
            "{}[{}]",
            context.runtime("native"),
            literal::quoted(&function.native_name)
        );
        emit_typescript_function(
            &mut module,
            &function.callable,
            &function.returns,
            /* resource */ None,
            &callee,
            context,
        )?;
    }
    for resource in &items.resources {
        emit_typescript_resource(&mut module, resource, close, context)?;
    }
    for constant in &items.constants {
        module.declarations.push('\n');
        let docs = constant
            .docs
            .as_deref()
            .map(|docs| render::typescript_documentation_text(docs, context));
        render::emit_typescript_doc(&mut module.declarations, docs.as_deref(), "");
        let _ = writeln!(
            module.declarations,
            "export declare const {}: {};",
            constant.host_name,
            render::type_ref(&constant.ty, context)?
        );
        let value = render::typescript_value(&constant.value, &constant.ty, context.contract)?;
        let spec = contract::spec(&constant.ty);
        let value = if spec.is_null() {
            value
        } else {
            format!(
                "{}({value}, {})",
                context.runtime("restoreHost"),
                render::spec_literal(&spec)
            )
        };
        let _ = writeln!(
            module.javascript,
            "\nexport const {} = {value};",
            constant.host_name
        );
    }
    let mut declarations = context.declarations(API_MODULE, &module.declarations);
    // Resources declare `[Symbol.dispose]` and `[Symbol.asyncDispose]`.
    if !items.resources.is_empty() {
        declarations = format!("{}{declarations}", render::DISPOSABLE_LIBRARY);
    }
    Ok(Module {
        javascript: context.javascript(&module.javascript),
        declarations,
    })
}

/// Render an error class and the union of its codes.
fn emit_typescript_error(
    module: &mut Module,
    error: &ErrorDef,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    let mut details = Vec::new();
    for code in &error.codes {
        if let Some(model) = &code.details {
            details.push(render::typescript_named_ref(model, context)?);
        }
    }
    let declarations = &mut module.declarations;
    let code_alias = contract::error_code_alias(error);
    declarations.push('\n');
    render::emit_typescript_doc(
        declarations,
        Some(&format!("The codes that {{@link {}}} reports.", error.name)),
        "",
    );
    let _ = writeln!(declarations, "export type {code_alias} =");
    for (index, code) in error.codes.iter().enumerate() {
        render::emit_typescript_doc(declarations, code.docs.as_deref(), "  ");
        let end = if index + 1 == error.codes.len() {
            ";"
        } else {
            ""
        };
        let _ = writeln!(declarations, "  | {}{end}", literal::quoted(&code.code));
    }
    render::emit_typescript_doc(
        declarations,
        Some(
            error
                .docs
                .as_deref()
                .unwrap_or("An error reported by the Rust implementation."),
        ),
        "",
    );
    if details.is_empty() {
        let _ = writeln!(
            declarations,
            "export declare class {} extends globalThis.Error {{\n  /**\n   * Stable machine-readable error code.\n   */\n  readonly code: {code_alias};\n  constructor(code: {code_alias}, message: string);\n}}",
            error.name
        );
        let _ = writeln!(
            module.javascript,
            "\nexport class {} extends globalThis.Error {{\n  constructor(code, message) {{\n    super(message);\n    this.name = {};\n    this.code = code;\n  }}\n}}",
            error.name,
            literal::quoted(&error.name)
        );
        return Ok(());
    }
    let union = details.join(" | ");
    let _ = writeln!(
        declarations,
        "export declare class {} extends globalThis.Error {{\n  /**\n   * Stable machine-readable error code.\n   */\n  readonly code: {code_alias};\n  /**\n   * The fields of the error, for the codes that declare them.\n   */\n  readonly details: {union} | null;\n  constructor(code: {code_alias}, message: string, details?: {union} | null);\n}}",
        error.name
    );
    let _ = writeln!(
        module.javascript,
        "\nexport class {} extends globalThis.Error {{\n  constructor(code, message, details = null) {{\n    super(message);\n    this.name = {};\n    this.code = code;\n    this.details = details;\n  }}\n}}",
        error.name,
        literal::quoted(&error.name)
    );
    Ok(())
}

/// Render one free function, or one method of the class `resource`, which
/// returns `returns`, around `callee`, the native callable.
fn emit_typescript_function(
    module: &mut Module,
    callable: &CallableDef,
    returns: &TypeRef,
    resource: Option<&str>,
    callee: &str,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    emit_typescript_callable(module, callable, returns, resource, None, callee, context)
}

/// Render one free function, or one member of the class `resource`, which
/// returns `returns`, around `callee`, the native callable. `protocol` makes
/// a getter or a setter of a member.
fn emit_typescript_callable(
    module: &mut Module,
    callable: &CallableDef,
    returns: &TypeRef,
    resource: Option<&str>,
    protocol: Option<Protocol>,
    callee: &str,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    let params = render::typescript_params(&callable.params, context)?;
    let names = parameter_signature(&callable.params, context.contract)?;
    let mut return_type = render::return_type_ref(returns, context)?;
    let name = &callable.host_name;
    let callable_label = match resource {
        Some(class) => Callable::Member { class, name }.to_string(),
        None => Callable::Function(name).to_string(),
    };
    let mut call = format!(
        "{callee}({})",
        host_arguments(&callable.params, &callable_label, context)
    );
    if callable.is_async {
        return_type = format!("Promise<{return_type}>");
        call = format!("await {}({call})", context.runtime("nativePromise"));
    }
    let prefix = if callable.is_async { "async " } else { "" };
    let property = setter_property(name);
    // Argument errors name the wrapper, such as `greet()` or
    // `Ledger.deposit()`.
    let (indent, declaration, signature, label) = match resource {
        Some(class) => match protocol {
            Some(Protocol::Getter) => (
                "  ",
                format!("  get {name}(): {return_type};"),
                format!("  get {name}() {{"),
                Callable::Member { class, name }.to_string(),
            ),
            Some(Protocol::Setter) => (
                "  ",
                format!("  set {property}({params});"),
                format!("  set {property}({names}) {{"),
                Callable::Member { class, name }.to_string(),
            ),
            _ => (
                "  ",
                format!("  {name}({params}): {return_type};"),
                format!("  {prefix}{name}({names}) {{"),
                Callable::Member { class, name }.to_string(),
            ),
        },
        None => (
            "",
            format!("export declare function {name}({params}): {return_type};"),
            format!("export {prefix}function {name}({names}) {{"),
            Callable::Function(name).to_string(),
        ),
    };
    module.declarations.push('\n');
    render::emit_typescript_callable_doc(
        &mut module.declarations,
        &CallableDocumentation {
            callable,
            fallback_summary: format!("Calls the Rust implementation of `{name}`."),
            returns: CallableReturn::from_type(returns),
        },
        context,
        indent,
    )?;
    let _ = writeln!(module.declarations, "{declaration}");
    // An awaited call stays inside the `try`, so a rejection reaches `nativeError`.
    let result = if matches!(returns, TypeRef::Unit) {
        format!("{call};")
    } else {
        format!("return {};", host_result(&call, returns, context))
    };
    let _ = writeln!(module.javascript, "\n{signature}");
    emit_typescript_native_call(
        &mut module.javascript,
        &format!("{indent}  "),
        &label,
        &callable.params,
        &result,
        callable.error.as_ref(),
        context,
    )?;
    let _ = writeln!(module.javascript, "{indent}}}");
    Ok(())
}

/// Return the property that a setter assigns, from its host name: `title` for
/// `setTitle`.
fn setter_property(host_name: &str) -> String {
    let name = host_name.strip_prefix("set").unwrap_or(host_name);
    let mut letters = name.chars();
    match letters.next() {
        Some(first) => first.to_lowercase().chain(letters).collect(),
        None => host_name.to_owned(),
    }
}

/// Emit a call into the module. `checkNative` and `checkArgument` run first,
/// for the module and for each of `params`, whose argument errors name
/// `label`, the callable as [`Callable`] writes it. The `statement` then runs inside a `try` whose `catch` throws what
/// `nativeError` makes of the failure: the class of `error` for a typed
/// contract error, and otherwise a JavaScript exception.
fn emit_typescript_native_call(
    source: &mut String,
    indent: &str,
    label: &str,
    params: &[ParamDef],
    statement: &str,
    error: Option<&DefinitionId>,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    let error_type = match error {
        Some(error) => format!(", {}", render::typescript_error_ref(error, context)?),
        None => String::new(),
    };
    let _ = writeln!(source, "{indent}{}();", context.runtime("checkNative"));
    for param in params {
        let Some(kind) = argument_kind(&param.ty, context.contract)? else {
            continue;
        };
        let name = render::parameter_name(param);
        let _ = writeln!(
            source,
            "{indent}{}({name}, {}, {});",
            context.runtime("checkArgument"),
            literal::quoted(&kind),
            literal::quoted(&typescript_names::argument_label(label, &name))
        );
    }
    // The sources of stream arguments that a failed call never started.
    let takes_streams = params.iter().any(|param| param.ty == contract::STREAM);
    let close_sources = if takes_streams {
        let _ = writeln!(source, "{indent}const {SOURCES} = [];");
        format!("{indent}  {}({SOURCES});\n", context.arrow("closeSources"))
    } else {
        String::new()
    };
    let _ = writeln!(
        source,
        "{indent}try {{\n{indent}  {statement}\n{indent}}} catch ($error) {{\n{close_sources}{indent}  throw {}($error{error_type});\n{indent}}}",
        context.runtime("nativeError")
    );
    Ok(())
}

/// The wrapper's list of the sources of its stream arguments.
const SOURCES: &str = "$sources";

/// Render one resource class, its factories, methods, and disposal hooks.
/// `close` documents the class's `close()`.
fn emit_typescript_resource(
    module: &mut Module,
    resource: &ResourceDef,
    close: &str,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    let name = &resource.name;
    let native_class = format!(
        "{}[{}]",
        context.runtime("native"),
        literal::quoted(&resource.native_name)
    );
    module.declarations.push('\n');
    render::emit_typescript_doc(
        &mut module.declarations,
        Some(
            resource
                .docs
                .as_deref()
                .unwrap_or("A stateful resource implemented in Rust."),
        ),
        "",
    );
    let _ = writeln!(
        module.declarations,
        "export declare class {name} {{\n  private {NATIVE_RESOURCE};"
    );
    let _ = writeln!(module.javascript, "\nexport class {name} {{");
    emit_typescript_constructor(module, resource, &native_class, context)?;
    let _ = writeln!(
        module.javascript,
        "\n  static {WRAP}(native) {{\n    const $resource = globalThis.Object.create({name}.prototype);\n    $resource.{NATIVE_RESOURCE} = native;\n    return $resource;\n  }}"
    );
    for factory in &resource.factories {
        emit_typescript_factory(module, resource, factory, &native_class, context)?;
    }
    for method in &resource.methods {
        let callee = format!("this.{NATIVE_RESOURCE}.{}", method.callable.host_name);
        emit_typescript_callable(
            module,
            &method.callable,
            &method.returns,
            Some(name),
            method.protocol,
            &callee,
            context,
        )?;
        emit_typescript_protocol(module, method, context)?;
    }
    let exit = resource.protocol_method(Protocol::Exit);
    let needs_async = exit.is_some_and(|method| method.callable.is_async);
    let declarations = &mut module.declarations;
    declarations.push('\n');
    render::emit_typescript_doc(declarations, Some(close), "  ");
    declarations.push_str("  close(): void;\n");
    render::emit_typescript_doc(declarations, Some("Whether `close()` ran."), "  ");
    declarations.push_str("  get closed(): boolean;\n");
    if !needs_async {
        render::emit_typescript_doc(
            declarations,
            Some(if exit.is_some() {
                "Ends a `using` declaration: runs the exit method, told that the block failed because JavaScript cannot tell, and calls `close()`."
            } else {
                "Calls `close()` when a `using` declaration leaves scope."
            }),
            "  ",
        );
        declarations.push_str("  [Symbol.dispose](): void;\n");
    }
    render::emit_typescript_doc(
        declarations,
        Some(if exit.is_some() {
            "Ends an `await using` declaration: runs the exit method, told that the block failed because JavaScript cannot tell, and calls `close()`."
        } else {
            "Calls `close()` when an `await using` declaration leaves scope."
        }),
        "  ",
    );
    declarations.push_str("  [Symbol.asyncDispose](): Promise<void>;\n}\n");
    // Closing skips `checkNative`, so it does nothing once a trap aborts the
    // module, and a `using` block reports the trap alone. A closed resource
    // skips the exit method, and so does an aborted module, where it would
    // throw over the trap.
    let usable = context.runtime("nativeUsable");
    let exiting = |awaited: &str| match exit {
        Some(method) => format!(
            "  if (!{usable}() || this.{NATIVE_RESOURCE}.closed()) return;\n  try {{\n    {awaited}this.{}(true);\n  }} finally {{\n    this.close();\n  }}\n",
            method.callable.host_name
        ),
        None => "  this.close();\n".to_owned(),
    };
    let dispose = if needs_async {
        format!(
            "  throw new globalThis.TypeError({});\n",
            literal::quoted(&format!("{name} needs `await using`"))
        )
    } else {
        exiting("")
    };
    let async_dispose = exiting(if needs_async { "await " } else { "" });
    let _ = writeln!(
        module.javascript,
        "\n  close() {{\n    this.{NATIVE_RESOURCE}.close();\n  }}\n\n  get closed() {{\n    return !{usable}() || this.{NATIVE_RESOURCE}.closed();\n  }}\n}}\nif (globalThis.Symbol.dispose) {name}.prototype[globalThis.Symbol.dispose] = function () {{\n{dispose}}};\nif (globalThis.Symbol.asyncDispose) {name}.prototype[globalThis.Symbol.asyncDispose] = async function () {{\n{async_dispose}}};"
    );
    Ok(())
}

/// Render what a method's protocol adds to its class beside the method: an
/// iterator, a `length`, or the text that Node.js and `String()` show.
/// JavaScript has no operators for the other protocols.
fn emit_typescript_protocol(
    module: &mut Module,
    method: &MethodDef,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    let name = &method.callable.host_name;
    match method.protocol {
        Some(Protocol::Next) => {
            let TypeRef::Option { item } = &method.returns else {
                return Ok(());
            };
            let item = render::return_type_ref(item, context)?;
            let (declaration, signature, awaited) = if method.callable.is_async {
                (
                    format!("  [Symbol.asyncIterator](): AsyncGenerator<{item}, void, void>;"),
                    "  async *[globalThis.Symbol.asyncIterator]() {",
                    "await ",
                )
            } else {
                (
                    format!("  [Symbol.iterator](): Generator<{item}, void, void>;"),
                    "  *[globalThis.Symbol.iterator]() {",
                    "",
                )
            };
            let _ = writeln!(module.declarations, "\n{declaration}");
            let _ = writeln!(
                module.javascript,
                "\n{signature}\n    for (;;) {{\n      const $value = {awaited}this.{name}();\n      if ($value === null) return;\n      yield $value;\n    }}\n  }}"
            );
        }
        Some(Protocol::Len) => {
            let _ = writeln!(module.declarations, "\n  get length(): number;");
            let _ = writeln!(
                module.javascript,
                "\n  get length() {{\n    return globalThis.Number(this.{name}());\n  }}"
            );
        }
        Some(Protocol::Repr) => {
            let _ = writeln!(
                module.javascript,
                "\n  [globalThis.Symbol.for(\"nodejs.util.inspect.custom\")]() {{\n    return this.{name}();\n  }}"
            );
        }
        Some(Protocol::Display) => {
            let _ = writeln!(module.declarations, "\n  toString(): string;");
            let _ = writeln!(
                module.javascript,
                "\n  toString() {{\n    return this.{name}();\n  }}"
            );
        }
        _ => {}
    }
    Ok(())
}

/// Render a resource class's constructor, which calls `native_class`, the
/// native class, or which refuses to construct a resource whose constructors
/// are all async, because `new` cannot await.
fn emit_typescript_constructor(
    module: &mut Module,
    resource: &ResourceDef,
    native_class: &str,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    let name = &resource.name;
    let Some(constructor) = &resource.constructor else {
        let factories = resource
            .factories
            .iter()
            .map(|factory| format!("{name}.{}()", factory.host_name))
            .collect::<Vec<_>>()
            .join(" or ");
        module.declarations.push_str("  private constructor();\n");
        // An export may take the name of a JavaScript global, so the
        // generated code reaches globals through `globalThis`.
        let _ = writeln!(
            module.javascript,
            "  constructor() {{\n    throw new globalThis.TypeError({});\n  }}",
            literal::quoted(&if factories.is_empty() {
                format!("{name} has no constructor; other exports return it")
            } else {
                format!("{name} has no synchronous constructor; use {factories}")
            })
        );
        return Ok(());
    };
    render::emit_typescript_callable_doc(
        &mut module.declarations,
        &CallableDocumentation {
            callable: constructor,
            fallback_summary: format!("Creates a new `{name}`."),
            returns: CallableReturn::Nothing,
        },
        context,
        "  ",
    )?;
    let _ = writeln!(
        module.declarations,
        "  constructor({});",
        render::typescript_params(&constructor.params, context)?
    );
    let _ = writeln!(
        module.javascript,
        "  constructor({}) {{",
        parameter_signature(&constructor.params, context.contract)?
    );
    emit_typescript_native_call(
        &mut module.javascript,
        "    ",
        &Callable::Constructor(name).to_string(),
        &constructor.params,
        &format!(
            "this.{NATIVE_RESOURCE} = new {native_class}({});",
            host_arguments(
                &constructor.params,
                &Callable::Constructor(name).to_string(),
                context
            )
        ),
        constructor.error.as_ref(),
        context,
    )?;
    module.javascript.push_str("  }\n");
    Ok(())
}

/// Render one factory of a resource class, a static method that calls the
/// factory of `native_class`, the native class.
fn emit_typescript_factory(
    module: &mut Module,
    resource: &ResourceDef,
    factory: &CallableDef,
    native_class: &str,
    context: &TypeScriptContext<'_>,
) -> Result<()> {
    let name = &resource.name;
    let mut call = format!(
        "{native_class}.{}({})",
        factory.host_name,
        host_arguments(
            &factory.params,
            &Callable::Member {
                class: name,
                name: &factory.host_name,
            }
            .to_string(),
            context,
        )
    );
    let (returns, prefix) = if factory.is_async {
        call = format!("await {}({call})", context.runtime("nativePromise"));
        (format!("Promise<{name}>"), "async ")
    } else {
        (name.clone(), "")
    };
    module.declarations.push('\n');
    render::emit_typescript_callable_doc(
        &mut module.declarations,
        &CallableDocumentation {
            callable: factory,
            fallback_summary: format!("Creates a new `{name}`."),
            returns: CallableReturn::Value,
        },
        context,
        "  ",
    )?;
    let _ = writeln!(
        module.declarations,
        "  static {}({}): {returns};",
        factory.host_name,
        render::typescript_params(&factory.params, context)?,
    );
    let _ = writeln!(
        module.javascript,
        "\n  static {prefix}{}({}) {{\n    const $resource = globalThis.Object.create({name}.prototype);",
        factory.host_name,
        parameter_signature(&factory.params, context.contract)?,
    );
    emit_typescript_native_call(
        &mut module.javascript,
        "    ",
        &Callable::Member {
            class: name,
            name: &factory.host_name,
        }
        .to_string(),
        &factory.params,
        &format!("$resource.{NATIVE_RESOURCE} = {call};"),
        factory.error.as_ref(),
        context,
    )?;
    module.javascript.push_str("    return $resource;\n  }\n");
    Ok(())
}

/// Render a JavaScript parameter list, with the defaults of the parameters
/// that a caller may omit.
fn parameter_signature(params: &[ParamDef], contract: &Contract<'_>) -> Result<String> {
    let omissible = contract::omissible_params(params);
    let names = params
        .iter()
        .zip(omissible)
        .map(|(param, omissible)| {
            let name = render::parameter_name(param);
            Ok(match &param.default {
                Some(default) => format!(
                    "{name} = {}",
                    render::typescript_value(default, &param.ty, contract)?
                ),
                None if omissible => format!("{name} = null"),
                None => name,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(names.join(", "))
}

/// Render the native-call arguments for a parameter list: each prepared by
/// its schema, or an Arrow value encoded as an IPC stream.
#[must_use]
fn host_arguments(params: &[ParamDef], label: &str, context: &TypeScriptContext<'_>) -> String {
    params
        .iter()
        .map(|param| {
            let name = render::parameter_name(param);
            if let Some(argument) = host_object_argument(&param.ty, &name, label, context) {
                return argument;
            }
            match &param.ty {
                // A stream crosses as a source, which the async wrapper opens
                // before the call, and closes if the call fails first.
                TypeRef::Arrow {
                    value: ArrowKind::Stream,
                } => format!(
                    "await {}({name}, {SOURCES})",
                    context.arrow("arrowStreamToHost")
                ),
                TypeRef::Arrow { value } => format!(
                    "{}({name}, {})",
                    context.arrow("arrowToIpc"),
                    literal::quoted(contract::arrow_spelling(*value).key)
                ),
                reference => format!(
                    "{}({name}, {})",
                    context.runtime("prepareHost"),
                    render::spec_literal(&contract::spec(reference))
                ),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render the argument of a parameter that crosses as a host object: a
/// resource's native object, or the function that wraps a callback. Its errors
/// name `label`, the callable as [`Callable`] writes it.
fn host_object_argument(
    reference: &TypeRef,
    name: &str,
    label: &str,
    context: &TypeScriptContext<'_>,
) -> Option<String> {
    let argument_label = typescript_names::argument_label(label, name);
    match reference {
        TypeRef::Resource { identity } => {
            let class = render::typescript_resource_ref(identity, context).ok()?;
            Some(format!(
                "{}({name}, {class}, {})",
                context.runtime("nativeResource"),
                literal::quoted(&argument_label)
            ))
        }
        TypeRef::Option { item } => host_object_argument(item, name, label, context)
            .map(|argument| format!("{name} == null ? null : {argument}")),
        TypeRef::Callback { signature } => {
            let (returns, params) = signature
                .split_last()
                .expect("a callback's signature ends with its return");
            let params = params
                .iter()
                .map(|param| render::spec_literal(&contract::spec(param)))
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!(
                "{}({name}, [{params}], {}, {})",
                context.runtime("callbackFromHost"),
                render::spec_literal(&contract::spec(returns)),
                literal::quoted(&argument_label)
            ))
        }
        _ => None,
    }
}

/// Render the generated resource objects of `value`, the native objects that
/// a call of `returns` returned.
fn wrapped_resources(value: &str, returns: &TypeRef, context: &TypeScriptContext<'_>) -> String {
    match returns {
        TypeRef::Resource { identity } => {
            let class = render::typescript_resource_ref(identity, context)
                .expect("validation found every resource that the contract names");
            format!("{class}.{WRAP}({value})")
        }
        TypeRef::Option { item } => format!(
            "(($native) => $native == null ? null : {})({value})",
            wrapped_resources("$native", item, context)
        ),
        TypeRef::List { item } => format!(
            "globalThis.Array.from({value}, ($native) => {})",
            wrapped_resources("$native", item, context)
        ),
        _ => value.to_owned(),
    }
}

/// Render the value that a wrapper returns from `call`, a native value of
/// `returns`: restored by its schema, decoded from an Arrow IPC stream, or as
/// it is.
#[must_use]
fn host_result(call: &str, returns: &TypeRef, context: &TypeScriptContext<'_>) -> String {
    if contract::nodes(returns).any(|node| matches!(node, TypeRef::Resource { .. })) {
        return wrapped_resources(call, returns, context);
    }
    if let TypeRef::Arrow { value } = returns {
        return arrow_result(call, *value, context);
    }
    if let TypeRef::Option { item } = returns
        && let TypeRef::Arrow { value } = item.as_ref()
    {
        return format!(
            "(($native) => $native == null ? null : {})({call})",
            arrow_result("$native", *value, context)
        );
    }
    let spec = contract::spec(returns);
    // wasm-bindgen returns an empty `Option` of bytes or a buffer as `undefined`.
    let direct_option = matches!(returns, TypeRef::Option { item } if matches!(
        item.as_ref(),
        TypeRef::Bytes | TypeRef::FixedBytes { .. } | TypeRef::Buffer { .. }
    ));
    if spec.is_null() {
        call.to_owned()
    } else if direct_option {
        format!(
            "{}({call}, {}) ?? null",
            context.runtime("restoreHost"),
            render::spec_literal(&spec)
        )
    } else {
        format!(
            "{}({call}, {})",
            context.runtime("restoreHost"),
            render::spec_literal(&spec)
        )
    }
}

/// Render the Arrow value of `kind` that `native` holds: a stream that reads
/// it, or the value that its IPC stream decodes to.
fn arrow_result(native: &str, kind: ArrowKind, context: &TypeScriptContext<'_>) -> String {
    if kind == ArrowKind::Stream {
        return format!("{}({native})", context.arrow("arrowStreamFromHost"));
    }
    format!(
        "{}({native}, {})",
        context.arrow("arrowFromIpc"),
        literal::quoted(contract::arrow_spelling(kind).key)
    )
}

/// Return the kind of value that the runtime's `checkArgument` requires of an
/// argument: the key in its `argumentKinds` of the outermost JavaScript type
/// of the parameter's TypeScript type, with `?` when the argument may be
/// null. `None` for unit, for JSON, which takes any value, and for Arrow
/// values, which the Arrow adapters check as they encode them.
fn argument_kind(reference: &TypeRef, contract: &Contract<'_>) -> Result<Option<String>> {
    let kind = match reference {
        TypeRef::Unit
        | TypeRef::Json
        | TypeRef::Arrow { .. }
        | TypeRef::Resource { .. }
        | TypeRef::Callback { .. } => return Ok(None),
        TypeRef::Option { item } => {
            return Ok(argument_kind(item, contract)?
                .map(|kind| format!("{}?", kind.trim_end_matches('?'))));
        }
        TypeRef::Named { identity } => match &contract.type_definition(identity)?.shape {
            TypeShape::Struct { .. } | TypeShape::TaggedEnum { .. } => "object",
            TypeShape::StringEnum { .. } => "string",
            // A variant without data crosses as a string, and one with data
            // as an object, so only an enum without unit variants has one
            // kind.
            TypeShape::ExternalEnum { variants } => {
                if variants
                    .iter()
                    .any(|variant| variant.payload == VariantPayload::Unit)
                {
                    return Ok(None);
                }
                "object"
            }
            TypeShape::Alias { target } => return argument_kind(target, contract),
        },
        TypeRef::Bool => "boolean",
        TypeRef::Int { bits: 64, .. } => "bigint",
        TypeRef::Int { .. } | TypeRef::Float { .. } => "number",
        TypeRef::String => "string",
        TypeRef::DateTime => "date",
        // A date crosses as its `YYYY-MM-DD` text.
        TypeRef::Date => "string",
        TypeRef::Duration => "bigint",
        TypeRef::List { .. } | TypeRef::Tuple { .. } => "array",
        TypeRef::Map { .. } => "object",
        TypeRef::Bytes | TypeRef::FixedBytes { .. } => BufferElement::U8.numpy_name(),
        TypeRef::Buffer { element } => element.numpy_name(),
    };
    Ok(Some(kind.to_owned()))
}

#[cfg(test)]
mod tests {
    use bimetal_ir::{
        ArrowKind, CargoPackageId, ConstantDef, FunctionDef, Manifest, MethodDef, Namespace,
        TypeDef,
    };

    use super::*;
    use crate::fixtures;

    /// Render the root module of `manifest`.
    fn render_api(manifest: &Manifest) -> Module {
        let namespace = Namespace::root();
        let contract = fixtures::contract(manifest);
        let context = TypeScriptContext::new(&contract, &namespace);
        typescript_api(&contract.namespaces()[&namespace], "Closes.", &context)
            .expect("render the module")
    }

    #[test]
    fn modules_import_exactly_what_their_code_uses() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        manifest.functions.push(FunctionDef {
            returns: TypeRef::Arrow {
                value: ArrowKind::RecordBatch,
            },
            ..fixtures::function(
                "summarize",
                "summarize",
                vec![fixtures::typed_param(
                    "table",
                    TypeRef::Arrow {
                        value: ArrowKind::Table,
                    },
                )],
            )
        });

        let module = render_api(&manifest);

        assert!(module.javascript.starts_with(concat!(
            "import { checkNative as $checkNative, native as $native, nativeError as $nativeError } from \"./runtime.js\";\n",
            "import { arrowFromIpc as $arrowFromIpc, arrowToIpc as $arrowToIpc } from \"./arrow.js\";\n",
        )));
        assert!(module.javascript.contains(
            "return $arrowFromIpc($native[\"native_summarize\"]($arrowToIpc(table, \"table\")), \"recordBatch\");"
        ));
        assert!(
            module
                .declarations
                .starts_with("import type * as apache_arrow from \"apache-arrow\";\n\n")
        );
        assert!(module.declarations.contains(
            "export declare function summarize(table: apache_arrow.Table): apache_arrow.RecordBatch;"
        ));
    }

    #[test]
    fn streams_cross_through_the_stream_adapters() {
        let stream = || TypeRef::Arrow {
            value: ArrowKind::Stream,
        };
        let mut manifest = fixtures::recorder_manifest();
        manifest.resources.clear();
        let mut scale = FunctionDef {
            returns: stream(),
            ..fixtures::function(
                "scale",
                "scale",
                vec![fixtures::typed_param("rows", stream())],
            )
        };
        scale.callable.is_async = true;
        manifest.functions.push(scale);

        let module = render_api(&manifest);

        assert!(module.javascript.contains(
            "  const $sources = [];\n  try {\n    return $arrowStreamFromHost(await $nativePromise($native[\"native_scale\"](await $arrowStreamToHost(rows, $sources))));\n  } catch ($error) {\n    $closeSources($sources);\n    throw $nativeError($error);\n  }"
        ));
        assert!(module.declarations.contains(
            "export declare function scale(rows: RecordBatchSource): Promise<RecordBatchStream>;"
        ));
    }

    #[test]
    fn resource_declarations_bring_the_disposable_library() {
        let module = render_api(&fixtures::recorder_manifest());
        let empty = render_api(&fixtures::manifest());

        assert!(
            module
                .declarations
                .starts_with("/// <reference lib=\"esnext.disposable\" />\n")
        );
        assert_eq!(empty.declarations, "export {};\n");
        assert_eq!(empty.javascript, "export {};\n");
    }

    #[test]
    fn async_callables_await_the_native_promise() {
        let mut manifest = fixtures::recorder_manifest();
        let resource = &mut manifest.resources[0];
        let mut connect = resource.constructor.take().expect("constructor");
        connect.host_name = "connect".to_owned();
        connect.is_async = true;
        resource.factories = vec![connect];

        let module = render_api(&manifest);

        assert!(module.declarations.contains("private constructor();"));
        assert!(module.javascript.contains(
            "throw new globalThis.TypeError(\"Recorder has no synchronous constructor; use Recorder.connect()\");"
        ));
        assert!(
            module
                .declarations
                .contains("static connect(): Promise<Recorder>;")
        );
        // A trap rejects every pending promise, which the module cannot do.
        assert!(module.javascript.contains(
            "  static async connect() {\n    const $resource = globalThis.Object.create(Recorder.prototype);\n    $checkNative();\n    try {\n      $resource.nativeResource = await $nativePromise($native[\"native_recorder\"].connect());"
        ));
    }

    /// A function, and a factory, whose parameters are named like JavaScript
    /// reserved words and like the bindings that generated code uses.
    #[test]
    fn parameters_never_collide_with_reserved_words_or_generated_bindings() {
        let string = |name: &str| fixtures::typed_param(name, TypeRef::String);
        let mut manifest = fixtures::recorder_manifest();
        let mut load = fixtures::function(
            "load",
            "load",
            ["package", "native", "prepareHost", "error"]
                .map(string)
                .into(),
        );
        load.callable.docs = Some(
            "Load a value.\n\n# Arguments\n\n- `package` - The package to load from.".to_owned(),
        );
        load.returns = TypeRef::Map {
            value: Box::new(TypeRef::String),
        };
        manifest.functions.push(load);
        manifest.resources[0].factories[0].params = vec![string("value"), string("default")];

        let module = render_api(&manifest);

        assert!(module.javascript.contains(concat!(
            "export function load(package_, native, prepareHost, error) {\n",
            "  $checkNative();\n",
            "  $checkArgument(package_, \"string\", \"load(): argument `package_`\");\n",
            "  $checkArgument(native, \"string\", \"load(): argument `native`\");\n",
            "  $checkArgument(prepareHost, \"string\", \"load(): argument `prepareHost`\");\n",
            "  $checkArgument(error, \"string\", \"load(): argument `error`\");\n",
            "  try {\n",
            "    return $restoreHost($native[\"native_load\"]($prepareHost(package_, null), $prepareHost(native, null), $prepareHost(prepareHost, null), $prepareHost(error, null)), [\"map\", null]);\n",
            "  } catch ($error) {\n",
            "    throw $nativeError($error);\n",
            "  }\n",
            "}",
        )));
        assert!(
            module
                .declarations
                .contains(" * @param package_ - The package to load from.")
        );
        assert!(module.javascript.contains(
            "  static fromDefault(value, default_) {\n    const $resource = globalThis.Object.create(Recorder.prototype);"
        ));
        assert!(
            module
                .declarations
                .contains("static fromDefault(value: string, default_: string): Recorder;")
        );
    }

    #[test]
    fn typed_errors_reach_native_error_with_their_class() {
        let mut manifest = fixtures::recorder_manifest();
        manifest.errors.push(fixtures::error("LoadError"));
        let mut load = fixtures::method("load", "load");
        load.callable.error = Some(DefinitionId::new("test", "test::LoadError"));
        manifest.resources[0].methods.push(load);

        let module = render_api(&manifest);

        assert!(module.javascript.contains(
            "  load() {\n    $checkNative();\n    try {\n      this.nativeResource.load();\n    } catch ($error) {\n      throw $nativeError($error, LoadError);\n    }\n  }"
        ));
    }

    #[test]
    fn wrappers_check_each_argument_by_its_outermost_type() {
        let mut manifest = fixtures::recorder_manifest();
        let model = |name: &str, shape: TypeShape| TypeDef {
            shape,
            ..fixtures::model(name, Vec::new())
        };
        manifest.types = vec![
            model("Point", TypeShape::Struct { fields: Vec::new() }),
            model(
                "Priority",
                TypeShape::StringEnum {
                    variants: Vec::new(),
                },
            ),
            model(
                "Score",
                TypeShape::Alias {
                    target: TypeRef::Int {
                        signed: false,
                        bits: 8,
                    },
                },
            ),
        ];
        let resource = &mut manifest.resources[0];
        resource.constructor.as_mut().expect("constructor").params =
            vec![fixtures::typed_param("owner", TypeRef::String)];
        resource.methods.push(MethodDef {
            callable: CallableDef {
                params: vec![
                    fixtures::typed_param(
                        "total",
                        TypeRef::Int {
                            signed: true,
                            bits: 64,
                        },
                    ),
                    fixtures::typed_param(
                        "note",
                        TypeRef::Option {
                            item: Box::new(TypeRef::String),
                        },
                    ),
                    fixtures::typed_param(
                        "samples",
                        TypeRef::Buffer {
                            element: BufferElement::F32,
                        },
                    ),
                    fixtures::typed_param("checksum", TypeRef::FixedBytes { length: 4 }),
                    fixtures::typed_param("at", fixtures::named("Point")),
                    fixtures::typed_param("priority", fixtures::named("Priority")),
                    fixtures::typed_param("score", fixtures::named("Score")),
                    fixtures::typed_param(
                        "tags",
                        TypeRef::List {
                            item: Box::new(TypeRef::String),
                        },
                    ),
                    fixtures::typed_param("when", TypeRef::DateTime),
                    fixtures::typed_param("extra", TypeRef::Json),
                ],
                ..fixtures::callable("record", "record", Vec::new())
            },
            returns: TypeRef::Unit,
            protocol: None,
        });

        let module = render_api(&manifest);

        assert!(module.javascript.contains(
            "  constructor(owner) {\n    $checkNative();\n    $checkArgument(owner, \"string\", \"new Recorder(): argument `owner`\");\n    try {"
        ));
        assert!(module.javascript.contains(concat!(
            "  record(total, note, samples, checksum, at, priority, score, tags, when, extra) {\n",
            "    $checkNative();\n",
            "    $checkArgument(total, \"bigint\", \"Recorder.record(): argument `total`\");\n",
            "    $checkArgument(note, \"string?\", \"Recorder.record(): argument `note`\");\n",
            "    $checkArgument(samples, \"float32\", \"Recorder.record(): argument `samples`\");\n",
            "    $checkArgument(checksum, \"uint8\", \"Recorder.record(): argument `checksum`\");\n",
            "    $checkArgument(at, \"object\", \"Recorder.record(): argument `at`\");\n",
            "    $checkArgument(priority, \"string\", \"Recorder.record(): argument `priority`\");\n",
            "    $checkArgument(score, \"number\", \"Recorder.record(): argument `score`\");\n",
            "    $checkArgument(tags, \"array\", \"Recorder.record(): argument `tags`\");\n",
            "    $checkArgument(when, \"date\", \"Recorder.record(): argument `when`\");\n",
            "    try {\n",
        )));
    }

    #[test]
    fn constants_keep_a_proto_key_as_data() {
        let mut manifest = fixtures::manifest();
        manifest.constants.push(ConstantDef {
            owner: CargoPackageId::new("test"),
            rust_module: "test".to_owned(),
            host_name: "LIMITS".to_owned(),
            docs: None,
            ty: TypeRef::Map {
                value: Box::new(TypeRef::Int {
                    bits: 32,
                    signed: false,
                }),
            },
            value: serde_json::json!({ "__proto__": 1 }),
        });

        let module = render_api(&manifest);

        assert_eq!(
            module.javascript,
            "import { restoreHost as $restoreHost } from \"./runtime.js\";\n\nexport const LIMITS = $restoreHost({[\"__proto__\"]: 1}, [\"map\", null]);\n"
        );
    }
}
