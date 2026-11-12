//! The models and values that cross: the members and wire names of models,
//! options that cannot tell `None` from `Some`, constants that JSON cannot
//! hold, and Arrow streams, which TypeScript passes only to async calls.

use std::collections::BTreeSet;
use std::iter;

use anyhow::Result;
use bimetal_ir::python_names;
use bimetal_ir::{FieldDef, TypeRef, TypeShape, VariantPayload};
use serde_json::Value;

use crate::contract::{self, Contract, Item, NamespaceCallable, NamespaceItems};
use crate::python;

use super::contract::Packages;

/// The property that sets an object's prototype when JavaScript assigns it.
const PROTOTYPE_KEY: &str = "__proto__";

/// Report model members that collide after Python identifier normalization,
/// wire names that collide or that a host would misread, and tagged enums
/// that no value could cross.
pub(super) fn validate_type_names(
    items: &NamespaceItems<'_>,
    packages: &Packages<'_>,
    problems: &mut Vec<String>,
) {
    let python = packages.python.is_some();
    for definition in &items.types {
        let name = &definition.name;
        match &definition.shape {
            TypeShape::Struct { fields } => {
                let members = python_field_names(fields);
                validate_fields(name, fields, &members, packages, problems);
                if python {
                    validate_python_members(name, members.into_iter(), problems);
                }
            }
            TypeShape::StringEnum { variants } => {
                validate_unique_wire_names(
                    name,
                    "variants",
                    variants.iter().map(|variant| variant.wire_name.as_str()),
                    problems,
                );
                if python {
                    validate_python_members(
                        name,
                        variants
                            .iter()
                            .map(|variant| python_names::identifier(&variant.rust_name)),
                        problems,
                    );
                }
            }
            TypeShape::TaggedEnum { tag, variants } => {
                if variants.is_empty() {
                    problems.push(format!(
                        "tagged enum `{name}` has no variants, so no value can cross; add a variant"
                    ));
                }
                if packages.typescript.is_some() && tag == PROTOTYPE_KEY {
                    problems.push(format!(
                        "the tag `{tag}` of `{name}` would set the prototype of a JavaScript object; rename it"
                    ));
                }
                validate_unique_wire_names(
                    name,
                    "variants",
                    variants.iter().map(|variant| variant.wire_name.as_str()),
                    problems,
                );
                for variant in variants {
                    let owner = format!("{name}::{}", variant.rust_name);
                    let members = python_field_names(&variant.fields);
                    validate_fields(&owner, &variant.fields, &members, packages, problems);
                    if let Some(field) = variant.fields.iter().find(|field| field.wire_name == *tag)
                    {
                        problems.push(format!(
                            "field `{}` of `{owner}` crosses as `{tag}`, which is the tag of `{name}`; rename the field or the tag",
                            field.rust_name
                        ));
                    }
                    if python {
                        validate_python_members(
                            &owner,
                            iter::once(python::python_field_name(tag)).chain(members),
                            problems,
                        );
                    }
                }
            }
            TypeShape::ExternalEnum { variants } => {
                validate_unique_wire_names(
                    name,
                    "variants",
                    variants.iter().map(|variant| variant.wire_name.as_str()),
                    problems,
                );
                for variant in variants {
                    let owner = format!("{name}::{}", variant.rust_name);
                    let carries_data = variant.payload != VariantPayload::Unit;
                    if packages.typescript.is_some()
                        && carries_data
                        && variant.wire_name == PROTOTYPE_KEY
                    {
                        problems.push(format!(
                            "`{owner}` crosses under the key `{PROTOTYPE_KEY}`, which would set the prototype of a JavaScript object; rename the variant"
                        ));
                    }
                    if let VariantPayload::Fields { fields } = &variant.payload {
                        let members = python_field_names(fields);
                        // A model built by keyword, with its other fields
                        // left to their defaults, would read as the
                        // variant's wire form.
                        if python {
                            for (field, member) in fields.iter().zip(&members) {
                                if field.wire_name == variant.wire_name
                                    || *member == variant.wire_name
                                {
                                    problems.push(format!(
                                        "field `{}` of `{owner}` has the name of its variant, `{}`, so Python cannot tell a model built by keyword from the variant's wire form; rename the field or the variant",
                                        field.rust_name, variant.wire_name
                                    ));
                                }
                            }
                        }
                        validate_fields(&owner, fields, &members, packages, problems);
                        if python {
                            validate_python_members(&owner, members.into_iter(), problems);
                        }
                    }
                }
            }
            TypeShape::Alias { .. } => {}
        }
    }
}

/// Return the Python names of model fields, in order.
#[must_use]
fn python_field_names(fields: &[FieldDef]) -> Vec<String> {
    fields
        .iter()
        .map(|field| python::python_field_name(&field.rust_name))
        .collect()
}

