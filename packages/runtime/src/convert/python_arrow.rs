//! Python conversion of Arrow values through the Arrow PyCapsule interface.
//!
//! Python reads and writes Arrow memory through the interface's capsules, so
//! values cross without a copy in either direction. The capsules hold the
//! structs of the Arrow C Data Interface, which `arrow_ffi` imports and
//! exports.
//!
//! A returned [`RecordBatchStream`] crosses as a C stream that any thread may
//! pull, with or without the interpreter lock: each pull waits with the lock
//! released, so the stream can run Python code on other threads.

use std::ffi::CStr;
use std::mem;
use std::ptr::NonNull;
use std::sync::Mutex;

use arrow_array::ffi::{FFI_ArrowArray, FFI_ArrowSchema};
use arrow_array::ffi_stream::FFI_ArrowArrayStream;
use arrow_array::{ArrayRef, RecordBatch, RecordBatchReader};
use arrow_schema::{ArrowError, Field, SchemaRef};
use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyCapsule;

use super::arrow_ffi::{self, Export, ImportError};
use super::python::Parameter;
use crate::arrow::{self, ArrowValue, BlockingBatches, Parts, RecordBatchStream};
use crate::ir::ArrowKind;
use crate::runtime::executor::{self, Process};

/// Import an Arrow argument, without copying it, from any producer of the
/// Arrow PyCapsule interface, such as pyarrow, polars, or DuckDB.
///
/// # Errors
///
/// Returns `TypeError` when the object does not export the expected Arrow
/// value, and `ValueError` when its Arrow data is invalid; both name the
/// parameter.
pub fn arrow_from_host<T: ArrowValue>(
    value: &Bound<'_, PyAny>,
    parameter: &Parameter,
) -> PyResult<T> {
    import_value(value).map_err(|error| parameter.error(value.py(), error))
}

/// Import an Arrow value from a producer of the Arrow PyCapsule interface.
/// A stream's batches stay with the producer until Rust reads them.
fn import_value<T: ArrowValue>(value: &Bound<'_, PyAny>) -> PyResult<T> {
    let parts = match T::KIND {
        ArrowKind::Array => import_array(
            value,
            "an Arrow array, such as pyarrow.Array",
            arrow_ffi::import_array,
        )?,
        ArrowKind::RecordBatch => import_array(
            value,
            "an Arrow record batch, such as pyarrow.RecordBatch",
            arrow_ffi::import_batch,
        )?,
        ArrowKind::Table => import_stream(value, "an Arrow stream, such as pyarrow.Table")?
            .collected()
            .map_err(arrow_error)?,
        ArrowKind::Stream => {
            import_stream(value, "an Arrow stream, such as pyarrow.RecordBatchReader")?
        }
        ArrowKind::Schema => {
            let capsule = exported(
                value,
                "__arrow_c_schema__",
                "an Arrow schema, such as pyarrow.Schema",
            )?;
            let schema: NonNull<FFI_ArrowSchema> = pointer(&capsule, c"arrow_schema")?;
            // SAFETY: the capsule keeps a valid schema alive and releases
            // it when dropped; reading it does not take it.
            arrow_ffi::import_schema(unsafe { schema.as_ref() }).map_err(import_error)?
        }
    };
    T::from_parts(parts).map_err(arrow_error)
}

/// Export an Arrow value as an object that pyarrow imports, without
/// copying it, through the Arrow PyCapsule interface.
///
/// # Errors
///
/// Returns a Python exception when the export object cannot be created.
pub fn arrow_to_host<T: ArrowValue>(py: Python<'_>, value: T) -> PyResult<Py<PyAny>> {
    Ok(match arrow_ffi::export(value) {
        Export::Array { field, array } => Py::new(py, ArrowArray { field, array })?.into_any(),
        Export::Table { schema, batches } => {
            Py::new(py, ArrowTable { schema, batches })?.into_any()
        }
        Export::Stream(stream) => Py::new(py, ArrowReader::new(stream))?.into_any(),
        Export::Schema(schema) => Py::new(py, ArrowSchema { schema })?.into_any(),
    })
}

/// Report invalid Arrow data as a `ValueError`.
fn arrow_error(error: ArrowError) -> PyErr {
    PyValueError::new_err(error.to_string())
}

/// Report an Arrow value of the wrong kind as a `TypeError`, and a malformed
/// struct or invalid Arrow data as a `ValueError`.
fn import_error(error: ImportError) -> PyErr {
    match error {
        ImportError::WrongType(message) => PyTypeError::new_err(message),
        ImportError::Malformed(error) => PyValueError::new_err(error.to_string()),
        ImportError::Invalid(error) => arrow_error(error),
    }
}

/// Call an Arrow PyCapsule export method of `value`.
fn exported<'py>(
    value: &Bound<'py, PyAny>,
    method: &str,
    expected: &str,
) -> PyResult<Bound<'py, PyAny>> {
    if !value.hasattr(method)? {
        return Err(PyTypeError::new_err(format!(
            "must be {expected}: an object with `{method}`"
        )));
    }
    value.call_method0(method)
}

