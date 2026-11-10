//! The conversions that generated addon wrappers call for each argument and
//! result, and the errors that they throw.

use std::fmt::Display;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{de, ser};
use crate::ContractType;
use crate::convert::parameter;
pub use crate::convert::parameter::Parameter;
use crate::runtime::node::{self, Element, Env, Error, Value};
use crate::runtime::{Callback, ContractError, Resource, rules};

/// Deserialize a JavaScript argument through Serde, as the Wasm module does,
/// and check its field rules.
///
/// # Errors
///
/// Returns a `TypeError` when the value does not match `T`, and a
/// `RangeError` when it breaks a field rule; both name the parameter. An
/// exception that JavaScript code threw while Rust read the value stays.
pub fn from_host<T: DeserializeOwned + ContractType>(
    value: Value<'_>,
    parameter: &Parameter,
) -> Result<T, Error> {
    let decoded: T = de::from_value(value).map_err(|error| match error {
        Error::Message(message) => Error::Type(parameter.javascript_message(&message)),
        other => other,
    })?;
    rules::checked(decoded)
        .map_err(|violation| Error::Range(parameter.javascript_message(&violation.to_string())))
}

/// Copy a `Uint8Array` argument into Rust-owned memory.
///
/// # Errors
///
/// Returns a `TypeError` that names the parameter when the argument is not a
/// `Uint8Array`.
pub fn bytes_from_host(value: Value<'_>, parameter: &Parameter) -> Result<Vec<u8>, Error> {
    buffer_from_host::<u8>(value, parameter)
}

/// Copy a `Uint8Array` argument of exactly `N` bytes into a Rust array.
///
/// # Errors
///
/// Returns the errors of [`bytes_from_host`], and a `TypeError` that names
/// the parameter when the argument has another length.
pub fn fixed_bytes_from_host<const N: usize>(
    value: Value<'_>,
    parameter: &Parameter,
) -> Result<[u8; N], Error> {
    parameter::fixed_bytes(bytes_from_host(value, parameter)?)
        .map_err(|cause| Error::Type(parameter.javascript_message(&cause)))
}

/// Copy a typed array argument of `T` into Rust-owned memory.
///
/// # Errors
///
/// Returns a `TypeError` that names the parameter when the argument is not
/// the typed array of `T`.
pub fn buffer_from_host<T: Element>(
    value: Value<'_>,
    parameter: &Parameter,
) -> Result<Vec<T>, Error> {
    value.typed_array_values()?.ok_or_else(|| {
        // As the generated `checkArgument` names the typed array.
        let array = T::ARRAY;
        let article = if array.starts_with('I') { "an" } else { "a" };
        Error::Type(parameter.javascript_message(&format!("must be {article} {array}")))
    })
}

/// Return bytes as a plain `Uint8Array`.
///
/// When Node-API cannot allocate it, the exception stays pending, and the
/// call throws it, or its promise rejects with it, instead of this value.
#[must_use]
pub fn bytes_to_host<'env>(env: Env<'env>, value: &[u8]) -> Value<'env> {
    buffer_to_host(env, value)
}

/// Return a numeric buffer as the plain typed array of `T`, as
/// [`bytes_to_host`] returns bytes.
#[must_use]
pub fn buffer_to_host<'env, T: Element>(env: Env<'env>, value: &[T]) -> Value<'env> {
    env.typed_array(value).unwrap_or_else(|_| env.undefined())
}

/// Check the field rules of what `callable` returned, and serialize it into
/// JavaScript through Serde, as the Wasm module does.
///
/// # Errors
///
/// Returns a JavaScript `Error` when the value breaks a field rule, which is
/// a bug of the Rust code, or when Serde cannot represent the value.
pub fn to_host<'env, T: Serialize + ContractType>(
    env: Env<'env>,
    value: &T,
    callable: &str,
) -> Result<Value<'env>, Error> {
    rules::checked_return(value, callable).map_err(Error::Message)?;
    value.serialize(&ser::Serializer::new(env))
}

/// Convert a typed contract error into the payload that the generated wrapper
/// turns into the error's class.
pub fn contract_error(error: &impl ContractError) -> Error {
    Error::Contract {
        code: error.code(),
        message: error.to_string(),
        details: error.details(),
    }
}

/// Convert an untyped wrapper failure, such as a closed resource, into a
/// JavaScript `Error` that carries its message and a stack.
pub fn runtime_error(error: &impl Display) -> Error {
    Error::Message(error.to_string())
}

