//! Python conversion primitives used by generated native wrappers.

use std::ffi::CStr;
use std::fmt::Display;
use std::{mem, ptr, slice};

use bimetal_ir::BufferElement;
use pyo3::buffer::{ElementType, PyUntypedBuffer};
use pyo3::exceptions::{PyBufferError, PyException, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyList, PyString, PyTuple};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use super::parameter::Parameter;
#[cfg(feature = "arrow-api")]
pub use super::python_arrow::{arrow_from_host, arrow_to_host};
use crate::ContractType;
use crate::convert::parameter;
use crate::runtime::{Callback, CallbackError, ContractError, Resource, rules};

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

/// Deserialize a Python argument through Serde, and check its field rules.
///
/// # Errors
///
/// Returns `TypeError` when the value has the wrong type for `T`, and
/// `ValueError` when it has the right type but an invalid value, such as one
/// that breaks a field rule; both name the parameter.
pub fn from_host<T: DeserializeOwned + ContractType>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
) -> PyResult<T> {
    let py = value.py();
    // pythonize would read a `str` as the list of its characters.
    if let Some(sequence) = T::SEQUENCE
        && value.is_instance_of::<PyString>()
    {
        let error = PyTypeError::new_err(format!("must be {sequence}, not str"));
        return Err(parameter.error(py, error));
    }
    let decoded: T =
        pythonize::depythonize(value).map_err(|error| parameter.error(py, error.into()))?;
    rules::checked(decoded)
        .map_err(|violation| parameter.error(py, PyValueError::new_err(violation.to_string())))
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
/// Python through Serde.
///
/// # Errors
///
/// Returns `RuntimeError` when the value breaks a field rule, which is a bug
/// of the Rust code, and `ValueError` when Serde cannot represent the value
/// in Python.
pub fn to_host<T: Serialize + ContractType>(
    py: Python<'_>,
    value: &T,
    callable: &str,
) -> PyResult<Py<PyAny>> {
    rules::checked_return(value, callable).map_err(PyRuntimeError::new_err)?;
    pythonize::pythonize(py, value)
        .map(Bound::unbind)
        .map_err(|error| PyValueError::new_err(error.to_string()))
}

/// Convert a typed contract error into the native Python exception payload.
#[must_use]
pub fn contract_error(error: &impl ContractError) -> PyErr {
    PyRuntimeError::new_err((error.code(), error.to_string(), error.details()))
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

/// Make the Python value of a call's resources.
///
/// # Errors
///
/// Returns the error of Python's allocation.
pub fn resource_to_host<T: ResourceOutput>(py: Python<'_>, value: T) -> PyResult<Py<PyAny>> {
    value.into_python(py)
}

/// Run `reach` on the resource behind a Python argument.
///
/// # Errors
///
/// Returns `TypeError` that names the parameter when the argument is not an
/// object of the resource's class.
pub fn resource_from_host<T: HostResource, R>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
    reach: impl FnOnce(&Resource<T>) -> R,
) -> PyResult<R> {
    T::with_resource(value, reach).ok_or_else(|| {
        let error = PyTypeError::new_err(format!("must be a {}", parameter::resource_name::<T>()));
        parameter.error(value.py(), error)
    })
}

/// Run `reach` on the resource behind a Python argument that may be `None`.
///
/// # Errors
///
/// Returns `TypeError` that names the parameter when the argument is another
/// kind of object.
pub fn optional_resource_from_host<T: HostResource, R>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
    reach: impl FnOnce(&Resource<T>) -> R,
) -> PyResult<Option<R>> {
    if value.is_none() {
        return Ok(None);
    }
    resource_from_host(value, parameter, reach).map(Some)
}

/// Keep a Python callable as a [`Callback`].
///
/// # Errors
///
/// Returns `TypeError` that names the parameter when the argument is not
/// callable.
pub fn callback_from_host<A, R>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
) -> PyResult<Callback<A, R>>
where
    A: Serialize + 'static,
    R: DeserializeOwned + ContractType + 'static,
{
    if !value.is_callable() {
        let error = PyTypeError::new_err("must be callable");
        return Err(parameter.error(value.py(), error));
    }
    let function = value.clone().unbind();
    Ok(Callback::new(move |arguments: A| {
        Python::attach(|py| call_python(py, &function, &arguments))
            .map_err(|error| CallbackError::new(error.to_string()))
    }))
}

/// Call a Python callable with the items of the tuple `arguments`.
fn call_python<A: Serialize, R: DeserializeOwned + ContractType>(
    py: Python<'_>,
    function: &Py<PyAny>,
    arguments: &A,
) -> PyResult<R> {
    let arguments = pythonize::pythonize(py, arguments)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    // A tuple of no items serializes as `None`.
    let arguments = if arguments.is_none() {
        PyTuple::empty(py)
    } else {
        arguments.cast_into::<PyTuple>().map_err(PyErr::from)?
    };
    let result = function.bind(py).call1(arguments)?;
    let decoded: R = pythonize::depythonize(&result).map_err(PyErr::from)?;
    rules::checked(decoded).map_err(|violation| PyValueError::new_err(violation.to_string()))
}
