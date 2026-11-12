//! A schema and record batches that a reader pulls one at a time, from an
//! async stream or from a blocking reader.

use std::future::{self, Future};
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker, ready};
use std::{error, fmt, mem, panic};

use arrow_array::{RecordBatch, RecordBatchReader};
use arrow_schema::{ArrowError, SchemaRef};
use futures_core::Stream;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::runtime::ContractError;

/// One batch, or the error that the stream failed with.
type Batch = Result<RecordBatch, ArrowError>;

/// A blocking source of batches.
type Blocking = Box<dyn Iterator<Item = Batch> + Send>;

/// A schema and the record batches that follow it, which the reader pulls
/// one at a time while Rust makes them.
///
/// Return one from an export, and Python receives a
/// `pyarrow.RecordBatchReader`, which pyarrow, DuckDB, and Polars read from
/// any thread. Take one as a parameter, and it reads any Arrow stream that
/// Python passes, one batch at a time. Async code reads it as a [`Stream`],
/// and synchronous code iterates it.
///
/// Every batch must have the stream's schema; one that does not reads as a
/// [`ArrowError::SchemaError`]. Dropping the stream drops its source, which
/// cancels the source's work.
pub struct RecordBatchStream {
    schema: SchemaRef,
    source: Source,
}

/// Where a stream's batches come from.
enum Source {
    /// An async stream, which the reader polls.
    Async(Pin<Box<dyn Stream<Item = Batch> + Send>>),
    /// A blocking reader, such as a Python object's Arrow stream.
    Blocking(Pull),
}

/// A blocking reader that an async reader pulls on a blocking thread of the
/// runtime that polls it.
enum Pull {
    /// The reader, ready for the next pull.
    Ready(Blocking),
    /// A pull on a blocking thread, which hands the reader back with the
    /// batch.
    Pulling(JoinHandle<(Blocking, Option<Batch>)>),
    /// The reader panicked on a blocking thread, which dropped it.
    Lost,
}

impl RecordBatchStream {
    /// Create a stream of the batches that `batches` makes, which must all
    /// have `schema`. An error that derives `bimetal::Error` keeps its code: see
    /// [`BatchError`].
    #[must_use]
    pub fn new<S, E>(schema: SchemaRef, batches: S) -> Self
    where
        S: Stream<Item = Result<RecordBatch, E>> + Send + 'static,
        E: BatchError,
    {
        Self {
            schema,
            source: Source::Async(Box::pin(IntoArrowErrors(Box::pin(batches)))),
        }
    }

    /// Create a stream of a blocking reader's batches, such as those of
    /// arrow-rs's `RecordBatchIterator`. A reader in async code pulls them
    /// on a blocking thread.
    #[must_use]
    pub fn from_reader<R: RecordBatchReader + Send + 'static>(reader: R) -> Self {
        Self {
            schema: reader.schema(),
            source: Source::Blocking(Pull::Ready(Box::new(reader))),
        }
    }

    /// The schema of every batch.
    #[must_use]
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// Wait on this thread for the next batch. A blocking reader runs here,
    /// and an async stream runs here inside Bimetal's runtime on a native host.
    /// In Wasm, which has one thread, an async stream that waits fails
    /// instead, since what it waits for could never run.
    fn blocking_next(&mut self) -> Option<Batch> {
        let batch = if let Source::Blocking(Pull::Ready(batches)) = &mut self.source {
            allow_blocking(|| batches.next())
        } else {
            allow_blocking(|| {
                in_runtime(|| block_on(future::poll_fn(|context| self.poll_source(context))))
            })
            .unwrap_or_else(|| Some(Err(failure(READ_WITH_AWAIT.to_owned()))))
        };
        batch.map(|batch| self.checked(batch))
    }

    /// Poll the source for its next batch, unchecked.
    fn poll_source(&mut self, context: &mut Context<'_>) -> Poll<Option<Batch>> {
        match &mut self.source {
            Source::Async(batches) => batches.as_mut().poll_next(context),
            Source::Blocking(pull) => pull.poll_next(context),
        }
    }

    /// Check that `batch` has the stream's schema, which every reader of the
    /// stream assumes.
    fn checked(&self, batch: Batch) -> Batch {
        let batch = batch?;
        if *batch.schema_ref() != self.schema {
            return Err(ArrowError::SchemaError(format!(
                "a stream's batch has the schema {} instead of {}",
                batch.schema_ref(),
                self.schema
            )));
        }
        Ok(batch)
    }
}

impl Stream for RecordBatchStream {
    type Item = Batch;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Batch>> {
        let stream = self.get_mut();
        let batch = ready!(stream.poll_source(context));
        Poll::Ready(batch.map(|batch| stream.checked(batch)))
    }
}

impl fmt::Debug for RecordBatchStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordBatchStream")
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}

impl IntoIterator for RecordBatchStream {
    type Item = Batch;
    type IntoIter = BlockingBatches;

    fn into_iter(self) -> BlockingBatches {
        BlockingBatches(self)
    }
}

