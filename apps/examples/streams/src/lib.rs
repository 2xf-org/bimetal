//! Streams of Arrow record batches. `count` returns a stream that Python
//! reads one batch at a time, `scale` takes one and returns another, and
//! `total` and `total_later` read what Python passes as they need it.
//! TypeScript cannot pass a stream to a synchronous call such as `total`, so
//! this example has only a Python package; the showcase streams to both.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bimetal::arrow::array::Int64Array;
use bimetal::arrow::array::cast::AsArray;
use bimetal::arrow::array::types::Int64Type;
use bimetal::arrow::{ArrowError, DataType, Field, RecordBatch, RecordBatchStream, Schema};
use futures::StreamExt;
use futures::stream;

/// Failures of the streams and of their readers.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum StreamError {
    /// The count stopped where `fail_at` asked it to.
    #[error("the count stopped at batch {batch}")]
    Stopped {
        /// The batch that failed.
        batch: u32,
    },
    /// A batch lacks an `n` column of 64-bit integers.
    #[error("each batch needs an `n` column of 64-bit integers")]
    NoNumbers,
    /// Reading a stream failed.
    #[error("{message}")]
    Read {
        /// The stream's error.
        message: String,
    },
}

impl From<ArrowError> for StreamError {
    fn from(error: ArrowError) -> Self {
        Self::Read {
            message: error.to_string(),
        }
    }
}

/// How many streams of `count` have not dropped.
static OPEN: AtomicU32 = AtomicU32::new(0);

/// Counts one stream of `count` in [`OPEN`] until the stream drops.
struct Open;

impl Open {
    fn new() -> Self {
        OPEN.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        OPEN.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Count up from zero in `batches` batches of `size` numbers, as a column
/// `n`. Each batch is made when the reader asks for it. The stream fails at
/// batch `fail_at`, if one is given.
#[bimetal::export]
#[bimetal(returns(arrow))]
#[must_use]
pub fn count(batches: u32, size: u32, fail_at: Option<u32>) -> RecordBatchStream {
    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
    let open = Open::new();
    let batch_schema = schema.clone();
    let numbers = stream::iter(0..batches).map(move |batch| {
        // The closure owns `open`, which counts the stream until it drops.
        let _open = &open;
        if fail_at == Some(batch) {
            return Err(StreamError::Stopped { batch });
        }
        let start = i64::from(batch) * i64::from(size);
        let column = Int64Array::from_iter_values(start..start + i64::from(size));
        let numbers = RecordBatch::try_new(batch_schema.clone(), vec![Arc::new(column)])
            .expect("the column matches the schema");
        Ok(numbers)
    });
    RecordBatchStream::new(schema, numbers)
}

/// Return how many streams of `count` have not dropped. A stream drops when
/// it ends or fails, or when Python closes or deletes its reader.
#[bimetal::export]
#[must_use]
pub fn open_streams() -> u32 {
    OPEN.load(Ordering::SeqCst)
}

/// Multiply the `n` column of every batch by `factor`, one batch at a time
/// as the reader asks.
#[bimetal::export]
#[bimetal(returns(arrow))]
pub async fn scale(#[bimetal(arrow)] rows: RecordBatchStream, factor: i64) -> RecordBatchStream {
    let schema = rows.schema().clone();
    let batch_schema = schema.clone();
    let scaled = rows.map(move |batch| {
        let batch = batch?;
        let index = numbers(&batch)?;
        let numbers = batch.column(index).as_primitive::<Int64Type>();
        let column: Int64Array = numbers.unary(|number| number * factor);
        let mut columns = batch.columns().to_vec();
        columns[index] = Arc::new(column);
        Ok::<_, StreamError>(RecordBatch::try_new(batch_schema.clone(), columns)?)
    });
    RecordBatchStream::new(schema, scaled)
}

/// Add up the `n` column, reading one batch at a time: every batch, or the
/// first `limit`. Stopping early drops the rest of the stream.
#[bimetal::export]
pub fn total(
    #[bimetal(arrow)] rows: RecordBatchStream,
    limit: Option<u32>,
) -> Result<i64, StreamError> {
    let limit = limit.map_or(usize::MAX, |limit| limit as usize);
    let mut total = 0;
    for batch in rows.into_iter().take(limit) {
        total += sum(&batch?)?;
    }
    Ok(total)
}

/// Add up the `n` column of every batch, as `total` does, in async code. Each
/// batch is read on a blocking thread, so a Python source runs while the
/// event loop waits.
#[bimetal::export]
pub async fn total_later(
    #[bimetal(arrow)] mut rows: RecordBatchStream,
) -> Result<i64, StreamError> {
    let mut total = 0;
    while let Some(batch) = rows.next().await {
        total += sum(&batch?)?;
    }
    Ok(total)
}

/// Return the index of a batch's `n` column, after checking its type.
fn numbers(batch: &RecordBatch) -> Result<usize, StreamError> {
    let index = batch
        .schema_ref()
        .index_of("n")
        .map_err(|_| StreamError::NoNumbers)?;
    match batch.column(index).data_type() {
        DataType::Int64 => Ok(index),
        _ => Err(StreamError::NoNumbers),
    }
}

/// Add up the `n` column of one batch. Nulls count as nothing.
fn sum(batch: &RecordBatch) -> Result<i64, StreamError> {
    let column = batch.column(numbers(batch)?).as_primitive::<Int64Type>();
    Ok(column.iter().flatten().sum())
}
