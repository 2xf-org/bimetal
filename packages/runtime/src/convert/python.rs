//! Python conversion primitives used by generated native wrappers.

use std::borrow::Borrow;
use std::ffi::CStr;
use std::fmt::Display;
use std::future::{self, Future};
use std::ops::Deref;
use std::sync::Arc;
use std::{mem, ptr, slice};

use bimetal_ir::BufferElement;
use pyo3::buffer::{ElementType, PyUntypedBuffer};
use pyo3::exceptions::{PyBufferError, PyException, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyList, PyString, PyTuple};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{OwnedRwLockReadGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub use super::parameter::Parameter;
#[cfg(feature = "arrow-api")]
pub use super::python_arrow::{arrow_from_host, arrow_to_host};
#[doc(hidden)]
pub use super::python_plan::ValuePlan;
use crate::convert::python_deserializer::{Dumped, Revalidate};
use crate::convert::{parameter, python_deserializer, python_plan, python_serializer};
use crate::owned::Owned;
use crate::runtime::{
    self, Callback, CallbackArguments, CallbackError, ContractError, ReachedResources, Resource,
    ResourceError, executor, rules,
};
use crate::{ContractType, crossing};

impl Parameter {
    /// Name this parameter in the error that reading its argument raised.
    ///
    /// The error stays a `TypeError` for a value of the wrong type and a
    /// `BufferError` for a buffer of the wrong layout, and becomes a
    /// `ValueError` otherwise: the type is right, but the value is not. An
    /// exception that is not an `Exception`, such as the `KeyboardInterrupt`
    /// of a Ctrl-C while Python code reads the argument, passes unchanged.
    #[cold]
    #[must_use]
    pub fn error(&self, py: Python<'_>, error: PyErr) -> PyErr {
        if !error.is_instance_of::<PyException>(py) {
            return error;
        }
        let cause = error.value(py).to_string();
        // pythonize prefixes the message of a value of the wrong type.
        let message =
            self.python_message(cause.strip_prefix("unexpected type: ").unwrap_or(&cause));
        if error.is_instance_of::<PyTypeError>(py) {
            PyTypeError::new_err(message)
        } else if error.is_instance_of::<PyBufferError>(py) {
            PyBufferError::new_err(message)
        } else {
            PyValueError::new_err(message)
        }
    }
}

/// Deserialize a Python argument through Serde, by `plan`, which the call
/// site keeps, and check its field rules.
///
/// A model instance of its generated class, a `datetime` and a `date` are
/// read as they are. An argument that the plan cannot read as it is, such as
/// a dictionary passed for a model, raises `Revalidate`, and the generated
/// code calls again with Pydantic's dump inside a `Dumped`, which is read as
/// `pythonize` reads it.
///
/// # Errors
///
/// Returns `TypeError` when the value has the wrong type for `T`, and
/// `ValueError` when it has the right type but an invalid value, such as one
/// that breaks a field rule; both name the parameter. Returns `Revalidate`
/// for an argument that needs Pydantic's dump.
pub fn from_host<T: DeserializeOwned + ContractType>(
    value: &Bound<'_, PyAny>,
    plan: &ValuePlan,
    parameter: &Parameter,
) -> PyResult<T> {
    let py = value.py();
    // `Dumped` has no subclasses, so its exact type tells it apart.
    let dumped = value
        .cast_exact::<Dumped>()
        .ok()
        .map(|dumped| dumped.get().0.bind(py).clone());
    let value = dumped.as_ref().unwrap_or(value);
    let native = plan
        .reads::<T>()
        .filter(|_| dumped.is_none() && python_plan::classes(py).is_some());
    let decoded: T = if let Some(plan) = native {
        python_deserializer::from_python(value, plan).map_err(|error| match error.0 {
            Some(error) if !error.is_instance_of::<PyException>(py) => error,
            _ => Revalidate::new_err(()),
        })?
    } else {
        read_plain(value).map_err(|error| parameter.error(py, error))?
    };
    rules::checked(decoded)
        .map_err(|violation| parameter.error(py, PyValueError::new_err(violation.to_string())))
}

/// Read `value` as `pythonize` does.
fn read_plain<T: DeserializeOwned + ContractType>(value: &Bound<'_, PyAny>) -> PyResult<T> {
    // pythonize would read a `str` as the list of its characters.
    if let Some(sequence) = T::SEQUENCE
        && value.is_instance_of::<PyString>()
    {
        return Err(PyTypeError::new_err(format!("must be {sequence}, not str")));
    }
    Ok(pythonize::depythonize(value)?)
}

/// Copy a Python bytes argument into Rust-owned memory.
///
/// # Errors
///
/// Returns `TypeError` when `value` is not a bytes-like object, and
/// `BufferError` when it is not a one-dimensional, C-contiguous byte buffer;
/// both name the parameter.
pub fn bytes_from_host(value: &Bound<'_, PyAny>, parameter: &Parameter) -> PyResult<Vec<u8>> {
    if let Ok(bytes) = value.cast::<PyBytes>() {
        return Ok(bytes.as_bytes().to_vec());
    }
    read_buffer(value).map_err(|error| parameter.error(value.py(), error))
}

/// Copy a Python bytes argument of exactly `N` bytes into a Rust array.
///
/// # Errors
///
/// Returns the errors of [`bytes_from_host`], and `ValueError` when the
/// argument has another length.
pub fn fixed_bytes_from_host<const N: usize>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
) -> PyResult<[u8; N]> {
    parameter::fixed_bytes(bytes_from_host(value, parameter)?)
        .map_err(|message| parameter.error(value.py(), PyValueError::new_err(message)))
}

