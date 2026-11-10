//! JavaScript conversion primitives used by generated WebAssembly wrappers.
//!
//! Failures cross as JavaScript exceptions that the generated wrappers can
//! tell apart: an untyped failure is an `Error`, a `TypeError` when an
//! argument has the wrong shape, or a `RangeError` when it breaks a field
//! rule, and a typed contract error is a plain payload object that the
//! wrapper turns into its error class.
//!
//! A panic traps the module instead, with a `WebAssembly.RuntimeError` that
//! carries no message, and leaves the module's locks, async executor, and
//! stack as they were partway through a call. The hook that [`record_panics`]
//! installs first hands the panic's message to the listener that the
//! generated JavaScript registered through [`on_panic`]. That JavaScript
//! reports the trap with the message, rejects the promises of the pending
//! async calls, whose futures cannot finish, and keeps every later call out
//! of the module.

use std::any::Any;
use std::cell::{Cell, OnceCell};
use std::fmt::Display;
use std::future::Future;
use std::panic::{self, PanicHookInfo};

use bimetal_ir::typescript_names;
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsError, JsValue};
use wasm_bindgen_futures::js_sys::{Array, Function, Object, Reflect};

pub use super::parameter::Parameter;
#[cfg(feature = "arrow-api")]
pub use super::wasm_arrow::{arrow_from_host, arrow_to_host};
use crate::ContractType;
use crate::convert::parameter;
use crate::runtime::{self, Callback, CallbackError, ContractError, Resource, rules};

#[wasm_bindgen]
extern "C" {
    /// JavaScript's `TypeError`, for an argument of the wrong shape.
    #[wasm_bindgen(js_name = TypeError)]
    type JsTypeError;

    #[wasm_bindgen(constructor, js_class = "TypeError")]
    fn new(message: &str) -> JsTypeError;

    /// JavaScript's `RangeError`, for an argument that breaks a field rule.
    #[wasm_bindgen(js_name = RangeError)]
    type JsRangeError;

    #[wasm_bindgen(constructor, js_class = "RangeError")]
    fn new(message: &str) -> JsRangeError;
}

/// Deserialize a JavaScript argument through Serde, and check its field
/// rules.
///
/// # Errors
///
/// Returns a JavaScript `TypeError` when the value does not match `T`, and a
/// `RangeError` when it breaks a field rule; both name the parameter.
pub fn from_host<T: DeserializeOwned + ContractType>(
    value: JsValue,
    parameter: &Parameter,
) -> Result<T, JsValue> {
    let decoded: T = serde_wasm_bindgen::from_value(value).map_err(|error| {
        let text = error.to_string();
        // serde-wasm-bindgen wraps each message in an `Error`, which `String()`
        // prefixes with its name.
        argument_error(&parameter.javascript_message(text.strip_prefix("Error: ").unwrap_or(&text)))
    })?;
    rules::checked(decoded).map_err(|violation| {
        JsRangeError::new(&parameter.javascript_message(&violation.to_string())).into()
    })
}

/// Convert an argument of the wrong shape into a JavaScript `TypeError`.
#[must_use]
pub(super) fn argument_error(message: &str) -> JsValue {
    JsTypeError::new(message).into()
}

/// Take a byte argument of exactly `N` bytes as a Rust array.
///
/// # Errors
///
/// Returns a JavaScript `TypeError` that names the parameter when the
/// argument has another length.
pub fn fixed_bytes_from_host<const N: usize>(
    bytes: Vec<u8>,
    parameter: &Parameter,
) -> Result<[u8; N], JsValue> {
    parameter::fixed_bytes(bytes)
        .map_err(|cause| argument_error(&parameter.javascript_message(&cause)))
}

/// Check the field rules of what `callable` returned, and serialize it into
/// JavaScript through Serde.
///
/// Maps become JavaScript `Map`s, which the generated `restoreHost` turns
/// into plain objects with an own property for each key. Assigning the keys
/// to an object here would lose a key such as `__proto__`, because the
/// assignment sets the object's prototype instead.
///
/// # Errors
///
/// Returns a JavaScript `Error` when the value breaks a field rule, which is
/// a bug of the Rust code, or when Serde cannot represent the value.
pub fn to_host<T: Serialize + ContractType>(value: &T, callable: &str) -> Result<JsValue, JsValue> {
    rules::checked_return(value, callable).map_err(|message| runtime_error(&message))?;
    let serializer = serde_wasm_bindgen::Serializer::new()
        .serialize_missing_as_null(true)
        .serialize_large_number_types_as_bigints(true);
    value.serialize(&serializer).map_err(JsValue::from)
}