/// The batches of a [`RecordBatchStream`] for synchronous code: each call to
/// `next` waits for one on the calling thread. It is an arrow-rs
/// [`RecordBatchReader`] too.
#[derive(Debug)]
pub struct BlockingBatches(RecordBatchStream);

impl Iterator for BlockingBatches {
    type Item = Batch;

    fn next(&mut self) -> Option<Batch> {
        self.0.blocking_next()
    }
}

impl RecordBatchReader for BlockingBatches {
    fn schema(&self) -> SchemaRef {
        self.0.schema.clone()
    }
}

impl Pull {
    /// Pull the reader's next batch on a blocking thread of the current
    /// runtime, or on this thread when there is none.
    fn poll_next(&mut self, context: &mut Context<'_>) -> Poll<Option<Batch>> {
        if let Self::Ready(batches) = self {
            let Ok(runtime) = Handle::try_current() else {
                return Poll::Ready(batches.next());
            };
            let Self::Ready(mut batches) = mem::replace(self, Self::Lost) else {
                unreachable!("the reader is ready");
            };
            *self = Self::Pulling(runtime.spawn_blocking(move || {
                let batch = batches.next();
                (batches, batch)
            }));
        }
        let Self::Pulling(pull) = self else {
            return Poll::Ready(None);
        };
        match ready!(Pin::new(pull).poll(context)) {
            Ok((batches, batch)) => {
                *self = Self::Ready(batches);
                Poll::Ready(batch)
            }
            Err(error) => {
                *self = Self::Lost;
                if error.is_panic() {
                    panic::resume_unwind(error.into_panic());
                }
                Poll::Ready(Some(Err(ArrowError::ExternalError(Box::new(error)))))
            }
        }
    }
}

/// An async stream whose errors become [`ArrowError`]s.
struct IntoArrowErrors<S>(Pin<Box<S>>);

impl<S, E> Stream for IntoArrowErrors<S>
where
    S: Stream<Item = Result<RecordBatch, E>>,
    E: BatchError,
{
    type Item = Batch;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Batch>> {
        self.0
            .as_mut()
            .poll_next(context)
            .map(|batch| batch.map(|batch| batch.map_err(BatchError::into_arrow_error)))
    }
}

/// An error that the stream of [`RecordBatchStream::new`] can fail with: an
/// [`ArrowError`], or an error that derives `bimetal::Error`. A typed error
/// becomes an [`ArrowError::ExternalError`] whose message starts with its
/// code, as in `not_found: no table named t`; its details stay behind.
pub trait BatchError: Send + 'static {
    /// Convert the error into the [`ArrowError`] that the stream yields.
    fn into_arrow_error(self) -> ArrowError;
}

impl BatchError for ArrowError {
    fn into_arrow_error(self) -> ArrowError {
        self
    }
}

impl<E: ContractError + Send + 'static> BatchError for E {
    fn into_arrow_error(self) -> ArrowError {
        ArrowError::ExternalError(Box::new(CodedError {
            code: self.code(),
            message: self.to_string(),
        }))
    }
}

/// A typed error's code and message.
#[derive(Debug)]
struct CodedError {
    code: &'static str,
    message: String,
}

impl fmt::Display for CodedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl error::Error for CodedError {}

/// What a blocking read of a stream that JavaScript writes fails with, where
/// it would wait for ever.
pub(crate) const READ_WITH_AWAIT: &str =
    "a stream that JavaScript writes must be read with `.await`";

/// The message that a host shows for a stream's error. A typed error's
/// `code: message` stays without the prefix of an external error's display.
#[cfg(any(target_arch = "wasm32", feature = "python", feature = "node"))]
pub(crate) fn failure_message(error: ArrowError) -> String {
    match error {
        ArrowError::ExternalError(source) => source.to_string(),
        error => error.to_string(),
    }
}

/// An error whose message is `message`, as the host reported it.
pub(crate) fn failure(message: String) -> ArrowError {
    ArrowError::ExternalError(Box::new(Failure(message)))
}

/// A failure that a host reported, by its message.
#[derive(Debug)]
struct Failure(String);

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl error::Error for Failure {}

/// Run `future` to its end on this thread, which sleeps while it waits.
#[cfg(not(target_arch = "wasm32"))]
fn block_on<F: Future>(future: F) -> Option<F::Output> {
    use std::thread;

    /// Wakes the thread that waits for the future.
    struct Unpark(thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    // This read blocks even inside an async call's first poll, so a stream
    // that JavaScript writes must fail it there as anywhere else.
    #[cfg(feature = "node")]
    let _mark = crate::runtime::executor::leave_first_poll();
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return Some(output);
        }
        thread::park();
    }
}

/// Run `future` to its end, or return `None` when it waits for anything
/// but itself: Wasm's one thread cannot run what it waits for.
#[cfg(target_arch = "wasm32")]
fn block_on<F: Future>(future: F) -> Option<F::Output> {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Records that the future woke itself.
    struct Woken(AtomicBool);

    impl Wake for Woken {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    let woken = Arc::new(Woken(AtomicBool::new(false)));
    let waker = Waker::from(Arc::clone(&woken));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return Some(output);
        }
        if !woken.0.swap(false, Ordering::Relaxed) {
            return None;
        }
    }
}