/// Copy a typed Python buffer argument into Rust-owned memory.
///
/// # Errors
///
/// Returns `TypeError` when `value` is not a buffer, and `BufferError`
/// unless it is a one-dimensional, C-contiguous, native-endian buffer with
/// the exact element type `T`; both name the parameter.
pub fn buffer_from_host<T: PythonBufferElement>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
) -> PyResult<Vec<T>> {
    let py = value.py();
    read_buffer(value).map_err(|error| {
        if !error.is_instance_of::<PyTypeError>(py) {
            return parameter.error(py, error);
        }
        let error = PyTypeError::new_err(format!(
            "must be a buffer of {} values, such as a NumPy array, not {}",
            T::NUMPY,
            value
                .get_type()
                .qualname()
                .map_or_else(|_| "another type".to_owned(), |name| name.to_string())
        ));
        parameter.error(py, error)
    })
}

/// Copy a one-dimensional, C-contiguous buffer of native-endian `T` values
/// into Rust-owned memory.
fn read_buffer<T: PythonBufferElement>(value: &Bound<'_, PyAny>) -> PyResult<Vec<T>> {
    let buffer = PyUntypedBuffer::get(value)?;
    if buffer.dimensions() != 1 {
        return Err(PyBufferError::new_err("must be one-dimensional"));
    }
    if !buffer.is_c_contiguous() {
        return Err(PyBufferError::new_err("must be C-contiguous"));
    }
    if buffer.item_size() != mem::size_of::<T>() || !holds_native::<T>(buffer.format()) {
        return Err(incompatible_buffer::<T>(value, buffer.format()));
    }
    let count = buffer.item_count();
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut values: Vec<T> = Vec::with_capacity(count);
    // pyo3's own typed copy would apply pyo3 0.29's format check, which reads
    // a big-endian buffer as native and rejects an explicit `<` on a
    // little-endian machine, so this copies the checked values itself.
    // SAFETY: the checks above make the buffer `count` contiguous values of
    // `T`'s size, kind, and byte order at `buf_ptr()`, which `buffer` keeps
    // alive, and the interpreter lock keeps Python code from changing them.
    // Every bit pattern is a valid `T`, a plain numeric primitive, and the
    // byte copy needs no alignment. It fills the new vector's capacity, which
    // nothing else references, before `set_len`.
    unsafe {
        ptr::copy_nonoverlapping(
            buffer.buf_ptr().cast::<u8>(),
            values.as_mut_ptr().cast::<u8>(),
            buffer.len_bytes(),
        );
        values.set_len(count);
    }
    Ok(values)
}

