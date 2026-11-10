//! Shared application fixture for the integration test suites.

use std::collections::BTreeMap;
use std::future;
use std::path::Path;
use std::task::Poll;
use std::time::Duration;

use bimetal::Model;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Barrier;

mod inline {
    use bimetal::Model;
    use serde::{Deserialize, Serialize};

    /// A model declared in an inline Rust module.
    #[allow(
        dead_code,
        reason = "the contract suite inspects this declaration without constructing it"
    )]
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
    pub struct InlineValue {
        /// Payload retained under the declaration's original module.
        pub value: String,
    }
}

#[allow(
    unused_imports,
    reason = "the contract suite verifies that a re-export does not change the declaration path"
)]
pub use inline::InlineValue;

/// A stable identifier supplied by another system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(transparent)]
pub struct JobId(
    /// Identifier text as received from or sent to the host.
    pub String,
);

/// The supported ways to run a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// Favor low latency.
    Fast,
    /// Favor repeatable output.
    Deterministic,
}

/// A request that crosses both generated host boundaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct JobRequest {
    /// A user-visible job label.
    #[bimetal(min_length = 2, max_length = 64)]
    pub display_name: String,
    /// The number of values to process.
    #[bimetal(ge = 1, le = 100)]
    pub count: u32,
    /// Select the execution strategy.
    pub mode: RunMode,
    /// Disable side effects when true.
    #[serde(default)]
    pub dry_run: bool,
    /// Optional context supplied by the caller.
    pub context: Option<JobContext>,
}

/// The result returned for a valid job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct JobResult {
    /// Stable identifier allocated for the accepted job.
    pub id: JobId,
    /// Number of values accepted for processing.
    pub accepted_count: u32,
    /// Sum of the supplied numeric samples.
    pub sample_total: f64,
    /// Terminal state produced by execution.
    pub event: JobEvent,
}

/// Full-width unsigned values used to verify the Python boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct WideNumbers {
    /// Scalar unsigned value at the full Rust width.
    pub value: u64,
    /// Nested unsigned values at the full Rust width.
    pub values: Vec<u64>,
    /// Optional unsigned value at the full Rust width.
    pub optional: Option<u64>,
}

/// Optional context attached to a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase")]
pub struct JobContext {
    /// UTC creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Arbitrary JSON metadata supplied by the caller.
    pub metadata: Value,
    /// Deterministically ordered labels.
    pub labels: BTreeMap<String, String>,
    /// Inclusive integer range associated with the job.
    pub range: (i32, i32),
    /// Optional free-form note.
    pub note: Option<String>,
    /// Binary fingerprint serialized as an ordinary model field.
    pub fingerprint: Vec<u8>,
}

/// A state update emitted by a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum JobEvent {
    /// The job completed.
    Completed {
        /// Number of values accepted by the completed job.
        accepted_count: u32,
    },
    /// The job was rejected.
    Rejected {
        /// Human-readable rejection reason.
        reason: String,
    },
}

/// Where a job stands, in Serde's default enum representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum JobStep {
    /// The job waits for its turn.
    Queued,
    /// The job runs a request, boxed as the large data of a variant often is.
    Running(Box<JobRequest>),
    /// The job stopped after accepting some values.
    Stopped {
        /// Number of values accepted before the job stopped.
        #[bimetal(ge = 1)]
        accepted_count: u32,
    },
}

/// How long a job may run, the hours it runs between, and the day it starts,
/// in the forms Serde writes a `Duration`, an array, and a date.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
pub struct JobWindow {
    /// The longest the job may run.
    pub timeout: Duration,
    /// The first and the last hour it may run in.
    pub hours: [u8; 2],
    /// The day it starts.
    pub starts: NaiveDate,
}

/// Failures that callers can handle by stable code.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum JobError {
    /// A requested count fell outside the supported interval.
    #[error("count must be between 1 and 100")]
    InvalidCount,
    /// A counter constructor received a negative initial value.
    #[error("the initial counter value cannot be negative")]
    #[serde(rename = "negative_start")]
    NegativeStart,
}

