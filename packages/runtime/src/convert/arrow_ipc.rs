//! Arrow values as the IPC streams that the JavaScript hosts pass.
//!
//! apache-arrow has no C Data Interface, so values cross as an IPC stream: one
//! copy between the host and Rust memory, and one into or out of the stream.
//!
//! A `RecordBatchStream` crosses one batch at a time, each batch an IPC
//! stream of its own, which [`encode`] and [`decode`] write and read.
//! JavaScript reads a returned stream through an [`Outgoing`], and writes a
//! stream parameter through a [`Feed`], when Rust asks for each batch.

use std::future::{self, Future};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll, ready};
#[cfg(not(target_arch = "wasm32"))]
use std::thread::{self, ThreadId};

use arrow_array::{Array, RecordBatch};
use arrow_buffer::Buffer;
use arrow_ipc::reader::StreamDecoder;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{ArrowError, Schema, SchemaRef};
use futures_core::Stream;
use tokio::sync::{Mutex, mpsc};

use crate::arrow::{self, ArrowValue, Parts, RecordBatchStream};
#[cfg(not(target_arch = "wasm32"))]
use crate::runtime::executor;

/// Decode an Arrow value from an IPC stream. Aligned buffers keep the
/// stream's memory rather than copying it. A stream's parts cross on their
/// own, through a [`Feed`].
///
/// # Errors
///
/// Returns an [`ArrowError`] when the stream or its data is invalid.
pub(super) fn decode<T: ArrowValue>(stream: Vec<u8>) -> Result<T, ArrowError> {
    let mut buffer = Buffer::from_vec(stream);
    let mut decoder = StreamDecoder::new();
    let mut batches = Vec::new();
    while !buffer.is_empty() {
        if let Some(batch) = decoder.decode(&mut buffer)? {
            batches.push(batch);
        }
    }
    decoder.finish()?;
    let schema = decoder
        .schema()
        .ok_or_else(|| ArrowError::IpcError("the Arrow stream has no schema".to_owned()))?;
    T::from_parts(Parts::Batches { schema, batches })
}

/// An Arrow value as a JavaScript host passes it.
pub(super) enum Encoded {
    /// An IPC stream of the value's batches.
    Batches(Vec<u8>),
    /// A stream, which JavaScript reads one batch at a time.
    Stream {
        /// The stream.
        outgoing: Outgoing,
        /// Its schema, as an IPC stream.
        schema: Vec<u8>,
    },
}

/// Encode an Arrow value as an IPC stream, or a stream for JavaScript to
/// read.
///
/// # Errors
///
/// Returns an [`ArrowError`] when the value or the schema cannot be encoded.
pub(super) fn encode<T: ArrowValue>(value: T) -> Result<Encoded, ArrowError> {
    Ok(match value.into_parts() {
        Parts::Batches { schema, batches } => Encoded::Batches(write(&schema, &batches)?),
        Parts::Stream(stream) => Encoded::Stream {
            schema: write(stream.schema(), &[])?,
            outgoing: Outgoing::new(stream),
        },
    })
}

/// Write `batches`, which all have `schema`, as an IPC stream.
fn write(schema: &Schema, batches: &[RecordBatch]) -> Result<Vec<u8>, ArrowError> {
    // Reserving the stream's size keeps the vector from growing: Wasm memory
    // never shrinks, so every smaller buffer would stay in the peak.
    let data: usize = batches.iter().map(stream_size).sum();
    let mut writer = StreamWriter::try_new(Vec::with_capacity(data + STREAM_OVERHEAD), schema)?;
    for batch in batches {
        writer.write(batch)?;
    }
    writer.into_inner()
}

/// More than the bytes that the stream adds to its batches' data: the
/// schema, each message's header, and the end marker.
const STREAM_OVERHEAD: usize = 64 << 10;

/// Return at least the bytes of a batch's message: the data of its columns'
/// slices, each buffer padded to the writer's 64-byte alignment.
fn stream_size(batch: &RecordBatch) -> usize {
    batch
        .columns()
        .iter()
        .map(|column| {
            let data = column.to_data();
            let buffers = data.buffers().len() + 1 + data.child_data().len();
            data.get_slice_memory_size().unwrap_or(0) + 64 * buffers
        })
        .sum::<usize>()
        + 1024
}

/// The callable that a read of a closed stream names.
pub(super) const NEXT: &str = "RecordBatchStream.next";

/// A returned stream, which JavaScript reads one batch at a time.
///
/// The source drops as soon as it ends or fails, and every later read ends,
/// or fails with the same message.
pub(super) struct Outgoing {
    /// The source, until it ends. Reads hold the stream alone and take no
    /// lock: the mutex only makes it `Sync`, as a value that the addon's
    /// threads share must be.
    stream: StdMutex<Option<RecordBatchStream>>,
    failure: Option<String>,
}

impl Outgoing {
    /// Read `stream`.
    fn new(stream: RecordBatchStream) -> Self {
        Self {
            stream: StdMutex::new(Some(stream)),
            failure: None,
        }
    }

