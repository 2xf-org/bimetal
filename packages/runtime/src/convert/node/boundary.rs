//! The conversions that generated addon wrappers call for each argument and
//! result, and the errors that they throw.

use std::borrow::Borrow;
use std::fmt::Display;
use std::future::{self, Future};
use std::ops::Deref;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{OwnedRwLockReadGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::{de, ser};
use crate::convert::parameter;
pub use crate::convert::parameter::Parameter;
use crate::owned::Owned;
use crate::runtime::node::{self, Element, Env, Error, Value};
use crate::runtime::{Callback, ContractError, ReachedResources, Resource, ResourceError, rules};
use crate::{ContractType, crossing};

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

/// How a parameter that the call borrows, `&T`, crosses from JavaScript: as
/// its type's crossing, [`crossing::Serde`] or [`crossing::Resource`], says.
#[doc(hidden)]
pub trait Lend<T: ?Sized> {
    /// What a synchronous call decodes the argument into.
    type Decoded;
    /// What an async call holds the argument through once it has waited.
    type Waited;

    /// Decode the argument of a synchronous call: through Serde, or as a
    /// resource, which `reached` notes.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`from_host`] and [`resource_from_host`].
    fn decode(
        value: Value<'_>,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> Result<Self::Decoded, Error>;

    /// Lend the decoded argument to the call named `callable`, locking a
    /// resource's value as [`read`] does.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`read`].
    fn lock<'decoded>(
        decoded: &'decoded Self::Decoded,
        callable: &'static str,
    ) -> Result<impl Deref<Target = T> + use<'decoded, Self, T>, Error>
    where
        T: 'decoded;

    /// Decode the argument of an async call, as [`Lend::decode`] does. The
    /// future waits for a resource's value while another call holds it.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Lend::decode`].
    fn decode_async(
        value: Value<'_>,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> Result<impl Future<Output = Self::Waited> + use<Self, T>, Error>;
}

impl<T: ?Sized + Owned> Lend<T> for crossing::Serde
where
    T::Owned: DeserializeOwned + ContractType + Borrow<T>,
{
    type Decoded = T::Owned;
    type Waited = T::Owned;

    fn decode(
        value: Value<'_>,
        parameter: &Parameter,
        _: &mut ReachedResources,
    ) -> Result<T::Owned, Error> {
        from_host(value, parameter)
    }

    fn lock<'decoded>(
        decoded: &'decoded T::Owned,
        _: &'static str,
    ) -> Result<impl Deref<Target = T> + use<'decoded, T>, Error>
    where
        T: 'decoded,
    {
        Ok(decoded.borrow())
    }

    fn decode_async(
        value: Value<'_>,
        parameter: &Parameter,
        _: &mut ReachedResources,
    ) -> Result<impl Future<Output = T::Owned> + use<T>, Error> {
        from_host(value, parameter).map(future::ready)
    }
}

impl<T: HostResource> Lend<T> for crossing::Resource {
    type Decoded = Arc<RwLock<T>>;
    type Waited = OwnedRwLockReadGuard<T>;

    fn decode(
        value: Value<'_>,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> Result<Arc<RwLock<T>>, Error> {
        resource_from_host(value, parameter, reached, Resource::<T>::get)
    }

    fn lock<'decoded>(
        decoded: &'decoded Arc<RwLock<T>>,
        callable: &'static str,
    ) -> Result<impl Deref<Target = T> + use<'decoded, T>, Error>
    where
        T: 'decoded,
    {
        read(decoded, callable)
    }

    fn decode_async(
        value: Value<'_>,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> Result<impl Future<Output = OwnedRwLockReadGuard<T>> + use<T>, Error> {
        resource_from_host(value, parameter, reached, Resource::<T>::read_owned)
    }
}

