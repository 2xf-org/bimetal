//! Apache Arrow values that cross the host boundaries without Serde.
//!
//! Annotate a parameter with `#[bimetal(arrow)]`, or an export with
//! `#[bimetal(returns(arrow))]`, to pass an [`ArrayRef`], a [`RecordBatch`], a
//! [`Table`], a [`Schema`], or a [`RecordBatchStream`]. Python receives them
//! through the Arrow PyCapsule interface without copying; Wasm receives an
//! Arrow IPC stream. A [`RecordBatchStream`] crosses only to Python, one batch
//! at a time.
//!
//! The arrow-rs types are re-exported here, so an application's Arrow values
//! are always the runtime's own. The `arrow` feature uses arrow-rs 60 and
//! `arrow-59` uses 59, whose types are those of a library that uses that version.

mod arrow_value;
mod record_batch_stream;
mod table;

#[cfg(not(feature = "arrow-59"))]
pub use arrow_array as array;
pub use arrow_array::{Array, ArrayRef, RecordBatch};
#[cfg(feature = "arrow-59")]
pub use arrow_array_59 as array;
#[cfg(not(feature = "arrow-59"))]
pub use arrow_schema as schema;
pub use arrow_schema::{ArrowError, DataType, Field, Schema, SchemaRef};
#[cfg(feature = "arrow-59")]
pub use arrow_schema_59 as schema;

pub use arrow_value::ArrowValue;
#[doc(hidden)]
pub use arrow_value::Parts;
#[cfg(all(feature = "node", not(target_arch = "wasm32")))]
pub(crate) use record_batch_stream::READ_WITH_AWAIT;
pub use record_batch_stream::{BatchError, BlockingBatches, RecordBatchStream};
#[cfg(any(target_arch = "wasm32", feature = "python", feature = "node"))]
pub(crate) use record_batch_stream::{failure, failure_message};
pub use table::Table;