    /// Read the next batch as an IPC stream, or `None` at the end.
    ///
    /// # Errors
    ///
    /// Returns the message of the failure that ended the stream.
    pub(super) async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        if let Some(message) = &self.failure {
            return Err(message.clone());
        }
        let Some(stream) = self.source() else {
            return Ok(None);
        };
        let batch = future::poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await;
        let Some(batch) = batch else {
            self.source().take();
            return Ok(None);
        };
        let encoded = batch.and_then(|batch| write(&batch.schema(), &[batch]));
        encoded.map(Some).map_err(|error| {
            let message = arrow::failure_message(error);
            self.fail(message.clone());
            message
        })
    }

    /// End the stream with `message`, unless it already failed, and drop its
    /// source, as a read that panicked must.
    pub(super) fn fail(&mut self, message: String) {
        if self.failure.is_none() {
            let source = self.source().take();
            self.failure = Some(message);
            drop(source);
        }
    }

    /// The source, until it ends.
    ///
    /// # Panics
    ///
    /// Never: nothing locks the stream, so nothing poisons it.
    fn source(&mut self) -> &mut Option<RecordBatchStream> {
        self.stream
            .get_mut()
            .expect("nothing locks the stream, so nothing poisons it")
    }
}

/// What JavaScript writes into a stream parameter.
enum Written {
    /// One batch, as an IPC stream.
    Batch(Vec<u8>),
    /// The end of the stream.
    End,
    /// The failure that ended the stream, by its message.
    Failed(String),
}

/// JavaScript's end of a stream parameter. JavaScript waits for Rust to ask
/// for a batch, then writes it, the end, or a failure.
pub(super) struct Feed {
    asks: Arc<Mutex<mpsc::Receiver<()>>>,
    written: mpsc::UnboundedSender<Written>,
}

impl Feed {
    /// Make a feed, and the stream of what JavaScript writes into it, whose
    /// schema the IPC stream `schema` holds.
    ///
    /// # Errors
    ///
    /// Returns an [`ArrowError`] when `schema` is not a valid IPC stream.
    pub(super) fn new(schema: Vec<u8>) -> Result<(Self, RecordBatchStream), ArrowError> {
        let schema: SchemaRef = decode(schema)?;
        let (ask, asks) = mpsc::channel(1);
        let (write, written) = mpsc::unbounded_channel();
        let fed = Fed {
            ask: Some(ask),
            written,
            asked: false,
            #[cfg(not(target_arch = "wasm32"))]
            javascript: thread::current().id(),
        };
        let feed = Self {
            asks: Arc::new(Mutex::new(asks)),
            written: write,
        };
        Ok((feed, RecordBatchStream::new(schema, fed)))
    }

    /// Wait until Rust asks for the next batch: `true`, or `false` once the
    /// stream has dropped, when JavaScript should close its source.
    pub(super) fn asked(&self) -> impl Future<Output = bool> + Send + 'static {
        let asks = Arc::clone(&self.asks);
        async move { asks.lock().await.recv().await.is_some() }
    }

    /// Write a batch, as an IPC stream.
    pub(super) fn write(&self, batch: Vec<u8>) {
        // A stream that has dropped reads nothing more.
        let _ = self.written.send(Written::Batch(batch));
    }

    /// Write the end of the stream.
    pub(super) fn finish(&self) {
        let _ = self.written.send(Written::End);
    }

    /// End the stream with a failure.
    pub(super) fn fail(&self, message: String) {
        let _ = self.written.send(Written::Failed(message));
    }
}

/// Rust's end of a [`Feed`].
struct Fed {
    /// Asks JavaScript for a batch, until the stream ends.
    ask: Option<mpsc::Sender<()>>,
    written: mpsc::UnboundedReceiver<Written>,
    /// Whether JavaScript was asked for the batch that the stream waits for.
    asked: bool,
    /// The addon's JavaScript thread, which made the feed, and which only an
    /// async reader leaves free to write into it.
    #[cfg(not(target_arch = "wasm32"))]
    javascript: ThreadId,
}

impl Stream for Fed {
    type Item = Result<RecordBatch, ArrowError>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(ask) = &self.ask else {
            return Poll::Ready(None);
        };
        if !self.asked {
            // JavaScript took the previous ask before it wrote, so the channel
            // has room, unless JavaScript dropped the feed, which the read
            // below reports.
            let _ = ask.try_send(());
            self.asked = true;
        }
        let written = self.written.poll_recv(context);
        // Code that waits on the JavaScript thread would wait for what only
        // that thread can write. An async call's first poll returns instead,
        // which lets it write. Wasm's blocking reads stop on their own.
        #[cfg(not(target_arch = "wasm32"))]
        if written.is_pending()
            && thread::current().id() == self.javascript
            && !executor::in_first_poll()
        {
            self.ask = None;
            return Poll::Ready(Some(Err(arrow::failure(arrow::READ_WITH_AWAIT.to_owned()))));
        }
        let batch = match ready!(written) {
            Some(Written::Batch(batch)) => {
                self.asked = false;
                return Poll::Ready(Some(decode(batch)));
            }
            Some(Written::End) => None,
            Some(Written::Failed(message)) => Some(Err(arrow::failure(message))),
            None => Some(Err(arrow::failure(
                "the JavaScript source stopped without finishing".to_owned(),
            ))),
        };
        self.ask = None;
        Poll::Ready(batch)
    }
}
