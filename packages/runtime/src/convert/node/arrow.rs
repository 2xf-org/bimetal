//! Arrow values on the Node.js addon, as the IPC streams of `arrow_ipc` in
//! `Uint8Array`s, as the Wasm module passes them.
//!
//! A stream crosses as an object of a class that the addon defines: a
//! returned stream's reader, with `next` and `close`, and the feed of a
//! stream parameter, which the generated JavaScript's `start` receives. Each
//! drops its Rust half when the garbage collector collects it. A pending
//! `asked()` leaves Node.js free to exit, as Rust's own pending calls keep it
//! running while Rust reads.

use super::boundary::{Parameter, bytes_from_host, bytes_to_host, runtime_error};
use crate::arrow::{ArrowValue, Parts, RecordBatchStream};
use crate::convert::arrow_ipc::{self, Encoded, Feed, NEXT, Outgoing};
use crate::ir::ArrowKind;
use crate::runtime::Resource;
use crate::runtime::executor::{self, Pace, Start};
use crate::runtime::node::{
    self, CallInfo, Class, ClassDescriptor, Env, Error, Method, Registration, Value,
};

/// Decode an Arrow argument: an IPC stream in a `Uint8Array`, or, for a
/// stream, the source that the generated JavaScript made.
///
/// # Errors
///
/// Returns a `TypeError` that names the parameter when the argument has the
/// wrong shape, and a JavaScript `Error` when its data is invalid.
pub fn arrow_from_host<T: ArrowValue>(value: Value<'_>, parameter: &Parameter) -> Result<T, Error> {
    if T::KIND == ArrowKind::Stream {
        let parts = Parts::Stream(stream_from_host(value, parameter)?);
        return T::from_parts(parts).map_err(|error| runtime_error(&error));
    }
    let stream = bytes_from_host(value, parameter)?;
    arrow_ipc::decode(stream).map_err(|error| runtime_error(&error))
}

/// Encode an Arrow value as an IPC stream in a `Uint8Array`, or a stream as
/// the object that the generated JavaScript reads.
///
/// # Errors
///
/// Returns a JavaScript `Error` when the value cannot be encoded.
pub fn arrow_to_host<T: ArrowValue>(env: Env<'_>, value: T) -> Result<Value<'_>, Error> {
    match arrow_ipc::encode(value).map_err(|error| runtime_error(&error))? {
        Encoded::Batches(stream) => Ok(bytes_to_host(env, &stream)),
        Encoded::Stream { outgoing, schema } => {
            let reader = node::instance(env, ReaderObject(Resource::new(outgoing)))?;
            reader.set_named(c"schema", bytes_to_host(env, &schema))?;
            Ok(reader)
        }
    }
}

/// Read a stream argument: give its `start` a feed, into which it writes each
/// batch when Rust asks.
fn stream_from_host(value: Value<'_>, parameter: &Parameter) -> Result<RecordBatchStream, Error> {
    let env = value.env();
    let start = value.named(c"start")?;
    if !start.is_function()? {
        return Err(Error::Type(
            parameter.javascript_message("must be a stream source"),
        ));
    }
    let schema = bytes_from_host(value.named(c"schema")?, parameter)?;
    let (feed, stream) = Feed::new(schema).map_err(|error| runtime_error(&error))?;
    let feed = node::instance(env, FeedObject(feed))?;
    start.call(value, [feed])?;
    Ok(stream)
}

/// A returned stream, which JavaScript reads. Closing it drops the stream
/// once no read holds it.
struct ReaderObject(Resource<Outgoing>);

/// The class of [`ReaderObject`].
static READER: ClassDescriptor = ClassDescriptor::new::<ReaderObject>(
    "__bimetal_arrow_reader",
    None,
    &[],
    &[
        Method {
            name: "next",
            call: read_next,
        },
        Method {
            name: "close",
            call: close_reader,
        },
    ],
);

impl Class for ReaderObject {
    fn descriptor() -> &'static ClassDescriptor {
        &READER
    }
}

inventory::submit! { Registration::Class(&READER) }

/// Resolve with the next batch, or `null` at the end. A panic ends the
/// stream, as a failure does. Each batch starts on the JavaScript thread, as
/// an async call does, unless making and encoding batches has worked too long
/// for that thread, and a blocking reader pulls on a blocking thread.
fn read_next(call: CallInfo<'_>) -> Result<Value<'_>, Error> {
    static PACE: Pace = Pace::new();
    let held = call
        .this::<ReaderObject>()?
        .0
        .write_owned(NEXT)
        .map_err(|error| runtime_error(&error))?;
    node::awaitable(call.env(), Start::Caller(&PACE), async move {
        let mut outgoing = held.await;
        let batch = executor::catch_panic(outgoing.next(), |panic| format!("panic: {panic}")).await;
        if let Err(message) = &batch {
            outgoing.fail(message.clone());
        }
        let batch = batch.map_err(Error::Message)?;
        Ok(node::deferred(move |env| match batch {
            Some(batch) => Ok(bytes_to_host(env, &batch)),
            None => env.null(),
        }))
    })
}

/// Drop the stream, once no read holds it.
fn close_reader(call: CallInfo<'_>) -> Result<Value<'_>, Error> {
    call.this::<ReaderObject>()?.0.close();
    Ok(call.env().undefined())
}

/// The feed of a stream argument, which JavaScript writes.
struct FeedObject(Feed);

/// The class of [`FeedObject`].
static FEED: ClassDescriptor = ClassDescriptor::new::<FeedObject>(
    "__bimetal_arrow_feed",
    None,
    &[],
    &[
        Method {
            name: "asked",
            call: feed_asked,
        },
        Method {
            name: "write",
            call: feed_write,
        },
        Method {
            name: "finish",
            call: feed_finish,
        },
        Method {
            name: "fail",
            call: feed_fail,
        },
    ],
);

impl Class for FeedObject {
    fn descriptor() -> &'static ClassDescriptor {
        &FEED
    }
}

inventory::submit! { Registration::Class(&FEED) }

/// The argument of the feed's `write`, which only the generated JavaScript
/// calls.
const BATCH: Parameter = Parameter {
    callable: "RecordBatchStream.write",
    name: "batch",
};

/// Resolve with `true` when Rust asks for the next batch, or `false` once the
/// stream has dropped.
fn feed_asked(call: CallInfo<'_>) -> Result<Value<'_>, Error> {
    static PACE: Pace = Pace::new();
    let asked = call.this::<FeedObject>()?.0.asked();
    node::unref_awaitable(call.env(), Start::Caller(&PACE), async move {
        let asked = asked.await;
        Ok(node::deferred(move |env| env.boolean(asked)))
    })
}

/// Write a batch, an IPC stream in a `Uint8Array`.
fn feed_write(call: CallInfo<'_>) -> Result<Value<'_>, Error> {
    let [batch] = call.arguments()?;
    call.this::<FeedObject>()?
        .0
        .write(bytes_from_host(batch, &BATCH)?);
    Ok(call.env().undefined())
}

/// Write the end of the stream.
fn feed_finish(call: CallInfo<'_>) -> Result<Value<'_>, Error> {
    call.this::<FeedObject>()?.0.finish();
    Ok(call.env().undefined())
}

/// End the stream with a failure, whose message is the argument.
fn feed_fail(call: CallInfo<'_>) -> Result<Value<'_>, Error> {
    let [message] = call.arguments()?;
    call.this::<FeedObject>()?.0.fail(message.to_js_string()?);
    Ok(call.env().undefined())
}