/// Validate and execute one job.
#[bimetal::export]
pub fn execute_job(
    request: &JobRequest,
    #[bimetal(bytes)] payload: &[u8],
    #[bimetal(buffer)] samples: &[f64],
) -> Result<JobResult, JobError> {
    if !(1..=100).contains(&request.count) {
        return Err(JobError::InvalidCount);
    }
    let _payload_size = payload.len();
    Ok(JobResult {
        id: JobId(format!("job-{}", request.display_name)),
        accepted_count: request.count,
        sample_total: samples.iter().sum(),
        event: JobEvent::Completed {
            accepted_count: request.count,
        },
    })
}

/// Give a job request another display name, which the request's rules may
/// not allow.
#[bimetal::export]
pub fn rename_job(request: JobRequest, display_name: String) -> JobRequest {
    JobRequest {
        display_name,
        ..request
    }
}

/// Return a job step unchanged.
#[bimetal::export]
#[must_use]
pub fn echo_step(step: JobStep) -> JobStep {
    step
}

/// Return a job window unchanged.
#[bimetal::export]
#[must_use]
pub fn echo_window(window: JobWindow) -> JobWindow {
    window
}

/// Return the last component of a path, or `None` when it has none.
#[bimetal::export]
#[must_use]
pub fn file_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Reverse a byte sequence without changing its host boundary type.
#[bimetal::export]
#[bimetal(returns(bytes))]
pub fn reverse_bytes(#[bimetal(bytes)] input: &[u8]) -> Vec<u8> {
    input.iter().rev().copied().collect()
}

/// Scale a numeric buffer without turning it into a model.
#[bimetal::export]
#[bimetal(returns(buffer))]
pub fn scale_samples(#[bimetal(buffer)] samples: &[f64], factor: f64) -> Vec<f64> {
    samples.iter().map(|value| value * factor).collect()
}

/// Return full-width unsigned values unchanged.
#[bimetal::export]
#[must_use]
pub fn echo_u64(value: u64) -> u64 {
    value
}

/// Return a label unchanged, through a parameter named like the function.
#[bimetal::export]
#[must_use]
pub fn label(label: String) -> String {
    label
}

/// Report whether the Rust body runs while its thread holds the Python interpreter lock.
#[bimetal::export]
#[must_use]
pub fn holds_interpreter_lock() -> bool {
    #[cfg(all(feature = "python", not(target_arch = "wasm32")))]
    {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        use bimetal::__private::pyo3::Python;

        // Another thread can attach to the interpreter only while this one does not hold the lock.
        let (attached, receiver) = mpsc::channel();
        thread::spawn(move || {
            Python::attach(|_| attached.send(()).ok());
        });
        receiver.recv_timeout(Duration::from_secs(5)).is_err()
    }
    #[cfg(not(all(feature = "python", not(target_arch = "wasm32"))))]
    {
        false
    }
}

/// Return nested full-width unsigned values unchanged.
#[bimetal::export]
#[must_use]
pub fn echo_wide_numbers(value: WideNumbers) -> WideNumbers {
    value
}

/// A stateful counter exposed as one native resource.
pub struct Counter {
    value: i64,
}

#[bimetal::export]
impl Counter {
    /// Create a counter with an explicit value.
    #[bimetal(constructor)]
    pub fn new(initial_value: i64) -> Result<Self, JobError> {
        if initial_value < 0 {
            return Err(JobError::NegativeStart);
        }
        Ok(Self {
            value: initial_value,
        })
    }

    /// Create a counter at zero.
    #[bimetal(constructor)]
    pub fn from_zero() -> Self {
        Self { value: 0 }
    }

    /// Add a value and return the new total.
    pub fn add(&mut self, delta: i64) -> i64 {
        self.value += delta;
        self.value
    }

    /// Add six values without changing the counter.
    pub fn add_many(
        &self,
        first: i64,
        second: i64,
        third: i64,
        fourth: i64,
        fifth: i64,
        sixth: i64,
    ) -> i64 {
        self.value + first + second + third + fourth + fifth + sixth
    }