/// Run `pull`, which may block its thread, after a Tokio worker hands its
/// other work away.
#[cfg(all(any(feature = "python", feature = "node"), not(target_arch = "wasm32")))]
fn allow_blocking<T>(pull: impl FnOnce() -> T) -> T {
    crate::runtime::executor::allow_blocking(pull)
}

#[cfg(not(all(any(feature = "python", feature = "node"), not(target_arch = "wasm32"))))]
fn allow_blocking<T>(pull: impl FnOnce() -> T) -> T {
    pull()
}

/// Run `pull` inside Bimetal's runtime, where async streams on a native host
/// make their batches.
#[cfg(all(any(feature = "python", feature = "node"), not(target_arch = "wasm32")))]
fn in_runtime<T>(pull: impl FnOnce() -> T) -> T {
    let _runtime = crate::runtime::executor::enter();
    pull()
}

#[cfg(not(all(any(feature = "python", feature = "node"), not(target_arch = "wasm32"))))]
fn in_runtime<T>(pull: impl FnOnce() -> T) -> T {
    pull()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    use arrow_array::{Int32Array, RecordBatchIterator};
    use arrow_schema::{DataType, Field, Schema};

    fn schema() -> SchemaRef {
        Arc::new(Schema::new(vec![Field::new(
            "n",
            DataType::Int32,
            /* nullable */ false,
        )]))
    }

    fn batch(values: &[i32]) -> RecordBatch {
        RecordBatch::try_new(schema(), vec![Arc::new(Int32Array::from(values.to_vec()))])
            .expect("valid batch")
    }

    /// A stream that yields the items it was given, one per poll.
    struct Items(Vec<Result<RecordBatch, ArrowError>>);

    impl Stream for Items {
        type Item = Result<RecordBatch, ArrowError>;

        fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Batch>> {
            Poll::Ready((!self.0.is_empty()).then(|| self.0.remove(0)))
        }
    }

    #[test]
    fn synchronous_code_iterates_both_sources() {
        let streamed =
            RecordBatchStream::new(schema(), Items(vec![Ok(batch(&[1])), Ok(batch(&[2]))]));
        let read: Vec<_> = streamed
            .into_iter()
            .collect::<Result<_, _>>()
            .expect("batches");
        assert_eq!(read, [batch(&[1]), batch(&[2])]);

        let reader = RecordBatchIterator::new([Ok(batch(&[3]))], schema());
        let read: Vec<_> = RecordBatchStream::from_reader(reader)
            .into_iter()
            .collect::<Result<_, _>>()
            .expect("batches");
        assert_eq!(read, [batch(&[3])]);
    }

    #[test]
    fn a_batch_of_another_schema_fails() {
        let other = Arc::new(Schema::new(vec![Field::new(
            "m",
            DataType::Int32,
            /* nullable */ false,
        )]));
        let stray = RecordBatch::try_new(other, vec![Arc::new(Int32Array::from(vec![1]))])
            .expect("valid batch");
        let mut batches = RecordBatchStream::new(schema(), Items(vec![Ok(stray)])).into_iter();
        let error = batches
            .next()
            .expect("an item")
            .expect_err("the schemas differ");
        assert!(matches!(error, ArrowError::SchemaError(_)), "{error}");
    }

    /// A typed error, as `#[derive(bimetal::Error)]` implements it.
    #[derive(Debug)]
    struct Missing;

    impl fmt::Display for Missing {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("no table named t")
        }
    }

    impl ContractError for Missing {
        fn type_identity() -> crate::ir::DefinitionId {
            unreachable!("the test never registers the error")
        }

        fn code(&self) -> &'static str {
            "not_found"
        }
    }

    /// A stream of typed errors.
    struct Failing;

    impl Stream for Failing {
        type Item = Result<RecordBatch, Missing>;

        fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(Some(Err(Missing)))
        }
    }

    #[test]
    fn a_typed_error_keeps_its_code() {
        let mut batches = RecordBatchStream::new(schema(), Failing).into_iter();
        let error = batches
            .next()
            .expect("an item")
            .expect_err("the stream fails");
        assert_eq!(
            error.to_string(),
            "External error: not_found: no table named t"
        );
    }

    #[test]
    fn an_async_reader_pulls_a_blocking_source_on_a_blocking_thread() {
        let caller = thread::current().id();
        let reader = RecordBatchIterator::new(
            [batch(&[1]), batch(&[2])].into_iter().map(move |batch| {
                assert_ne!(thread::current().id(), caller, "the pull runs elsewhere");
                Ok(batch)
            }),
            schema(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime");
        let mut stream = RecordBatchStream::from_reader(reader);
        let read = runtime.block_on(async {
            let mut read = Vec::new();
            while let Some(batch) =
                future::poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await
            {
                read.push(batch.expect("a batch"));
            }
            read
        });
        assert_eq!(read, [batch(&[1]), batch(&[2])]);
    }
}