/// Return the pointer inside an Arrow capsule after checking its name.
fn pointer<T>(capsule: &Bound<'_, PyAny>, name: &CStr) -> PyResult<NonNull<T>> {
    Ok(capsule
        .cast::<PyCapsule>()?
        .pointer_checked(Some(name))?
        .cast())
}

/// Move the array out of an `__arrow_c_array__` export, and import it with
/// `import`, which reads the export's schema; the capsule keeps the schema.
fn import_array(
    value: &Bound<'_, PyAny>,
    expected: &str,
    import: unsafe fn(FFI_ArrowArray, &FFI_ArrowSchema) -> Result<Parts, ImportError>,
) -> PyResult<Parts> {
    let (schema, array): (Bound<'_, PyAny>, Bound<'_, PyAny>) =
        exported(value, "__arrow_c_array__", expected)?.extract()?;
    let schema: NonNull<FFI_ArrowSchema> = pointer(&schema, c"arrow_schema")?;
    let array: NonNull<FFI_ArrowArray> = pointer(&array, c"arrow_array")?;
    // SAFETY: the capsules hold a valid schema and array. The interface
    // lets the consumer move the array out, which marks the capsule's copy
    // released, while the capsule keeps and later releases the schema.
    let (schema, array) = unsafe { (schema.as_ref(), FFI_ArrowArray::from_raw(array.as_ptr())) };
    // SAFETY: the producer guarantees that the array matches its schema.
    unsafe { import(array, schema) }.map_err(import_error)
}

/// Move the stream out of an `__arrow_c_stream__` export, and read its
/// schema.
fn import_stream(value: &Bound<'_, PyAny>, expected: &str) -> PyResult<Parts> {
    let capsule = exported(value, "__arrow_c_stream__", expected)?;
    let stream: NonNull<FFI_ArrowArrayStream> = pointer(&capsule, c"arrow_array_stream")?;
    // SAFETY: the capsule holds a valid stream, and the interface lets the
    // consumer move it out, which marks the capsule's copy released.
    let stream = unsafe { FFI_ArrowArrayStream::from_raw(stream.as_ptr()) };
    arrow_ffi::import_stream(stream).map_err(import_error)
}

/// An array or record batch that Python imports through `__arrow_c_array__`.
#[pyclass(frozen, module = "bimetal")]
struct ArrowArray {
    field: Field,
    array: ArrayRef,
}

#[pymethods]
impl ArrowArray {
    /// Export the array. A producer may ignore the requested schema.
    #[pyo3(signature = (requested_schema = None))]
    fn __arrow_c_array__<'py>(
        &self,
        py: Python<'py>,
        requested_schema: Option<Bound<'py, PyAny>>,
    ) -> PyResult<(Bound<'py, PyCapsule>, Bound<'py, PyCapsule>)> {
        drop(requested_schema);
        let (schema, array) =
            arrow_ffi::export_array(&self.field, &self.array).map_err(arrow_error)?;
        Ok((
            PyCapsule::new_with_value(py, schema, c"arrow_schema")?,
            PyCapsule::new_with_value(py, array, c"arrow_array")?,
        ))
    }
}

/// A table's schema and batches, which Python imports through
/// `__arrow_c_stream__` as often as it asks.
#[pyclass(frozen, module = "bimetal")]
struct ArrowTable {
    schema: SchemaRef,
    batches: Vec<RecordBatch>,
}

#[pymethods]
impl ArrowTable {
    /// Export the batches. A producer may ignore the requested schema.
    #[pyo3(signature = (requested_schema = None))]
    fn __arrow_c_stream__<'py>(
        &self,
        py: Python<'py>,
        requested_schema: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyCapsule>> {
        drop(requested_schema);
        let stream = arrow_ffi::export_table(self.schema.clone(), self.batches.clone());
        PyCapsule::new_with_value(py, stream, c"arrow_array_stream")
    }
}

/// A stream, which Python imports once through `__arrow_c_stream__`.
#[pyclass(frozen, module = "bimetal")]
struct ArrowReader {
    /// The stream, until the first export takes it. Only code that holds
    /// the interpreter lock locks it, so a fork, which holds it too, never
    /// copies it held.
    stream: Mutex<Option<ExportedStream>>,
}

/// The reason to expect when locking an [`ArrowReader`]: only `Option::take`
/// runs under the lock, and it cannot panic.
const UNPOISONED: &str = "no code panics while holding an exported stream";

impl ArrowReader {
    /// Make the object that exports `stream`.
    fn new(stream: RecordBatchStream) -> Self {
        Self {
            stream: Mutex::new(Some(ExportedStream {
                schema: stream.schema().clone(),
                batches: Some(stream.into_iter()),
                process: Process::current(),
                end: None,
            })),
        }
    }
}