    /// Reset the counter for Rust-only callers.
    #[bimetal(skip)]
    #[allow(
        dead_code,
        reason = "the integration suite verifies that this method stays hidden"
    )]
    pub fn reset(&mut self) {
        self.value = 0;
    }
}

/// Records whether its construction ran while holding the Python interpreter lock.
pub struct LockProbe {
    held: bool,
}

#[bimetal::export]
impl LockProbe {
    /// Probe the interpreter lock during construction.
    #[bimetal(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            held: holds_interpreter_lock(),
        }
    }

    /// Whether construction held the interpreter lock.
    #[must_use]
    pub fn held(&self) -> bool {
        self.held
    }

    /// Whether this method holds the interpreter lock while it runs.
    #[must_use]
    pub fn holds_lock_now(&self) -> bool {
        holds_interpreter_lock()
    }
}

/// Yield to the executor a few times, so a call is still running when the
/// host acts next.
async fn yield_to_executor() {
    for _ in 0..3 {
        let mut yielded = false;
        future::poll_fn(|context| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                context.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }
}

/// Double a value after yielding to the executor.
#[bimetal::export]
pub async fn double_later(value: u32) -> u32 {
    yield_to_executor().await;
    value * 2
}

/// Accept a count inside the supported interval after yielding to the executor.
#[bimetal::export]
pub async fn check_count_later(count: u32) -> Result<u32, JobError> {
    yield_to_executor().await;
    if (1..=100).contains(&count) {
        Ok(count)
    } else {
        Err(JobError::InvalidCount)
    }
}

/// An append-only list that only async calls construct.
pub struct Ledger {
    entries: Vec<String>,
    meeting: Barrier,
}

#[bimetal::export]
impl Ledger {
    /// Open a ledger with its first entry.
    #[bimetal(constructor)]
    pub async fn open(first: String) -> Result<Self, JobError> {
        yield_to_executor().await;
        if first.is_empty() {
            return Err(JobError::InvalidCount);
        }
        Ok(Self {
            entries: vec![first],
            meeting: Barrier::new(2),
        })
    }

    /// Append an entry and return the new number of entries.
    pub async fn record(&mut self, entry: String) -> u64 {
        yield_to_executor().await;
        self.entries.push(entry);
        self.entries.len() as u64
    }

    /// Wait for a second call to meet this one, which needs both to share the
    /// value at once, and return the number of entries.
    pub async fn meet(&self) -> u64 {
        self.meeting.wait().await;
        self.entries.len() as u64
    }

    /// Hold the value alone until the host cancels the call.
    pub async fn hold(&mut self) {
        future::pending::<()>().await;
    }

    /// The number of entries.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.entries.len() as u64
    }
}

/// alloy's types in a model, which cross only with the `alloy` feature.
#[cfg(feature = "alloy")]
pub mod chain {
    use alloy_primitives::{Address, B256, I256, U64, U256};
    use bimetal::Model;
    use serde::{Deserialize, Serialize};

    /// A token transfer, as a chain records it.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
    pub struct Transfer {
        /// Who sent it.
        pub from: Address,
        /// How much, in the token's smallest units.
        pub amount: U256,
        /// The transaction it was in.
        pub transaction: B256,
        /// The sender's nonce.
        pub nonce: U64,
        /// The change in the sender's balance.
        pub change: I256,
    }

    /// Return a transfer unchanged.
    #[bimetal::export]
    #[must_use]
    pub fn echo_transfer(transfer: Transfer) -> Transfer {
        transfer
    }
}

/// Arrow round trips, which link only with the `arrow` feature.
#[cfg(feature = "arrow-api")]
pub mod columns {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use bimetal::Model;
    use bimetal::arrow::array::{Int64Array, StringArray};
    use bimetal::arrow::{ArrayRef, DataType, Field, RecordBatch, Schema, SchemaRef, Table};
    use serde::{Deserialize, Serialize};