/// Convert a typed contract error into the payload that the generated wrapper
/// turns into the error's class: `{bimetalErrorCode, message}`.
#[must_use]
pub fn contract_error(error: &impl ContractError) -> JsValue {
    let payload = Object::new();
    let properties = [
        (
            typescript_names::ERROR_CODE_PROPERTY,
            Some(error.code().to_owned()),
        ),
        ("message", Some(error.to_string())),
        (typescript_names::ERROR_DETAILS_PROPERTY, error.details()),
    ];
    for (key, value) in properties {
        if let Some(value) = value {
            // Setting a property of a new plain object cannot fail.
            let _ = Reflect::set(&payload, &JsValue::from(key), &JsValue::from(value));
        }
    }
    payload.into()
}

/// Convert an untyped wrapper failure, such as a closed resource, into a
/// JavaScript `Error` that carries its message and a stack.
#[must_use]
pub fn runtime_error(error: &impl Display) -> JsValue {
    JsError::new(&error.to_string()).into()
}

/// Install the panic hook that hands each panic's message to the listener of
/// [`on_panic`], then runs the hook that was installed before. The generated
/// start function calls this once, at instantiation.
pub fn record_panics() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        report_panic(&panic_message(info));
        previous(info);
    }));
}

/// Call `listener` with the message of every later panic, before the panic
/// traps the module. Only the first listener is kept.
pub fn on_panic(listener: JsValue) {
    let _ = PANIC_LISTENER.with(|registered| registered.set(Function::from(listener)));
}

thread_local! {
    /// The listener that [`on_panic`] registered.
    static PANIC_LISTENER: OnceCell<Function> = const { OnceCell::new() };
}

/// Run an async export's future and return its promise.
#[must_use]
pub fn promise<F>(future: F) -> JsValue
where
    F: Future<Output = Result<JsValue, JsValue>> + 'static,
{
    wasm_bindgen_futures::future_to_promise(future).into()
}

/// Call the listener that [`on_panic`] registered with a panic's message.
fn report_panic(message: &str) {
    // A hook must not panic itself, so an error that the listener throws is
    // dropped.
    let _ = PANIC_LISTENER.try_with(|listener| {
        listener
            .get()
            .map(|listener| listener.call1(&JsValue::UNDEFINED, &JsValue::from(message)))
    });
}

/// Describe a panic by its message and, when known, where it happened.
#[must_use]
fn panic_message(info: &PanicHookInfo<'_>) -> String {
    let payload = info
        .payload_as_str()
        .unwrap_or(runtime::PANIC_WITHOUT_MESSAGE);
    match info.location() {
        Some(location) => format!("{payload} (at {location})"),
        None => payload.to_owned(),
    }
}

/// A Rust type whose values JavaScript holds as objects of a wasm-bindgen
/// class, which `#[bimetal::export]` implements in the module.
pub trait HostResource: Sized + 'static {
    /// Make the JavaScript object that holds a new resource.
    fn into_host(value: Self) -> JsValue;
}

/// What a call that returns resources returns: one, an option of one, or a
/// list of them.
pub trait ResourceOutput {
    /// Make the JavaScript value.
    fn into_javascript(self) -> JsValue;
}

impl<T: HostResource> ResourceOutput for T {
    fn into_javascript(self) -> JsValue {
        T::into_host(self)
    }
}

impl<T: ResourceOutput> ResourceOutput for Option<T> {
    fn into_javascript(self) -> JsValue {
        self.map_or(JsValue::NULL, ResourceOutput::into_javascript)
    }
}

impl<T: ResourceOutput> ResourceOutput for Vec<T> {
    fn into_javascript(self) -> JsValue {
        self.into_iter()
            .map(ResourceOutput::into_javascript)
            .collect::<Array>()
            .into()
    }
}

/// Make the JavaScript value of a call's resources.
#[must_use]
pub fn resource_to_host<T: ResourceOutput>(value: T) -> JsValue {
    value.into_javascript()
}

