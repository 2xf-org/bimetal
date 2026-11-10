//! The shapes that a resource's protocols and a callable's defaults need, and
//! the fields of error details.

use anyhow::Result;
use bimetal_ir::{
    CallableDef, MethodDef, ParamDef, Protocol, ResourceDef, TypeRef, TypeShape, VariantPayload,
};
use serde_json::Value;

use crate::contract::{self, Contract, NamespaceItems};

use super::contract::Packages;

/// What a protocol's method must return: a description, and a check.
type Returned = (&'static str, fn(&TypeRef) -> bool);

/// What a protocol asks of its method's signature.
struct Shape {
    /// How many parameters the method takes.
    params: usize,
    /// Whether the method may be async.
    may_be_async: bool,
    /// What the method must return.
    returns: Option<Returned>,
}

impl Shape {
    /// Return what `protocol` asks of its method, or `None` for a protocol
    /// that takes any signature.
    fn of(protocol: Protocol) -> Option<Self> {
        let unit: Returned = ("nothing", |reference| matches!(reference, TypeRef::Unit));
        let integer: Returned = ("an integer", |reference| {
            matches!(reference, TypeRef::Int { .. })
        });
        let boolean: Returned = ("a `bool`", |reference| matches!(reference, TypeRef::Bool));
        let text: Returned = ("a `String`", |reference| {
            matches!(reference, TypeRef::String)
        });
        let value: Returned = ("a value", |reference| !matches!(reference, TypeRef::Unit));
        let option: Returned = ("an `Option`", |reference| {
            matches!(reference, TypeRef::Option { .. })
        });
        let shape = |params, may_be_async, returns| {
            Some(Self {
                params,
                may_be_async,
                returns,
            })
        };
        match protocol {
            Protocol::Enter => shape(0, true, None),
            Protocol::Exit => shape(1, true, Some(unit)),
            Protocol::Next => shape(0, true, Some(option)),
            Protocol::Getter => shape(0, false, Some(value)),
            Protocol::Setter => shape(1, false, Some(unit)),
            Protocol::Len | Protocol::Hash => shape(0, false, Some(integer)),
            Protocol::Repr | Protocol::Display => shape(0, false, Some(text)),
            Protocol::Bool => shape(0, false, Some(boolean)),
            Protocol::Contains | Protocol::Eq => shape(1, false, Some(boolean)),
            Protocol::GetItem => shape(1, false, Some(value)),
            Protocol::SetItem => shape(2, false, Some(unit)),
            Protocol::DelItem => shape(1, false, Some(unit)),
            Protocol::Call => None,
        }
    }
}

/// Report protocols whose methods no host could use: one with the wrong
/// signature, and a second method for a protocol that a class has once.
pub(super) fn validate_protocols(items: &NamespaceItems<'_>, problems: &mut Vec<String>) {
    for resource in &items.resources {
        for method in &resource.methods {
            let Some(protocol) = method.protocol else {
                continue;
            };
            validate_protocol(resource, method, protocol, problems);
        }
        for protocol in Protocol::ALL {
            if matches!(protocol, Protocol::Getter | Protocol::Setter) {
                continue;
            }
            let count = resource
                .methods
                .iter()
                .filter(|method| method.protocol == Some(protocol))
                .count();
            if count > 1 {
                problems.push(format!(
                    "resource `{}` has {count} `#[bimetal({})]` methods; it can have one",
                    resource.name,
                    protocol.attribute()
                ));
            }
        }
    }
}

/// Report what is wrong with `method`, of `resource`, as `protocol`.
fn validate_protocol(
    resource: &ResourceDef,
    method: &MethodDef,
    protocol: Protocol,
    problems: &mut Vec<String>,
) {
    let name = format!("`{}.{}`", resource.name, method.callable.rust_name);
    let attribute = protocol.attribute();
    if let Some(shape) = Shape::of(protocol) {
        let count = method.callable.params.len();
        if count != shape.params {
            problems.push(format!(
                "{name} is `#[bimetal({attribute})]`, which takes {} parameters, but it takes {count}",
                shape.params
            ));
        }
        if method.callable.is_async && !shape.may_be_async {
            problems.push(format!(
                "{name} is `#[bimetal({attribute})]`, which cannot be async"
            ));
        }
        if let Some((description, check)) = shape.returns
            && !check(&method.returns)
        {
            problems.push(format!(
                "{name} is `#[bimetal({attribute})]`, which returns {description}"
            ));
        }
    }
    match protocol {
        Protocol::Exit => {
            if let Some(param) = method.callable.params.first()
                && !matches!(param.ty, TypeRef::Bool)
            {
                problems.push(format!(
                    "{name} is `#[bimetal(exit)]`, whose parameter says whether the block failed, so it must be a `bool`"
                ));
            }
        }
        Protocol::Setter => {
            let property = contract::property_name(&method.callable.rust_name);
            if property == method.callable.rust_name {
                problems.push(format!(
                    "{name} is `#[bimetal(setter)]`, whose name must start with `set_`"
                ));
            }
            let position = |candidate: &MethodDef| {
                candidate.protocol == Some(Protocol::Getter)
                    && candidate.callable.rust_name == property
            };
            let getter = resource.methods.iter().position(position);
            let setter = resource
                .methods
                .iter()
                .position(|candidate| candidate == method);
            if !matches!((getter, setter), (Some(getter), Some(setter)) if getter < setter) {
                problems.push(format!(
                    "{name} is `#[bimetal(setter)]`, which needs a `#[bimetal(getter)]` method named `{property}` before it"
                ));
            }
        }
        Protocol::Len | Protocol::Display => {
            let member = if protocol == Protocol::Len {
                "length"
            } else {
                "toString"
            };
            if resource
                .methods
                .iter()
                .any(|candidate| candidate.callable.host_name == member)
            {
                problems.push(format!(
                    "{name} is `#[bimetal({attribute})]`, which defines `{member}` in JavaScript, and `{}` has a method of that name; rename it",
                    resource.name
                ));
            }
        }
        _ => {}
    }
}

/// Report defaults that a host could not write: one that does not fit its
/// parameter, or one with a required parameter after it, which Python's syntax
/// forbids.
pub(super) fn validate_defaults(
    items: &NamespaceItems<'_>,
    contract: &Contract<'_>,
    problems: &mut Vec<String>,
) -> Result<()> {
    for callable in items.callables() {
        let label = callable.item.to_string();
        validate_callable_defaults(&label, callable.callable, contract, problems)?;
    }
    Ok(())
}

/// Report the defaults of one callable, which `label` names.
fn validate_callable_defaults(
    label: &str,
    callable: &CallableDef,
    contract: &Contract<'_>,
    problems: &mut Vec<String>,
) -> Result<()> {
    let omissible = contract::omissible_params(&callable.params);
    let mut required_after = false;
    for (param, omissible) in callable.params.iter().zip(omissible).rev() {
        let Some(default) = &param.default else {
            required_after |= !omissible;
            continue;
        };
        if required_after {
            problems.push(format!(
                "parameter `{}` of {label} has a default before a required parameter; move it after them",
                param.rust_name
            ));
        }
        if !default_fits(default, &param.ty, contract)? {
            problems.push(default_problem(label, param));
        }
    }
    Ok(())
}

/// Describe a default that does not fit its parameter.
fn default_problem(label: &str, param: &ParamDef) -> String {
    format!(
        "the default of parameter `{}` of {label} does not fit the parameter's type",
        param.rust_name
    )
}

/// Return whether a default is a value of the parameter's type.
fn default_fits(default: &Value, reference: &TypeRef, contract: &Contract<'_>) -> Result<bool> {
    Ok(match reference {
        TypeRef::Bool => default.is_boolean(),
        TypeRef::Int { signed, bits } => {
            let fits = default
                .as_i64()
                .map(i128::from)
                .or_else(|| default.as_u64().map(i128::from));
            fits.is_some_and(|value| {
                let (low, high) = if *signed {
                    (-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1)
                } else {
                    (0, (1_i128 << bits) - 1)
                };
                (low..=high).contains(&value)
            })
        }
        TypeRef::Float { .. } => default.is_number(),
        TypeRef::String | TypeRef::Json => true,
        TypeRef::Date => default.as_str().is_some_and(is_iso_date),
        // The hosts pass a duration as its nanoseconds.
        TypeRef::Duration => default.is_u64(),
        TypeRef::Option { item } => default_fits(default, item, contract)?,
        TypeRef::Named { identity } => match &contract.type_definition(identity)?.shape {
            TypeShape::StringEnum { variants } => default
                .as_str()
                .is_some_and(|value| variants.iter().any(|variant| variant.wire_name == value)),
            // Only a variant without data is a string.
            TypeShape::ExternalEnum { variants } => default.as_str().is_some_and(|value| {
                variants.iter().any(|variant| {
                    variant.wire_name == value && variant.payload == VariantPayload::Unit
                })
            }),
            TypeShape::Alias { target } => default_fits(default, target, contract)?,
            TypeShape::Struct { .. } | TypeShape::TaggedEnum { .. } => false,
        },
        _ => false,
    })
}

/// Return whether `text` is a day that exists, written `YYYY-MM-DD` as Serde
/// reads a date, in years 1 to 9999, the range of Python's `date`.
fn is_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    let shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !shaped {
        return false;
    }
    let number = |range: std::ops::Range<usize>| -> u32 {
        text[range]
            .parse()
            .expect("the shape check found only digits")
    };
    let (year, month, day) = (number(0..4), number(5..7), number(8..10));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    year >= 1 && (1..=days).contains(&day)
}