/// Return whether a buffer's struct-module format names `T` in this
/// machine's byte order.
fn holds_native<T: PythonBufferElement>(format: &CStr) -> bool {
    let native = match format.to_bytes().first() {
        Some(b'<') => cfg!(target_endian = "little"),
        Some(b'>' | b'!') => cfg!(target_endian = "big"),
        _ => true,
    };
    native && ElementType::from_format(format) == T::ELEMENT
}

/// Report a buffer whose values are not native-endian `T`, naming what it
/// holds instead: the NumPy dtype, or else the buffer's format.
#[cold]
fn incompatible_buffer<T: PythonBufferElement>(value: &Bound<'_, PyAny>, format: &CStr) -> PyErr {
    let numpy = T::NUMPY;
    let message = match value.getattr(intern!(value.py(), "dtype")) {
        Ok(dtype) => format!(
            "must hold native-endian {numpy} values, not {dtype:?}; convert it with `.astype('{numpy}')`"
        ),
        Err(_) => format!(
            "must hold native-endian {numpy} values, not values of the buffer format '{}'",
            format.to_string_lossy()
        ),
    };
    PyBufferError::new_err(message)
}

/// Return bytes through Python's native immutable byte representation.
#[must_use]
pub fn bytes_to_host(py: Python<'_>, value: &[u8]) -> Py<PyAny> {
    PyBytes::new(py, value).into_any().unbind()
}

mod sealed {
    pub trait Sealed {}
}

/// A primitive element supported by a Bimetal numeric buffer.
#[doc(hidden)]
pub trait PythonBufferElement: sealed::Sealed + Copy {
    /// The NumPy name of the element type, such as `float64`.
    const NUMPY: &'static str;
    /// The kind and size of the element in a buffer's format.
    const ELEMENT: ElementType;
}

macro_rules! buffer_element {
    ($($ty:ty => $element:ident, $kind:ident),+ $(,)?) => {
        $(
            impl sealed::Sealed for $ty {}
            impl PythonBufferElement for $ty {
                const NUMPY: &'static str = BufferElement::$element.numpy_name();
                const ELEMENT: ElementType = ElementType::$kind {
                    bytes: mem::size_of::<$ty>(),
                };
            }
        )+
    };
}

buffer_element!(
    u8 => U8, UnsignedInteger,
    i8 => I8, SignedInteger,
    u16 => U16, UnsignedInteger,
    i16 => I16, SignedInteger,
    u32 => U32, UnsignedInteger,
    i32 => I32, SignedInteger,
    u64 => U64, UnsignedInteger,
    i64 => I64, SignedInteger,
    f32 => F32, Float,
    f64 => F64, Float,
);

/// Return a numeric buffer as a `bytearray` of native-endian values.
///
/// Generated Python wraps it with `numpy.frombuffer`. A `bytearray` is
/// writable, so the array needs no copy of its own.
#[must_use]
pub fn buffer_to_host<T: PythonBufferElement>(py: Python<'_>, value: &[T]) -> Py<PyAny> {
    let length = mem::size_of_val(value);
    // Viewing the values as bytes lets Python copy them once. A safe
    // conversion would first copy every value into a new byte vector, a second
    // full copy of a buffer that can be large.
    // SAFETY: every `PythonBufferElement` is a plain numeric primitive without
    // padding, so all of its bytes are initialized, and the byte slice cannot
    // outlive `value` during this call.
    let bytes = unsafe { slice::from_raw_parts(value.as_ptr().cast::<u8>(), length) };
    PyByteArray::new(py, bytes).into_any().unbind()
}

/// Check the field rules of what `callable` returned, and serialize it into
/// Python through Serde, by `plan`, which the call site keeps: a model
/// becomes an instance of its generated class, and a date-time a `datetime`,
/// as Pydantic would validate them, so the generated code validates nothing.
/// An extension without its generated package makes plain values, as
/// `pythonize` does.
///
/// # Errors
///
/// Returns `RuntimeError` when the value breaks a field rule, which is a bug
/// of the Rust code, and `ValueError` when Serde cannot represent the value
/// in Python.
pub fn to_host<T: Serialize + ContractType>(
    py: Python<'_>,
    plan: &ValuePlan,
    value: &T,
    callable: &str,
) -> PyResult<Py<PyAny>> {
    rules::checked_return(value, callable).map_err(PyRuntimeError::new_err)?;
    make(py, plan.of::<T>(), value).map(Bound::unbind)
}