thread_local! {
    /// The resource that a class's `__bimetal_share` left for
    /// [`resource_from_host`]. wasm-bindgen gives Rust no typed access to the
    /// Rust value behind a `JsValue`.
    static SHARED: Cell<Option<Box<dyn Any>>> = const { Cell::new(None) };
}

/// Leave a resource for the call into Rust that is reading its host object.
pub fn share<T: 'static>(resource: &Resource<T>) {
    SHARED.set(Some(Box::new(resource.clone())));
}

/// Run `reach` on the resource behind a JavaScript argument.
///
/// # Errors
///
/// Returns a JavaScript `TypeError` that names the parameter when the
/// argument is not an object of the resource's class.
pub fn resource_from_host<T: HostResource, R>(
    value: JsValue,
    parameter: &Parameter,
    reach: impl FnOnce(&Resource<T>) -> R,
) -> Result<R, JsValue> {
    let wrong_type = || -> JsValue {
        JsTypeError::new(
            &parameter
                .javascript_message(&format!("must be a {}", parameter::resource_name::<T>())),
        )
        .into()
    };
    let share = Reflect::get(&value, &JsValue::from("__bimetal_share"))
        .ok()
        .and_then(|share| share.dyn_into::<Function>().ok())
        .ok_or_else(wrong_type)?;
    share.call0(&value).map_err(|_| wrong_type())?;
    let shared = SHARED.take().ok_or_else(wrong_type)?;
    let resource = shared.downcast::<Resource<T>>().map_err(|_| wrong_type())?;
    Ok(reach(&resource))
}

/// Run `reach` on the resource behind a JavaScript argument that may be
/// `null` or `undefined`.
///
/// # Errors
///
/// Returns a JavaScript `TypeError` that names the parameter when the
/// argument is another kind of object.
pub fn optional_resource_from_host<T: HostResource, R>(
    value: JsValue,
    parameter: &Parameter,
    reach: impl FnOnce(&Resource<T>) -> R,
) -> Result<Option<R>, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    resource_from_host(value, parameter, reach).map(Some)
}

/// A JavaScript function, which a callback may keep because `wasm32` has one
/// thread.
struct SingleThreaded(Function);

// SAFETY: a Wasm module without threads never moves a value to another thread,
// and the addon, which has threads, does not use this type.
unsafe impl Send for SingleThreaded {}
// SAFETY: as above.
unsafe impl Sync for SingleThreaded {}

/// Keep a JavaScript function as a [`Callback`].
///
/// # Errors
///
/// Returns a JavaScript `TypeError` that names the parameter when the
/// argument is not a function.
pub fn callback_from_host<A, R>(
    value: JsValue,
    parameter: &Parameter,
) -> Result<Callback<A, R>, JsValue>
where
    A: Serialize + 'static,
    R: DeserializeOwned + ContractType + 'static,
{
    let function = value.dyn_into::<Function>().map_err(|_| -> JsValue {
        JsTypeError::new(&parameter.javascript_message("must be a function")).into()
    })?;
    let function = SingleThreaded(function);
    Ok(Callback::new(move |arguments: A| {
        call_javascript(&function.0, &arguments)
            .map_err(|error| CallbackError::new(exception_text(&error)))
    }))
}

/// Call a JavaScript function with the items of the tuple `arguments`.
fn call_javascript<A: Serialize, R: DeserializeOwned + ContractType>(
    function: &Function,
    arguments: &A,
) -> Result<R, JsValue> {
    let serializer = serde_wasm_bindgen::Serializer::new()
        .serialize_missing_as_null(true)
        .serialize_large_number_types_as_bigints(true);
    let arguments = arguments.serialize(&serializer).map_err(JsValue::from)?;
    // A tuple of no items serializes as `null`.
    let arguments: Array = if arguments.is_null() || arguments.is_undefined() {
        Array::new()
    } else {
        arguments.unchecked_into()
    };
    let result = function.apply(&JsValue::UNDEFINED, &arguments)?;
    let decoded: R = serde_wasm_bindgen::from_value(result).map_err(JsValue::from)?;
    rules::checked(decoded).map_err(|violation| runtime_error(&violation))
}

/// Return what `String(error)` says of a JavaScript exception, or its message
/// when it is not an object that can say.
fn exception_text(error: &JsValue) -> String {
    let message = Reflect::get(error, &JsValue::from("message")).ok();
    message
        .and_then(|message| message.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| "the JavaScript function threw".to_owned())
}