    use super::JobError;

    /// The shape of a table.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Model)]
    pub struct TableSummary {
        /// Rows in every batch together.
        pub rows: u64,
        /// The number of batches.
        pub batches: u64,
        /// Column names in order.
        pub columns: Vec<String>,
    }

    /// A table of two batches with a number and a label column.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn sample_table() -> Table {
        let schema = Arc::new(Schema::new(vec![
            Field::new("n", DataType::Int64, false),
            Field::new("label", DataType::Utf8, true),
        ]));
        let batch = |numbers: Vec<i64>, labels: Vec<Option<&str>>| {
            RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int64Array::from(numbers)),
                    Arc::new(StringArray::from(labels)),
                ],
            )
            .expect("valid sample batch")
        };
        let batches = vec![
            batch(vec![1, 2], vec![Some("a"), None]),
            batch(vec![3], vec![Some("c")]),
        ];
        Table::try_new(schema.clone(), batches).expect("valid sample table")
    }

    /// Describe a table.
    #[bimetal::export]
    #[must_use]
    pub fn summarize_table(#[bimetal(arrow)] table: &Table) -> TableSummary {
        TableSummary {
            rows: table.num_rows() as u64,
            batches: table.batches().len() as u64,
            columns: table
                .schema()
                .fields()
                .iter()
                .map(|field| field.name().clone())
                .collect(),
        }
    }

    /// Return the first batch of a table.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    pub fn first_batch(#[bimetal(arrow)] table: Table) -> Result<RecordBatch, JobError> {
        table
            .into_batches()
            .into_iter()
            .next()
            .ok_or(JobError::InvalidCount)
    }

    /// Wrap one batch as a table.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn batch_as_table(#[bimetal(arrow)] batch: RecordBatch) -> Table {
        Table::from(batch)
    }

    /// Return a column of a batch.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn column_at(#[bimetal(arrow)] batch: &RecordBatch, index: u32) -> ArrayRef {
        batch.column(index as usize).clone()
    }

    /// Count the values of an array, including nulls.
    #[bimetal::export]
    #[must_use]
    pub fn array_length(#[bimetal(arrow)] values: ArrayRef) -> u64 {
        values.len() as u64
    }

    /// Return a table's schema.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn schema_of(#[bimetal(arrow)] table: &Table) -> SchemaRef {
        table.schema().clone()
    }

    /// Add one metadata entry to a schema.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn tag_schema(#[bimetal(arrow)] schema: Schema, key: String, value: String) -> Schema {
        let mut metadata = schema.metadata().clone();
        metadata.insert(key, value);
        schema.with_metadata(metadata)
    }

    /// Return a schema's metadata.
    #[bimetal::export]
    #[must_use]
    pub fn schema_metadata(#[bimetal(arrow)] schema: SchemaRef) -> BTreeMap<String, String> {
        schema
            .metadata()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// Streams of record batches, which only the Python suite reads: they need
/// the Tokio runtime that the feature links.
#[cfg(all(feature = "python", feature = "arrow-api"))]
pub mod streams {
    use std::future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::task::{Context, Poll};

    use bimetal::arrow::array::cast::AsArray;
    use bimetal::arrow::array::types::Int64Type;
    use bimetal::arrow::array::{Int64Array, RecordBatchIterator};
    use bimetal::arrow::{
        ArrowError, DataType, Field, RecordBatch, RecordBatchStream, Schema, SchemaRef,
    };
    use bimetal::{Callback, CallbackError};
    use futures_core::Stream;
    use tokio::task::JoinHandle;

    use super::JobError;

    /// How many streams of [`count`] have not dropped.
    static LIVE: AtomicU32 = AtomicU32::new(0);

    /// A failure to read a stream, with the stream's message.
    #[derive(Debug, thiserror::Error, bimetal::Error)]
    #[error("{message}")]
    pub struct ReadError {
        message: String,
    }

    impl From<ArrowError> for ReadError {
        fn from(error: ArrowError) -> Self {
            Self {
                message: error.to_string(),
            }
        }
    }

    /// The schema of every batch: one column of numbers.
    fn schema() -> SchemaRef {
        Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]))
    }

    /// A batch of `values`.
    fn batch(values: Vec<i64>) -> RecordBatch {
        RecordBatch::try_new(schema(), vec![Arc::new(Int64Array::from(values))])
            .expect("the column matches the schema")
    }

    /// Counts up from zero, and counts itself in [`LIVE`] until it drops.
    struct Counting {
        batches: u32,
        rows: u32,
        index: u32,
        fail_at: Option<u32>,
        panic_at: Option<u32>,
        /// Whether the stream yielded to the executor before its next batch.
        yielded: bool,
    }

    impl Stream for Counting {
        type Item = Result<RecordBatch, JobError>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Option<Self::Item>> {
            // Wait once before each batch, as a stream whose data arrives later does.
            if !self.yielded {
                self.yielded = true;
                context.waker().wake_by_ref();
                return Poll::Pending;
            }
            self.yielded = false;
            let index = self.index;
            if index == self.batches {
                return Poll::Ready(None);
            }
            self.index += 1;
            if self.panic_at == Some(index) {
                panic!("batch {index} panics");
            }
            if self.fail_at == Some(index) {
                return Poll::Ready(Some(Err(JobError::InvalidCount)));
            }
            let start = i64::from(index * self.rows);
            Poll::Ready(Some(Ok(batch(
                (start..start + i64::from(self.rows)).collect(),
            ))))
        }
    }

    impl Drop for Counting {
        fn drop(&mut self) {
            LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Count up from zero in `batches` batches of `rows` numbers. The stream
    /// fails at batch `fail_at`, and panics at batch `panic_at`.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn count(
        batches: u32,
        rows: u32,
        fail_at: Option<u32>,
        panic_at: Option<u32>,
    ) -> RecordBatchStream {
        LIVE.fetch_add(1, Ordering::SeqCst);
        let counting = Counting {
            batches,
            rows,
            index: 0,
            fail_at,
            panic_at,
            yielded: false,
        };
        RecordBatchStream::new(schema(), counting)
    }

    /// Count up from zero, as [`count`] does, from an async export.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    pub async fn count_later(batches: u32, rows: u32) -> RecordBatchStream {
        tokio::task::yield_now().await;
        count(batches, rows, None, None)
    }

    /// Return how many counting streams have not dropped.
    #[bimetal::export]
    #[must_use]
    pub fn live_streams() -> u32 {
        LIVE.load(Ordering::SeqCst)
    }

    /// Make one-row batches of the values that `next` returns for each index,
    /// on a blocking thread, until it returns `None`.
    struct Generated {
        next: Callback<(u32,), Option<i64>>,
        index: u32,
        call: Option<JoinHandle<Result<Option<i64>, CallbackError>>>,
    }

    impl Stream for Generated {
        type Item = Result<RecordBatch, ArrowError>;

        fn poll_next(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Option<Self::Item>> {
            let generated = &mut *self;
            let call = generated.call.get_or_insert_with(|| {
                let (next, index) = (generated.next.clone(), generated.index);
                tokio::task::spawn_blocking(move || next.call((index,)))
            });
            let Poll::Ready(value) = Pin::new(call).poll(context) else {
                return Poll::Pending;
            };
            generated.call = None;
            generated.index += 1;
            match value.expect("the call does not panic") {
                Ok(Some(value)) => Poll::Ready(Some(Ok(batch(vec![value])))),
                Ok(None) => Poll::Ready(None),
                Err(error) => Poll::Ready(Some(Err(ArrowError::ExternalError(Box::new(error))))),
            }
        }
    }

    /// Stream the values of a Python function, which a blocking thread calls
    /// for each index until it returns `None`.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn generate(next: Callback<(u32,), Option<i64>>) -> RecordBatchStream {
        let generated = Generated {
            next,
            index: 0,
            call: None,
        };
        RecordBatchStream::new(schema(), generated)
    }

    /// Stream the values of a Python function, as [`generate`] does, from a
    /// blocking reader that the puller's thread runs.
    #[bimetal::export]
    #[bimetal(returns(arrow))]
    #[must_use]
    pub fn generate_blocking(next: Callback<(u32,), Option<i64>>) -> RecordBatchStream {
        let values = (0..).map_while(move |index| match next.call((index,)) {
            Ok(Some(value)) => Some(Ok(batch(vec![value]))),
            Ok(None) => None,
            Err(error) => Some(Err(ArrowError::ExternalError(Box::new(error)))),
        });
        RecordBatchStream::from_reader(RecordBatchIterator::new(values, schema()))
    }

    /// Add up the numbers of one batch.
    fn total(batch: &RecordBatch) -> i64 {
        batch
            .column(0)
            .as_primitive::<Int64Type>()
            .values()
            .iter()
            .sum()
    }

    /// Add up every number of a stream, on the calling thread.
    #[bimetal::export]
    pub fn sum(#[bimetal(arrow)] rows: RecordBatchStream) -> Result<i64, ReadError> {
        let mut sum = 0;
        for batch in rows {
            sum += total(&batch?);
        }
        Ok(sum)
    }

    /// Add up every number of a stream, as [`sum`] does, in an async export.
    #[bimetal::export]
    pub async fn sum_later(
        #[bimetal(arrow)] mut rows: RecordBatchStream,
    ) -> Result<i64, ReadError> {
        let mut sum = 0;
        while let Some(batch) =
            future::poll_fn(|context| Pin::new(&mut rows).poll_next(context)).await
        {
            sum += total(&batch?);
        }
        Ok(sum)
    }

    /// Add up every number of a stream, iterating it on a runtime worker, as
    /// async code that calls synchronous code may.
    #[bimetal::export]
    pub async fn sum_on_worker(
        #[bimetal(arrow)] rows: RecordBatchStream,
    ) -> Result<i64, ReadError> {
        tokio::task::yield_now().await;
        sum(rows)
    }

    /// Read the first batch of a stream, and drop the rest.
    #[bimetal::export]
    pub fn first(#[bimetal(arrow)] rows: RecordBatchStream) -> Result<i64, ReadError> {
        let mut batches = rows.into_iter();
        let batch = batches.next().ok_or_else(|| ReadError {
            message: "the stream is empty".to_owned(),
        })??;
        Ok(total(&batch))
    }

    /// Read a stream `reads` times, even past its end or an error, and
    /// describe each read: `rows:<count>`, `error:<message>`, or `end`.
    #[bimetal::export]
    #[must_use]
    pub fn outcomes(#[bimetal(arrow)] rows: RecordBatchStream, reads: u32) -> Vec<String> {
        let mut batches = rows.into_iter();
        (0..reads)
            .map(|_| match batches.next() {
                Some(Ok(batch)) => format!("rows:{}", batch.num_rows()),
                Some(Err(error)) => format!("error:{error}"),
                None => "end".to_owned(),
            })
            .collect()
    }
}