/// Make the Python value of `value` by `plan`, or as `pythonize` does when
/// there is none, for a value that reaches a numeric buffer, or when the
/// extension has no generated package.
fn make<'py, T: Serialize + ?Sized>(
    py: Python<'py>,
    plan: Option<&python_plan::Plan>,
    value: &T,
) -> PyResult<Bound<'py, PyAny>> {
    match plan {
        Some(plan) if python_plan::classes(py).is_some() => {
            python_serializer::to_python(py, plan, value)
        }
        _ => pythonize::pythonize(py, value)
            .map_err(|error| PyValueError::new_err(error.to_string())),
    }
}

/// Convert a typed contract error into the generated package's exception
/// class for it, with its details as their model, or, for an extension
/// without a generated package, into a `RuntimeError` of its code, message
/// and details.
#[must_use]
pub fn contract_error<E: ContractError + 'static>(error: &E) -> PyErr {
    let (code, message, details) = (error.code(), error.to_string(), error.details());
    Python::attach(|py| {
        let class = python_plan::error_class::<E>(py)?;
        Some(typed_error(&class, code, &message, details.as_deref()))
    })
    .unwrap_or_else(|| PyRuntimeError::new_err((code, message, details)))
}

/// An instance of the exception class `class`, or the error of making it.
fn typed_error(
    class: &Bound<'_, PyAny>,
    code: &str,
    message: &str,
    details: Option<&str>,
) -> PyErr {
    error_instance(class, code, message, details).map_or_else(|error| error, PyErr::from_value)
}

/// An instance of the exception class `class`, made as its generated
/// `__init__` makes it, without the call: its message as its one argument,
/// its code, and, for a class that declares details, its details as their
/// model.
fn error_instance<'py>(
    class: &Bound<'py, PyAny>,
    code: &str,
    message: &str,
    details: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = class.py();
    let instance = class.call_method1(intern!(py, "__new__"), (class, message))?;
    instance.setattr(intern!(py, "code"), code)?;
    if let Ok(models) = class.getattr(intern!(py, "__bimetal_details__")) {
        let details = match (details, models.get_item(code)) {
            (Some(details), Ok(model)) => {
                model.call_method1(intern!(py, "model_validate_json"), (details,))?
            }
            _ => py.None().into_bound(py),
        };
        instance.setattr(intern!(py, "details"), details)?;
    }
    Ok(instance)
}