/// Report the fields of one model or variant that a host would misread, given
/// their Python names, in order: fields whose wire names collide; for
/// TypeScript, a field that crosses as `__proto__`, because the conversion
/// assigns each field, and assigning `__proto__` sets the prototype instead;
/// and for Python, a field whose Python name is the wire name of another
/// field, because Pydantic looks a keyword up by wire name first, and so
/// would give that value to the other field.
fn validate_fields(
    owner: &str,
    fields: &[FieldDef],
    python_names: &[String],
    packages: &Packages<'_>,
    problems: &mut Vec<String>,
) {
    validate_unique_wire_names(
        owner,
        "fields",
        fields.iter().map(|field| field.wire_name.as_str()),
        problems,
    );
    for (field, python_name) in fields.iter().zip(python_names) {
        if packages.typescript.is_some() && field.wire_name == PROTOTYPE_KEY {
            problems.push(format!(
                "field `{}` of `{owner}` crosses as `{PROTOTYPE_KEY}`, which would set the prototype of a JavaScript object; rename it",
                field.rust_name
            ));
        }
        let other = fields
            .iter()
            .find(|other| other.wire_name == *python_name && other.rust_name != field.rust_name);
        if packages.python.is_some()
            && let Some(other) = other
        {
            problems.push(format!(
                "field `{}` of `{owner}` has the Python name `{python_name}`, which field `{}` crosses as, so Pydantic would read one for the other; rename one",
                field.rust_name, other.rust_name
            ));
        }
    }
}

/// Report wire names that two fields, or two variants, of `owner` share.
fn validate_unique_wire_names<'a>(
    owner: &str,
    members: &str,
    names: impl Iterator<Item = &'a str>,
    problems: &mut Vec<String>,
) {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            problems.push(format!(
                "{members} of `{owner}` collide at the wire name `{name}`; rename one"
            ));
        }
    }
}

/// Report collisions in one generated Python class's members.
fn validate_python_members(
    owner: &str,
    names: impl Iterator<Item = String>,
    problems: &mut Vec<String>,
) {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name.clone()) {
            problems.push(format!(
                "members of `{owner}` collide at the Python name `{name}`; rename one"
            ));
        }
    }
}