/// How a parameter that the call takes, `T`, crosses from JavaScript: as its
/// type's crossing says.
#[doc(hidden)]
pub trait Take<T> {
    /// Decode the argument: through Serde, or as a resource, which `reached`
    /// notes and the call closes, moving its value out.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`from_host`] and [`resource_from_host`].
    fn decode(
        value: Value<'_>,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> Result<T, Error>;
}

impl<T: DeserializeOwned + ContractType> Take<T> for crossing::Serde {
    fn decode(
        value: Value<'_>,
        parameter: &Parameter,
        _: &mut ReachedResources,
    ) -> Result<T, Error> {
        from_host(value, parameter)
    }
}

impl<T: HostResource> Take<T> for crossing::Resource {
    fn decode(
        value: Value<'_>,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> Result<T, Error> {
        resource_from_host(value, parameter, reached, Resource::<T>::take)
    }
}

/// How what a call returns, `V`, crosses into JavaScript: a value of the
/// crossing, or an `Option` or a `Vec` of them.
#[doc(hidden)]
pub trait Return<V> {
    /// Make the JavaScript value of what `callable` returned: through Serde,
    /// as [`to_host`] does, or as the objects of new resources.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`to_host`], and of Node-API, which are
    /// pending.
    fn to_host<'env>(env: Env<'env>, value: V, callable: &str) -> Result<Value<'env>, Error>;
}

impl<V: Serialize + ContractType> Return<V> for crossing::Serde {
    fn to_host<'env>(env: Env<'env>, value: V, callable: &str) -> Result<Value<'env>, Error> {
        to_host(env, &value, callable)
    }
}

impl<V: ResourceOutput> Return<V> for crossing::Resource {
    fn to_host<'env>(env: Env<'env>, value: V, _: &str) -> Result<Value<'env>, Error> {
        value.into_javascript(env)
    }
}

/// Lock a resource's value for a synchronous call named `callable`, which
/// shares it with other readers.
///
/// # Errors
///
/// Returns a JavaScript `Error` when another call holds the value alone. The
/// call cannot wait: JavaScript has one thread, and inside a host function
/// the other call may be the one that called it.
pub fn read<'value, T>(
    shared: &'value RwLock<T>,
    callable: &'static str,
) -> Result<RwLockReadGuard<'value, T>, Error> {
    shared
        .try_read()
        .map_err(|_| runtime_error(&ResourceError::Busy(callable)))
}

/// Lock a resource's value for a synchronous call named `callable`, which
/// holds it alone.
///
/// # Errors
///
/// Returns a JavaScript `Error` when another call holds the value, as
/// [`read`] says.
pub fn write<'value, T>(
    shared: &'value RwLock<T>,
    callable: &'static str,
) -> Result<RwLockWriteGuard<'value, T>, Error> {
    shared
        .try_write()
        .map_err(|_| runtime_error(&ResourceError::Busy(callable)))
}

/// Reach the resource behind a JavaScript argument through `reach`, such as
/// [`Resource::get`], for the call that `parameter` names, and note it in
/// `reached`.
///
/// # Errors
///
/// Returns a `TypeError` that names the parameter when the argument is not an
/// object of the resource's class, and an `Error` when the call reached it
/// already or `reach` fails, such as on a closed resource.
pub fn resource_from_host<T: HostResource, R>(
    value: Value<'_>,
    parameter: &Parameter,
    reached: &mut ReachedResources,
    reach: impl FnOnce(&Resource<T>, &'static str) -> Result<R, ResourceError>,
) -> Result<R, Error> {
    let callable = parameter.callable;
    T::with_resource(value, |resource| {
        reached.add(resource, callable)?;
        reach(resource, callable)
    })?
    .ok_or_else(|| {
        Error::Type(
            parameter.javascript_message(&format!("must be a {}", parameter::resource_name::<T>())),
        )
    })?
    .map_err(|error| runtime_error(&error))
}

/// Reach the resource behind a JavaScript argument that may be `null` or
/// `undefined`, as [`resource_from_host`] does.
///
/// # Errors
///
/// Returns the errors of [`resource_from_host`].
pub fn optional_resource_from_host<T: HostResource, R>(
    value: Value<'_>,
    parameter: &Parameter,
    reached: &mut ReachedResources,
    reach: impl FnOnce(&Resource<T>, &'static str) -> Result<R, ResourceError>,
) -> Result<Option<R>, Error> {
    if value.is_nullish()? {
        return Ok(None);
    }
    resource_from_host(value, parameter, reached, reach).map(Some)
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