/// Convert an untyped wrapper failure, such as a closed resource, into a
/// `RuntimeError` that carries only its message.
#[must_use]
pub fn runtime_error(error: &impl Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

/// Python's `None`, what a call that may return no direct value returns.
///
/// # Errors
///
/// Never; the signature matches the conversions it stands in for.
pub fn none(py: Python<'_>) -> PyResult<Py<PyAny>> {
    Ok(py.None())
}

/// A Rust type whose values Python holds as objects of a native class, which
/// `#[bimetal::export]` implements in the extension.
pub trait HostResource: Sized + Send + Sync + 'static {
    /// Make the Python object that holds a new resource.
    ///
    /// # Errors
    ///
    /// Returns the error of Python's allocation.
    fn into_host(py: Python<'_>, value: Self) -> PyResult<Py<PyAny>>;

    /// Run `reach` on the resource behind `value`, or return `None` when
    /// `value` is not an object of this class.
    fn with_resource<R>(
        value: &Bound<'_, PyAny>,
        reach: impl FnOnce(&Resource<Self>) -> R,
    ) -> Option<R>;
}

/// What a call that returns resources returns: one, an option of one, or a
/// list of them.
pub trait ResourceOutput {
    /// Make the Python value.
    ///
    /// # Errors
    ///
    /// Returns the error of Python's allocation.
    fn into_python(self, py: Python<'_>) -> PyResult<Py<PyAny>>;
}

impl<T: HostResource> ResourceOutput for T {
    fn into_python(self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        T::into_host(py, self)
    }
}

impl<T: ResourceOutput> ResourceOutput for Option<T> {
    fn into_python(self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        self.map_or_else(|| Ok(py.None()), |value| value.into_python(py))
    }
}

impl<T: ResourceOutput> ResourceOutput for Vec<T> {
    fn into_python(self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let items = self
            .into_iter()
            .map(|value| value.into_python(py))
            .collect::<PyResult<Vec<Py<PyAny>>>>()?;
        Ok(PyList::new(py, items)?.into_any().unbind())
    }
}

/// How a parameter that the call borrows, `&T`, crosses from Python: as its
/// type's crossing, [`crossing::Serde`] or [`crossing::Resource`], says.
#[doc(hidden)]
pub trait Lend<T: ?Sized> {
    /// What a synchronous call decodes the argument into.
    type Decoded;
    /// What an async call holds the argument through once it has waited.
    type Waited;

    /// Decode the argument of a synchronous call: through Serde by `plan`,
    /// or as a resource, which `reached` notes.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`from_host`] and [`resource_from_host`].
    fn decode(
        value: &Bound<'_, PyAny>,
        plan: &ValuePlan,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> PyResult<Self::Decoded>;

    /// Lend the decoded argument to the call named `callable`, locking a
    /// resource's value as [`read`] does.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`read`].
    fn lock<'decoded>(
        py: Python<'_>,
        decoded: &'decoded Self::Decoded,
        callable: &'static str,
    ) -> PyResult<impl Deref<Target = T> + use<'decoded, Self, T>>
    where
        T: 'decoded;

    /// Decode the argument of an async call, as [`Lend::decode`] does. The
    /// future waits for a resource's value while another call holds it.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Lend::decode`].
    fn decode_async(
        value: &Bound<'_, PyAny>,
        plan: &ValuePlan,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> PyResult<impl Future<Output = Self::Waited> + use<Self, T>>;
}

impl<T: ?Sized + Owned> Lend<T> for crossing::Serde
where
    T::Owned: DeserializeOwned + ContractType + Borrow<T>,
{
    type Decoded = T::Owned;
    type Waited = T::Owned;

    fn decode(
        value: &Bound<'_, PyAny>,
        plan: &ValuePlan,
        parameter: &Parameter,
        _: &mut ReachedResources,
    ) -> PyResult<T::Owned> {
        from_host(value, plan, parameter)
    }

    fn lock<'decoded>(
        _: Python<'_>,
        decoded: &'decoded T::Owned,
        _: &'static str,
    ) -> PyResult<impl Deref<Target = T> + use<'decoded, T>>
    where
        T: 'decoded,
    {
        Ok(decoded.borrow())
    }

    fn decode_async(
        value: &Bound<'_, PyAny>,
        plan: &ValuePlan,
        parameter: &Parameter,
        _: &mut ReachedResources,
    ) -> PyResult<impl Future<Output = T::Owned> + use<T>> {
        from_host(value, plan, parameter).map(future::ready)
    }
}

impl<T: HostResource> Lend<T> for crossing::Resource {
    type Decoded = Arc<RwLock<T>>;
    type Waited = OwnedRwLockReadGuard<T>;

    fn decode(
        value: &Bound<'_, PyAny>,
        _: &ValuePlan,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> PyResult<Arc<RwLock<T>>> {
        resource_from_host(value, parameter, reached, Resource::<T>::get)
    }

    fn lock<'decoded>(
        py: Python<'_>,
        decoded: &'decoded Arc<RwLock<T>>,
        callable: &'static str,
    ) -> PyResult<impl Deref<Target = T> + use<'decoded, T>>
    where
        T: 'decoded,
    {
        read(py, decoded, callable)
    }

    fn decode_async(
        value: &Bound<'_, PyAny>,
        _: &ValuePlan,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> PyResult<impl Future<Output = OwnedRwLockReadGuard<T>> + use<T>> {
        resource_from_host(value, parameter, reached, Resource::<T>::read_owned)
    }
}

/// How a parameter that the call takes, `T`, crosses from Python: as its
/// type's crossing says.
#[doc(hidden)]
pub trait Take<T> {
    /// Decode the argument: through Serde by `plan`, or as a resource, which
    /// `reached` notes and the call closes, moving its value out.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`from_host`] and [`resource_from_host`].
    fn decode(
        value: &Bound<'_, PyAny>,
        plan: &ValuePlan,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> PyResult<T>;
}

impl<T: DeserializeOwned + ContractType> Take<T> for crossing::Serde {
    fn decode(
        value: &Bound<'_, PyAny>,
        plan: &ValuePlan,
        parameter: &Parameter,
        _: &mut ReachedResources,
    ) -> PyResult<T> {
        from_host(value, plan, parameter)
    }
}

impl<T: HostResource> Take<T> for crossing::Resource {
    fn decode(
        value: &Bound<'_, PyAny>,
        _: &ValuePlan,
        parameter: &Parameter,
        reached: &mut ReachedResources,
    ) -> PyResult<T> {
        resource_from_host(value, parameter, reached, Resource::<T>::take)
    }
}

/// How what a call returns, `V`, crosses into Python: a value of the
/// crossing, or an `Option` or a `Vec` of them.
#[doc(hidden)]
pub trait Return<V> {
    /// Make the Python value of what `callable` returned: through Serde by
    /// `plan`, as [`to_host`] does, or as the objects of new resources.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`to_host`], and of Python's allocation.
    fn to_host(py: Python<'_>, plan: &ValuePlan, value: V, callable: &str) -> PyResult<Py<PyAny>>;
}

impl<V: Serialize + ContractType> Return<V> for crossing::Serde {
    fn to_host(py: Python<'_>, plan: &ValuePlan, value: V, callable: &str) -> PyResult<Py<PyAny>> {
        to_host(py, plan, &value, callable)
    }
}

impl<V: ResourceOutput> Return<V> for crossing::Resource {
    fn to_host(py: Python<'_>, _: &ValuePlan, value: V, _: &str) -> PyResult<Py<PyAny>> {
        value.into_python(py)
    }
}

/// Lock a resource's value for a synchronous call named `callable`, which
/// shares it with other readers. A free value is locked with the interpreter
/// lock held; only a busy one makes the call release that lock to wait.
///
/// # Errors
///
/// Returns `RuntimeError` when the value is busy inside a Python function
/// that Rust called, because the call that holds the value may be waiting
/// for that function.
pub fn read<'value, T: Send + Sync>(
    py: Python<'_>,
    shared: &'value RwLock<T>,
    callable: &'static str,
) -> PyResult<RwLockReadGuard<'value, T>> {
    match shared.try_read() {
        Ok(guard) => Ok(guard),
        Err(_) => wait(py, callable, || shared.blocking_read()),
    }
}

/// Lock a resource's value for a synchronous call named `callable`, which
/// holds it alone, as [`read`] does.
///
/// # Errors
///
/// Returns the errors of [`read`].
pub fn write<'value, T: Send + Sync>(
    py: Python<'_>,
    shared: &'value RwLock<T>,
    callable: &'static str,
) -> PyResult<RwLockWriteGuard<'value, T>> {
    match shared.try_write() {
        Ok(guard) => Ok(guard),
        Err(_) => wait(py, callable, || shared.blocking_write()),
    }
}

/// Wait for a busy resource's value through `lock`, as [`read`] says.
#[cold]
fn wait<G: Send>(
    py: Python<'_>,
    callable: &'static str,
    lock: impl FnOnce() -> G + Send,
) -> PyResult<G> {
    if runtime::inside_callback() {
        return Err(runtime_error(&ResourceError::Busy(callable)));
    }
    Ok(py.detach(|| executor::allow_blocking(lock)))
}

/// Reach the resource behind a Python argument through `reach`, such as
/// [`Resource::get`], for the call that `parameter` names, and note it in
/// `reached`.
///
/// # Errors
///
/// Returns `TypeError` that names the parameter when the argument is not an
/// object of the resource's class, and `RuntimeError` when the call reached
/// it already or `reach` fails, such as on a closed resource.
pub fn resource_from_host<T: HostResource, R>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
    reached: &mut ReachedResources,
    reach: impl FnOnce(&Resource<T>, &'static str) -> Result<R, ResourceError>,
) -> PyResult<R> {
    let callable = parameter.callable;
    T::with_resource(value, |resource| {
        reached.add(resource, callable)?;
        reach(resource, callable)
    })
    .ok_or_else(|| {
        let error = PyTypeError::new_err(format!("must be a {}", parameter::resource_name::<T>()));
        parameter.error(value.py(), error)
    })?
    .map_err(|error| runtime_error(&error))
}

/// Reach the resource behind a Python argument that may be `None`, as
/// [`resource_from_host`] does.
///
/// # Errors
///
/// Returns the errors of [`resource_from_host`].
pub fn optional_resource_from_host<T: HostResource, R>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
    reached: &mut ReachedResources,
    reach: impl FnOnce(&Resource<T>, &'static str) -> Result<R, ResourceError>,
) -> PyResult<Option<R>> {
    if value.is_none() {
        return Ok(None);
    }
    resource_from_host(value, parameter, reached, reach).map(Some)
}

/// The plans of one callback parameter's arguments and result, which the
/// macros keep in a static at each call site.
#[doc(hidden)]
#[derive(Debug)]
pub struct CallbackPlans {
    arguments: ValuePlan,
    result: ValuePlan,
}

impl CallbackPlans {
    /// Plans that are built at their first use.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            arguments: ValuePlan::new(),
            result: ValuePlan::new(),
        }
    }
}