/// Report the declarations of one namespace whose types hold an `Option`
/// whose value can itself be null, such as `Option<Option<T>>`: `None` and
/// `Some(None)` both cross as `null`, so the hosts cannot tell them apart.
pub(super) fn validate_nullable_options(
    items: &NamespaceItems<'_>,
    callables: &[NamespaceCallable<'_>],
    contract: &Contract<'_>,
    problems: &mut Vec<String>,
) -> Result<()> {
    let mut check = |reference: &TypeRef, item: &dyn Fn() -> String| -> Result<()> {
        if holds_nullable_option(reference, contract)? {
            problems.push(format!(
                "{} holds an `Option` whose value can itself be null, such as `Option<Option<T>>` or `Option<()>`, so the hosts cannot tell `None` from `Some`; remove one `Option`",
                item()
            ));
        }
        Ok(())
    };
    for definition in &items.types {
        let name = &definition.name;
        match &definition.shape {
            TypeShape::Struct { fields } => {
                for field in fields {
                    check(&field.ty, &|| {
                        format!("field `{}` of `{name}`", field.rust_name)
                    })?;
                }
            }
            TypeShape::TaggedEnum { variants, .. } => {
                for variant in variants {
                    for field in &variant.fields {
                        check(&field.ty, &|| {
                            format!(
                                "field `{}` of `{name}::{}`",
                                field.rust_name, variant.rust_name
                            )
                        })?;
                    }
                }
            }
            TypeShape::ExternalEnum { variants } => {
                for variant in variants {
                    match &variant.payload {
                        VariantPayload::Unit => {}
                        VariantPayload::Value { ty } => check(ty, &|| {
                            format!("the value of `{name}::{}`", variant.rust_name)
                        })?,
                        VariantPayload::Fields { fields } => {
                            for field in fields {
                                check(&field.ty, &|| {
                                    format!(
                                        "field `{}` of `{name}::{}`",
                                        field.rust_name, variant.rust_name
                                    )
                                })?;
                            }
                        }
                    }
                }
            }
            TypeShape::Alias { target } => check(target, &|| format!("alias `{name}`"))?,
            TypeShape::StringEnum { .. } => {}
        }
    }
    for callable in callables {
        for param in &callable.callable.params {
            check(&param.ty, &|| {
                format!("parameter `{}` of {}", param.rust_name, callable.item)
            })?;
        }
        if let Some(returns) = callable.returns {
            check(returns, &|| format!("the return type of {}", callable.item))?;
        }
    }
    for constant in &items.constants {
        check(&constant.ty, &|| Item::Constant(constant).to_string())?;
    }
    Ok(())
}

/// Report the synchronous callables of one namespace that take a
/// `RecordBatchStream`. JavaScript writes its batches asynchronously, and a
/// synchronous call on the JavaScript thread could never wait for them.
pub(super) fn validate_typescript_streams(
    callables: &[NamespaceCallable<'_>],
    problems: &mut Vec<String>,
) {
    let synchronous = callables
        .iter()
        .filter(|callable| !callable.callable.is_async);
    for callable in synchronous {
        for param in &callable.callable.params {
            if contract::holds_stream(&param.ty) {
                problems.push(format!(
                    "parameter `{}` of {} is a `RecordBatchStream`, which JavaScript writes asynchronously, so TypeScript needs the call to be async; make it async, or remove `[typescript]` from bimetal.toml",
                    param.rust_name, callable.item
                ));
            }
        }
    }
}

/// Return whether `reference` holds an `Option` whose value can be null.
fn holds_nullable_option(reference: &TypeRef, contract: &Contract<'_>) -> Result<bool> {
    for node in contract::nodes(reference) {
        if let TypeRef::Option { item } = node
            && can_be_null(item, contract, contract.manifest.types.len())?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Return whether a value of `reference` can cross as `null`, following at
/// most `aliases` aliases, which ends a chain of aliases that leads back to
/// itself.
fn can_be_null(reference: &TypeRef, contract: &Contract<'_>, aliases: usize) -> Result<bool> {
    Ok(match reference {
        TypeRef::Unit | TypeRef::Json | TypeRef::Option { .. } => true,
        TypeRef::Named { identity } if aliases > 0 => {
            match &contract.type_definition(identity)?.shape {
                TypeShape::Alias { target } => can_be_null(target, contract, aliases - 1)?,
                _ => false,
            }
        }
        _ => false,
    })
}

/// Report constants that hold a NaN or an infinity. Constants cross as JSON,
/// which has neither, so Serde writes `null`, which would change the value.
pub(super) fn validate_constant_values(
    items: &NamespaceItems<'_>,
    contract: &Contract<'_>,
    problems: &mut Vec<String>,
) -> Result<()> {
    for constant in &items.constants {
        if has_non_finite_float(&constant.ty, &constant.value, contract)? {
            problems.push(format!(
                "{} holds a NaN or an infinity, which the generated packages cannot represent; use a finite value",
                Item::Constant(constant)
            ));
        }
    }
    Ok(())
}

/// Return whether `value`, the JSON of a value of `reference`, holds a float
/// that Serde wrote as `null` because it was NaN or infinite. Inside an
/// `Option`, such a float is indistinguishable from `None`.
fn has_non_finite_float(
    reference: &TypeRef,
    value: &Value,
    contract: &Contract<'_>,
) -> Result<bool> {
    let any = |references: &mut dyn Iterator<Item = (&TypeRef, &Value)>| -> Result<bool> {
        for (reference, value) in references {
            if has_non_finite_float(reference, value, contract)? {
                return Ok(true);
            }
        }
        Ok(false)
    };
    let in_fields = |fields: &[FieldDef]| {
        any(&mut fields
            .iter()
            .filter_map(|field| Some((&field.ty, value.get(&field.wire_name)?))))
    };
    match (reference, value) {
        (TypeRef::Float { .. }, Value::Null) => Ok(true),
        (TypeRef::Option { item }, value) if !value.is_null() => {
            has_non_finite_float(item, value, contract)
        }
        (TypeRef::List { item }, Value::Array(values)) => {
            any(&mut values.iter().map(|value| (&**item, value)))
        }
        (TypeRef::Map { value: item }, Value::Object(values)) => {
            any(&mut values.values().map(|value| (&**item, value)))
        }
        (TypeRef::Tuple { items }, Value::Array(values)) => any(&mut items.iter().zip(values)),
        (TypeRef::Named { identity }, value) => match &contract.type_definition(identity)?.shape {
            TypeShape::Alias { target } => has_non_finite_float(target, value, contract),
            TypeShape::Struct { fields } => in_fields(fields),
            TypeShape::TaggedEnum { tag, variants } => {
                let wire_name = value.get(tag).and_then(Value::as_str);
                match variants
                    .iter()
                    .find(|variant| Some(variant.wire_name.as_str()) == wire_name)
                {
                    Some(variant) => in_fields(&variant.fields),
                    None => Ok(false),
                }
            }
            TypeShape::ExternalEnum { variants } => {
                // A variant with data is an object whose only key is its name.
                let Some((key, payload)) = value
                    .as_object()
                    .filter(|object| object.len() == 1)
                    .and_then(|object| object.iter().next())
                else {
                    return Ok(false);
                };
                let carried = variants
                    .iter()
                    .find(|variant| variant.wire_name == *key)
                    .map(|variant| &variant.payload);
                match carried {
                    Some(VariantPayload::Value { ty }) => {
                        has_non_finite_float(ty, payload, contract)
                    }
                    Some(VariantPayload::Fields { fields }) => any(&mut fields
                        .iter()
                        .filter_map(|field| Some((&field.ty, payload.get(&field.wire_name)?)))),
                    Some(VariantPayload::Unit) | None => Ok(false),
                }
            }
            TypeShape::StringEnum { .. } => Ok(false),
        },
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_member_validation_reports_collisions() {
        let mut problems = Vec::new();
        validate_python_members(
            "Model",
            ["class_value".to_owned(), "class_value".to_owned()].into_iter(),
            &mut problems,
        );

        assert_eq!(
            problems,
            ["members of `Model` collide at the Python name `class_value`; rename one"]
        );
    }
}