#[pymethods]
impl ArrowReader {
    /// Export the stream, once. A producer may ignore the requested schema.
    #[pyo3(signature = (requested_schema = None))]
    fn __arrow_c_stream__<'py>(
        &self,
        py: Python<'py>,
        requested_schema: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyCapsule>> {
        drop(requested_schema);
        let stream = self
            .stream
            .lock()
            .expect(UNPOISONED)
            .take()
            .ok_or_else(|| {
                PyRuntimeError::new_err(
                    "the stream was exported already, and can be read only once",
                )
            })?;
        let stream = FFI_ArrowArrayStream::new(Box::new(stream));
        PyCapsule::new_with_value(py, stream, c"arrow_array_stream")
    }
}

/// A stream that consumers pull through the C stream interface, from any
/// thread.
///
/// The source drops as soon as it ends, fails, or panics, and every later
/// pull ends or fails with the same message. In a forked child every pull
/// fails, and the source is never dropped: it may hold tasks of the parent's
/// runtime.
struct ExportedStream {
    schema: SchemaRef,
    /// The batches, until the source ends or the stream drops.
    batches: Option<BlockingBatches>,
    process: Process,
    end: Option<End>,
}

/// Why a stream yields no more batches.
enum End {
    /// Its source has no more.
    Finished,
    /// It failed with this message.
    Failed(String),
}

impl ExportedStream {
    /// Pull the next batch, with the interpreter lock released, and a panic
    /// caught: one that unwound into the C caller would abort the process.
    fn pull(&mut self) -> Result<Option<RecordBatch>, String> {
        let batches = self
            .batches
            .as_mut()
            .expect("an exported stream keeps its batches until it ends");
        match released(|| executor::catch(|| batches.next())) {
            Ok(batch) => batch.transpose().map_err(arrow::failure_message),
            Err(panic) => Err(format!("panic: {panic}")),
        }
    }

    /// Drop the source, unless this is a forked child, which leaves it alone.
    /// A drop may wait, and may panic, as a pull may.
    fn drop_source(&mut self) {
        let batches = self.batches.take();
        if self.process.is_current() {
            released(|| executor::catch(|| drop(batches)).ok());
        } else {
            mem::forget(batches);
        }
    }
}

impl Iterator for ExportedStream {
    type Item = Result<RecordBatch, ArrowError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.end.is_none() {
            self.end = Some(if self.process.is_current() {
                match self.pull() {
                    Ok(Some(batch)) => return Some(Ok(batch)),
                    Ok(None) => End::Finished,
                    // arrow-rs panics at a message that holds NUL.
                    Err(message) => End::Failed(message.replace('\0', "\u{fffd}")),
                }
            } else {
                End::Failed(
                    "the stream belongs to the process that this one forked from".to_owned(),
                )
            });
            self.drop_source();
        }
        match self.end.as_ref()? {
            End::Finished => None,
            End::Failed(message) => Some(Err(arrow::failure(message.clone()))),
        }
    }
}

impl RecordBatchReader for ExportedStream {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
}

impl Drop for ExportedStream {
    fn drop(&mut self) {
        self.drop_source();
    }
}

/// Run `pull` with the interpreter lock released, if this thread holds it,
/// as a consumer that Python code calls may.
fn released<T: Send>(pull: impl FnOnce() -> T + Send) -> T {
    let mut pull = Some(pull);
    if may_hold_lock() {
        // pyo3 refuses while the interpreter shuts down, and the pull then
        // keeps the lock.
        let detached = Python::try_attach(|py| py.detach(|| pull.take().map(|pull| pull())));
        if let Some(Some(output)) = detached {
            return output;
        }
    }
    pull.take()
        .expect("the pull runs here unless it ran detached")()
}

/// Return whether this thread may hold the interpreter lock. From Python
/// 3.12 the answer is exact, since the current thread state is this thread's.
/// Python 3.11 keeps only the holder's state, which another thread must not
/// touch, so there any thread with a state of its own may hold the lock.
fn may_hold_lock() -> bool {
    // SAFETY: Python sets its version before it loads any extension.
    if unsafe { pyo3::ffi::Py_Version } >= 0x030c_0000 {
        // SAFETY: from 3.12 this reads this thread's own state, which exists
        // only while the thread holds the lock, and returns NULL without it.
        !unsafe { pyo3::ffi::PyThreadState_GetDict() }.is_null()
    } else {
        // SAFETY: any thread may read its own state.
        !unsafe { pyo3::ffi::PyGILState_GetThisThreadState() }.is_null()
    }
}

/// A schema that Python imports through `__arrow_c_schema__`.
#[pyclass(frozen, module = "bimetal")]
struct ArrowSchema {
    schema: SchemaRef,
}

#[pymethods]
impl ArrowSchema {
    /// Export the schema.
    fn __arrow_c_schema__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyCapsule>> {
        let schema = arrow_ffi::export_schema(&self.schema).map_err(arrow_error)?;
        PyCapsule::new_with_value(py, schema, c"arrow_schema")
    }
}