/// The JavaScript `null`, what a call that may return no direct value returns.
///
/// # Errors
///
/// Returns the error of Node-API, which is pending.
pub fn none(env: Env<'_>) -> Result<Value<'_>, Error> {
    env.null()
}

/// A Rust type whose values JavaScript holds as objects of a native class,
/// which `#[bimetal::export]` implements in the addon.
pub trait HostResource: Sized + 'static {
    /// Make the JavaScript object that holds a new resource.
    ///
    /// # Errors
    ///
    /// Returns the error of the class's constructor, which is pending.
    fn into_host<'env>(env: Env<'env>, value: Self) -> Result<Value<'env>, Error>;

    /// Run `reach` on the resource behind `value`, or return `None` when
    /// `value` is not an object of this class.
    ///
    /// # Errors
    ///
    /// Returns the error of Node-API, which is pending.
    fn with_resource<R>(
        value: Value<'_>,
        reach: impl FnOnce(&Resource<Self>) -> R,
    ) -> Result<Option<R>, Error>;
}

/// What a call that returns resources returns: one, an option of one, or a
/// list of them.
pub trait ResourceOutput {
    /// Make the JavaScript value.
    ///
    /// # Errors
    ///
    /// Returns the error of Node-API, which is pending.
    fn into_javascript(self, env: Env<'_>) -> Result<Value<'_>, Error>;
}

impl<T: HostResource> ResourceOutput for T {
    fn into_javascript(self, env: Env<'_>) -> Result<Value<'_>, Error> {
        T::into_host(env, self)
    }
}

impl<T: ResourceOutput> ResourceOutput for Option<T> {
    fn into_javascript(self, env: Env<'_>) -> Result<Value<'_>, Error> {
        match self {
            Some(value) => value.into_javascript(env),
            None => env.null(),
        }
    }
}

impl<T: ResourceOutput> ResourceOutput for Vec<T> {
    fn into_javascript(self, env: Env<'_>) -> Result<Value<'_>, Error> {
        let items = env.array()?;
        for (index, value) in self.into_iter().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| Error::Message("too many resources for an array".to_owned()))?;
            items.set_element(index, value.into_javascript(env)?)?;
        }
        Ok(items)
    }
}

/// Make the JavaScript value of a call's resources.
///
/// # Errors
///
/// Returns the error of Node-API, which is pending.
pub fn resource_to_host<T: ResourceOutput>(env: Env<'_>, value: T) -> Result<Value<'_>, Error> {
    value.into_javascript(env)
}

/// Run `reach` on the resource behind a JavaScript argument.
///
/// # Errors
///
/// Returns a `TypeError` that names the parameter when the argument is not an
/// object of the resource's class.
pub fn resource_from_host<T: HostResource, R>(
    value: Value<'_>,
    parameter: &Parameter,
    reach: impl FnOnce(&Resource<T>) -> R,
) -> Result<R, Error> {
    T::with_resource(value, reach)?.ok_or_else(|| {
        Error::Type(
            parameter.javascript_message(&format!("must be a {}", parameter::resource_name::<T>())),
        )
    })
}

/// Run `reach` on the resource behind a JavaScript argument that may be
/// `null` or `undefined`.
///
/// # Errors
///
/// Returns a `TypeError` that names the parameter when the argument is
/// another kind of object.
pub fn optional_resource_from_host<T: HostResource, R>(
    value: Value<'_>,
    parameter: &Parameter,
    reach: impl FnOnce(&Resource<T>) -> R,
) -> Result<Option<R>, Error> {
    if value.is_nullish()? {
        return Ok(None);
    }
    resource_from_host(value, parameter, reach).map(Some)
}

/// Keep a JavaScript function as a [`Callback`].
///
/// # Errors
///
/// Returns a `TypeError` that names the parameter when the argument is not a
/// function, and the error of Node-API, which is pending, when it cannot keep
/// the function.
pub fn callback_from_host<A, R>(
    value: Value<'_>,
    parameter: &Parameter,
) -> Result<Callback<A, R>, Error>
where
    A: Serialize + Send + 'static,
    R: DeserializeOwned + ContractType + Send + 'static,
{
    if !value.is_function()? {
        return Err(Error::Type(
            parameter.javascript_message("must be a function"),
        ));
    }
    node::callback(value, prepare_arguments::<A>, decode_result::<R>)
}

/// Convert a callback's arguments.
fn prepare_arguments<'env, A: Serialize>(
    env: Env<'env>,
    arguments: &A,
) -> Result<Value<'env>, Error> {
    arguments.serialize(&ser::Serializer::new(env))
}

/// Convert a callback's result, and check its field rules.
fn decode_result<R: DeserializeOwned + ContractType>(value: Value<'_>) -> Result<R, Error> {
    let decoded: R = de::from_value(value)?;
    rules::checked(decoded).map_err(|violation| Error::Message(violation.to_string()))
}