impl Default for CallbackPlans {
    fn default() -> Self {
        Self::new()
    }
}

/// Keep a Python callable as a [`Callback`], by `plans`, which the call site
/// keeps.
///
/// The generated code passes the callable with the adapter of its result.
/// Each call makes its arguments as the extension makes a result, and reads
/// the callable's result as it reads an argument; a result that it cannot
/// read as it is, such as a dictionary for a model, crosses as the adapter's
/// dump, which Pydantic validates.
///
/// # Errors
///
/// Returns `TypeError` that names the parameter when the argument is not
/// callable.
pub fn callback_from_host<A, R>(
    value: &Bound<'_, PyAny>,
    plans: &'static CallbackPlans,
    parameter: &Parameter,
) -> PyResult<Callback<A, R>>
where
    A: Serialize + CallbackArguments + 'static,
    R: DeserializeOwned + ContractType + 'static,
{
    let py = value.py();
    let (function, adapter): (Bound<'_, PyAny>, Bound<'_, PyAny>) = value
        .extract()
        .map_err(|error| parameter.error(py, error))?;
    if !function.is_callable() {
        let error = PyTypeError::new_err("must be callable");
        return Err(parameter.error(py, error));
    }
    let (function, adapter) = (function.unbind(), adapter.unbind());
    Ok(Callback::new(move |arguments: A| {
        Python::attach(|py| call_python(py, &function, &adapter, plans, &arguments))
            .map_err(|error| CallbackError::new(error.to_string()))
    }))
}