/// Exports that only the Python suite calls, which the contract suite leaves
/// out of its exact lists. They need the Tokio runtime that the feature links.
#[cfg(feature = "python")]
pub mod extra {
    use bimetal::Model;
    use serde::{Deserialize, Serialize};
    use tokio::runtime::Handle;

    /// A route, whose name is also `std::path::Path`'s.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Model)]
    pub struct Path {
        /// The stops along it.
        pub stops: Vec<u32>,
    }

    /// Count a route's stops, through a reference to a type of the
    /// application's named `Path`.
    #[bimetal::export]
    #[must_use]
    pub fn count_stops(path: &Path) -> usize {
        path.stops.len()
    }

    /// Panic with a message.
    #[bimetal::export]
    #[must_use]
    pub fn panic_now(message: String) -> u32 {
        panic!("{message}");
    }

    /// Panic with a message after yielding to the executor.
    #[bimetal::export]
    pub async fn panic_later(message: String) -> u32 {
        tokio::task::yield_now().await;
        panic!("{message}");
    }

    /// Return whether the call runs inside a Tokio runtime.
    #[bimetal::export]
    #[must_use]
    pub fn in_runtime() -> bool {
        Handle::try_current().is_ok()
    }
}

/// The contract revision used by this integration fixture.
#[bimetal::export]
pub const CONTRACT_REVISION: u32 = 7;