/// Report error details that TypeScript's declarations would misdescribe: they
/// cross as JSON, which reads a 64-bit integer as a `number`, not a `bigint`,
/// and a duration as Serde's object, not its nanoseconds.
pub(super) fn validate_error_details(
    items: &NamespaceItems<'_>,
    contract: &Contract<'_>,
    packages: &Packages<'_>,
    problems: &mut Vec<String>,
) -> Result<()> {
    if packages.typescript.is_none() {
        return Ok(());
    }
    for error in &items.errors {
        for code in &error.codes {
            let Some(details) = &code.details else {
                continue;
            };
            let model = TypeRef::Named {
                identity: details.clone(),
            };
            if contract.reaches(&model, |node| matches!(node, TypeRef::Int { bits: 64, .. }))? {
                problems.push(format!(
                    "the details of `{}` code `{}` hold a 64-bit integer, which JSON cannot carry to TypeScript exactly; use a smaller integer or a `String`",
                    error.name, code.code
                ));
            }
            if contract.reaches(&model, |node| matches!(node, TypeRef::Duration))? {
                problems.push(format!(
                    "the details of `{}` code `{}` hold a `Duration`, which JSON carries to TypeScript as Serde's object, not as the `bigint` its declarations say; use a count of a unit",
                    error.name, code.code
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bimetal_ir::Manifest;

    use super::*;
    use crate::fixtures;

    #[test]
    fn date_defaults_name_days_that_exist() {
        for day in [
            "2024-01-31",
            "2024-02-29",
            "2000-02-29",
            "0001-01-01",
            "9999-12-31",
        ] {
            assert!(is_iso_date(day), "{day} is a day");
        }
        for text in [
            "2024-02-30",
            "2023-02-29",
            "1900-02-29",
            "2024-13-01",
            "0000-01-01",
            "2024-1-01",
        ] {
            assert!(!is_iso_date(text), "{text} is not a day Python can hold");
        }
    }

    /// Return the problems of a contract with one resource that has `methods`.
    fn protocol_problems(methods: Vec<MethodDef>) -> Vec<String> {
        let mut manifest: Manifest = fixtures::recorder_manifest();
        manifest.resources[0].methods = methods;
        let contract = fixtures::contract(&manifest);
        let mut problems = Vec::new();
        for items in contract.namespaces().values() {
            validate_protocols(items, &mut problems);
        }
        problems
    }

    /// Return a method with a protocol, parameters, and a return.
    fn method(name: &str, protocol: Protocol, params: Vec<TypeRef>, returns: TypeRef) -> MethodDef {
        let mut method = fixtures::method(name, name);
        method.protocol = Some(protocol);
        method.returns = returns;
        method.callable.params = params
            .into_iter()
            .enumerate()
            .map(|(index, ty)| fixtures::typed_param(&format!("p{index}"), ty))
            .collect();
        method
    }

    fn optional(item: TypeRef) -> TypeRef {
        TypeRef::Option {
            item: Box::new(item),
        }
    }

    #[test]
    fn a_protocol_needs_its_signature() {
        let int = TypeRef::Int {
            signed: false,
            bits: 32,
        };
        let cases = [
            (
                method("len_of", Protocol::Len, vec![], TypeRef::String),
                "returns an integer",
            ),
            (
                method("show", Protocol::Repr, vec![int.clone()], TypeRef::String),
                "takes 0 parameters",
            ),
            (
                method("walk", Protocol::Next, vec![], TypeRef::String),
                "returns an `Option`",
            ),
            (
                method("done", Protocol::Exit, vec![TypeRef::String], TypeRef::Unit),
                "must be a `bool`",
            ),
            (
                method(
                    "has",
                    Protocol::Contains,
                    vec![TypeRef::String],
                    int.clone(),
                ),
                "returns a `bool`",
            ),
        ];
        for (method, expected) in cases {
            let problems = protocol_problems(vec![method]);
            assert!(
                problems.iter().any(|problem| problem.contains(expected)),
                "expected {expected:?} in {problems:?}"
            );
        }
    }

    #[test]
    fn signatures_that_fit_pass() {
        let methods = vec![
            method("walk", Protocol::Next, vec![], optional(TypeRef::String)),
            method("done", Protocol::Exit, vec![TypeRef::Bool], TypeRef::Unit),
            method("name", Protocol::Getter, vec![], TypeRef::String),
            method(
                "set_name",
                Protocol::Setter,
                vec![TypeRef::String],
                TypeRef::Unit,
            ),
        ];
        assert_eq!(protocol_problems(methods), Vec::<String>::new());
    }

    #[test]
    fn only_some_protocols_may_be_async() {
        let mut next = method("walk", Protocol::Next, vec![], optional(TypeRef::String));
        next.callable.is_async = true;
        assert_eq!(protocol_problems(vec![next]), Vec::<String>::new());
        let mut len = method(
            "size",
            Protocol::Len,
            vec![],
            TypeRef::Int {
                signed: false,
                bits: 32,
            },
        );
        len.callable.is_async = true;
        let problems = protocol_problems(vec![len]);
        assert!(problems[0].contains("cannot be async"), "{problems:?}");
    }

    #[test]
    fn a_class_has_a_protocol_once() {
        let first = method("a", Protocol::Next, vec![], optional(TypeRef::String));
        let second = method("b", Protocol::Next, vec![], optional(TypeRef::String));
        let problems = protocol_problems(vec![first, second]);
        assert!(
            problems[0].contains("2 `#[bimetal(next)]` methods"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_setter_needs_its_name_and_an_earlier_getter() {
        let problems = protocol_problems(vec![method(
            "rename",
            Protocol::Setter,
            vec![TypeRef::String],
            TypeRef::Unit,
        )]);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("must start with `set_`")),
            "{problems:?}"
        );
        let problems = protocol_problems(vec![
            method(
                "set_name",
                Protocol::Setter,
                vec![TypeRef::String],
                TypeRef::Unit,
            ),
            method("name", Protocol::Getter, vec![], TypeRef::String),
        ]);
        assert!(
            problems.iter().any(|problem| problem.contains("before it")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_method_cannot_share_a_name_with_what_a_protocol_defines() {
        let problems = protocol_problems(vec![
            method(
                "size",
                Protocol::Len,
                vec![],
                TypeRef::Int {
                    signed: false,
                    bits: 32,
                },
            ),
            fixtures::method("length", "length"),
        ]);
        assert!(problems[0].contains("defines `length`"), "{problems:?}");
    }

    /// Return the problems of a contract with one function that has `params`.
    fn default_problems(params: Vec<ParamDef>) -> Vec<String> {
        let mut manifest: Manifest = fixtures::manifest();
        let mut function = fixtures::function("search", "search", params);
        function.returns = TypeRef::Unit;
        manifest.functions.push(function);
        let contract = fixtures::contract(&manifest);
        let mut problems = Vec::new();
        for items in contract.namespaces().values() {
            validate_defaults(items, &contract, &mut problems).expect("resolve types");
        }
        problems
    }

    fn with_default(name: &str, ty: TypeRef, default: Value) -> ParamDef {
        ParamDef {
            default: Some(default),
            ..fixtures::typed_param(name, ty)
        }
    }

    #[test]
    fn a_default_must_fit_its_parameter() {
        let byte = TypeRef::Int {
            signed: false,
            bits: 8,
        };
        for (ty, default, fits) in [
            (byte.clone(), serde_json::json!(255), true),
            (byte.clone(), serde_json::json!(256), false),
            (byte, serde_json::json!(-1), false),
            (TypeRef::Bool, serde_json::json!(1), false),
            (TypeRef::Float { bits: 64 }, serde_json::json!(1), true),
            (optional(TypeRef::String), serde_json::json!("a"), true),
        ] {
            let problems = default_problems(vec![with_default("limit", ty, default.clone())]);
            assert_eq!(problems.is_empty(), fits, "{default}: {problems:?}");
        }
    }

    #[test]
    fn a_default_cannot_come_before_a_required_parameter() {
        let problems = default_problems(vec![
            with_default("limit", TypeRef::Bool, serde_json::json!(true)),
            fixtures::typed_param("query", TypeRef::String),
        ]);
        assert!(
            problems[0].contains("before a required parameter"),
            "{problems:?}"
        );
    }

    #[test]
    fn an_option_before_a_default_is_omissible_and_an_option_before_a_required_one_is_not() {
        let omissible = contract::omissible_params(&[
            fixtures::typed_param("query", TypeRef::String),
            fixtures::typed_param("fields", optional(TypeRef::String)),
            with_default("limit", TypeRef::Bool, serde_json::json!(true)),
        ]);
        assert_eq!(omissible, [false, true, false]);
        let omissible = contract::omissible_params(&[
            fixtures::typed_param("fields", optional(TypeRef::String)),
            fixtures::typed_param("query", TypeRef::String),
        ]);
        assert_eq!(omissible, [false, false]);
    }
}