/// Call a Python callable with the items of the tuple `arguments`, and read
/// its result, or else the dump of its result by `adapter`.
fn call_python<A, R>(
    py: Python<'_>,
    function: &Py<PyAny>,
    adapter: &Py<PyAny>,
    plans: &CallbackPlans,
    arguments: &A,
) -> PyResult<R>
where
    A: Serialize + CallbackArguments,
    R: DeserializeOwned + ContractType,
{
    let arguments = make(py, plans.arguments.of_arguments::<A>(), arguments)?;
    // A tuple of no items serializes as `None`.
    let arguments = if arguments.is_none() {
        PyTuple::empty(py)
    } else {
        arguments.cast_into::<PyTuple>().map_err(PyErr::from)?
    };
    let result = function.bind(py).call1(arguments)?;
    let read = match plans
        .result
        .of::<R>()
        .filter(|_| python_plan::classes(py).is_some())
    {
        Some(plan) => python_deserializer::from_python(&result, plan)
            .map_err(|error| error.0.unwrap_or_else(|| Revalidate::new_err(()))),
        None => read_plain(&result),
    };
    let decoded: R = match read {
        Ok(decoded) => decoded,
        Err(error) if !error.is_instance_of::<PyException>(py) => return Err(error),
        Err(_) => read_plain(
            &adapter
                .bind(py)
                .call_method1(intern!(py, "dump"), (result,))?,
        )?,
    };
    rules::checked(decoded).map_err(|violation| PyValueError::new_err(violation.to_string()))
}