/// The default label used by this integration fixture.
#[bimetal::export]
pub static DEFAULT_LABEL: &str = "integration";

bimetal::application!();

/// Find the fixture's declarations in its manifest.
#[allow(
    dead_code,
    reason = "each suite looks up only some kinds of declaration"
)]
pub mod lookup {
    use bimetal::ir::{FunctionDef, Manifest, ResourceDef, TypeDef};

    /// Return the function that hosts call `host_name`.
    pub fn function<'a>(manifest: &'a Manifest, host_name: &str) -> &'a FunctionDef {
        manifest
            .functions
            .iter()
            .find(|function| function.callable.host_name == host_name)
            .unwrap_or_else(|| panic!("missing function {host_name}"))
    }

    /// Return the resource named `name`.
    pub fn resource<'a>(manifest: &'a Manifest, name: &str) -> &'a ResourceDef {
        manifest
            .resources
            .iter()
            .find(|resource| resource.name == name)
            .unwrap_or_else(|| panic!("missing resource {name}"))
    }

    /// Return the model named `name`.
    pub fn model<'a>(manifest: &'a Manifest, name: &str) -> &'a TypeDef {
        manifest
            .types
            .iter()
            .find(|model| model.name == name)
            .unwrap_or_else(|| panic!("missing type {name}"))
    }
}

/// The fixture's native Python module, for the suites that call it from Python.
#[cfg(all(feature = "python", not(target_arch = "wasm32")))]
pub mod python {
    use std::ffi::CStr;
    use std::sync::Once;

    use bimetal::__private::pyo3::types::{PyAnyMethods, PyDict, PyDictMethods, PyModule};
    use bimetal::__private::pyo3::{Bound, PyResult, Python};
    use bimetal::ir::Manifest;

    use super::lookup;

    /// Register the fixture's native module and pass it, with the contract,
    /// to `run`.
    pub fn with_registered_module<T>(
        run: impl for<'py> FnOnce(Python<'py>, &Bound<'py, PyModule>, &Manifest) -> PyResult<T>,
    ) -> PyResult<T> {
        static INITIALIZE: Once = Once::new();
        INITIALIZE.call_once(|| {
            Python::initialize();
            // `threading` calls its first importer the main thread, where
            // Windows' asyncio loop sets a signal wakeup that fails elsewhere.
            Python::attach(|py| py.import("threading").map(drop))
                .expect("the interpreter must import threading");
        });
        Python::attach(|py| {
            let module = PyModule::new(py, "native")?;
            bimetal::runtime::python::register(
                &module,
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            )?;
            let manifest =
                bimetal::registry::manifest(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                    .expect("the test registry must be valid");
            run(py, &module, &manifest)
        })
    }

    /// Run the Python code `scenario` with these globals: the `functions`,
    /// given by their host names, under their Rust names; the `resources`;
    /// and `PanicException`.
    pub fn run_scenario(scenario: &CStr, functions: &[&str], resources: &[&str]) -> PyResult<()> {
        with_registered_module(|py, module, manifest| {
            let globals = PyDict::new(py);
            for name in functions {
                let function = lookup::function(manifest, name);
                globals.set_item(
                    &function.callable.rust_name,
                    module.getattr(&function.native_name)?,
                )?;
            }
            for &name in resources {
                let resource = lookup::resource(manifest, name);
                globals.set_item(name, module.getattr(&resource.native_name)?)?;
            }
            globals.set_item("PanicException", module.getattr("PanicException")?)?;
            py.run(scenario, Some(&globals), None)
        })
    }
}
