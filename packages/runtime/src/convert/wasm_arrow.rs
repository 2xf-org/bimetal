//! Arrow values in the Wasm module, as the IPC streams of `arrow_ipc` in
//! `Uint8Array`s.
//!
//! A stream crosses as an object of closures, which JavaScript's garbage
//! collector frees: a returned stream's `next` and `close`, and the feed of a
//! stream parameter, which the generated JavaScript's `start` receives.

use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::js_sys::{Function, Object, Reflect, Uint8Array};

use super::arrow_ipc::{self, Encoded, Feed, NEXT, Outgoing};
use super::wasm::{Parameter, argument_error, promise, runtime_error};
use crate::arrow::{ArrowValue, Parts, RecordBatchStream};
use crate::ir::ArrowKind;
use crate::runtime::Resource;

/// Decode an Arrow argument: an IPC stream in a `Uint8Array`, or, for a
/// stream, the source that the generated JavaScript made.
///
/// # Errors
///
/// Returns a JavaScript `TypeError` that names the parameter when the
/// argument has the wrong shape, and an `Error` when its data is invalid.
pub fn arrow_from_host<T: ArrowValue>(value: JsValue, parameter: &Parameter) -> Result<T, JsValue> {
    if T::KIND == ArrowKind::Stream {
        let parts = Parts::Stream(stream_from_host(&value, parameter)?);
        return T::from_parts(parts).map_err(|error| runtime_error(&error));
    }
    let stream = bytes(value, parameter)?;
    arrow_ipc::decode(stream).map_err(|error| runtime_error(&error))
}

/// Encode an Arrow value as an IPC stream in a `Uint8Array`, or a stream as
/// the object that the generated JavaScript reads.
///
/// # Errors
///
/// Returns a JavaScript `Error` when the value cannot be encoded.
pub fn arrow_to_host<T: ArrowValue>(value: T) -> Result<JsValue, JsValue> {
    match arrow_ipc::encode(value).map_err(|error| runtime_error(&error))? {
        Encoded::Batches(stream) => Ok(Uint8Array::from(stream.as_slice()).into()),
        Encoded::Stream { outgoing, schema } => Ok(stream_to_host(outgoing, &schema)),
    }
}

/// Copy the bytes of a `Uint8Array` argument into Rust memory.
fn bytes(value: JsValue, parameter: &Parameter) -> Result<Vec<u8>, JsValue> {
    value
        .dyn_into::<Uint8Array>()
        .map(|bytes| bytes.to_vec())
        .map_err(|_| argument_error(&parameter.javascript_message("must be a Uint8Array")))
}

/// Make the object through which JavaScript reads a returned stream: its
/// `schema`, `next()`, which resolves with each batch or `null` at the end,
/// and `close()`, which drops the stream once no read holds it.
fn stream_to_host(outgoing: Outgoing, schema: &[u8]) -> JsValue {
    let outgoing = Resource::new(outgoing);
    let reading = outgoing.clone();
    let next = Closure::<dyn FnMut() -> JsValue>::new(move || {
        let held = reading.write_owned(NEXT);
        promise(async move {
            let mut outgoing = held.map_err(|error| runtime_error(&error))?.await;
            match outgoing.next().await {
                Ok(Some(batch)) => Ok(Uint8Array::from(batch.as_slice()).into()),
                Ok(None) => Ok(JsValue::NULL),
                Err(message) => Err(runtime_error(&message)),
            }
        })
    });
    let close = Closure::<dyn FnMut()>::new(move || outgoing.close());
    object(&[
        ("schema", Uint8Array::from(schema).into()),
        ("next", next.into_js_value()),
        ("close", close.into_js_value()),
    ])
}

/// Read a stream argument: give its `start` a feed, into which it writes each
/// batch when Rust asks.
fn stream_from_host(value: &JsValue, parameter: &Parameter) -> Result<RecordBatchStream, JsValue> {
    let wrong_type = || argument_error(&parameter.javascript_message("must be a stream source"));
    let schema = Reflect::get(value, &JsValue::from("schema")).map_err(|_| wrong_type())?;
    let start = Reflect::get(value, &JsValue::from("start"))
        .ok()
        .and_then(|start| start.dyn_into::<Function>().ok())
        .ok_or_else(wrong_type)?;
    let (feed, stream) =
        Feed::new(bytes(schema, parameter)?).map_err(|error| runtime_error(&error))?;
    let feed = Rc::new(feed);
    let asking = Rc::clone(&feed);
    let asked = Closure::<dyn FnMut() -> JsValue>::new(move || {
        let asked = asking.asked();
        promise(async move { Ok(JsValue::from(asked.await)) })
    });
    let writing = Rc::clone(&feed);
    let write = Closure::<dyn FnMut(Uint8Array)>::new(move |batch: Uint8Array| {
        writing.write(batch.to_vec());
    });
    let finishing = Rc::clone(&feed);
    let finish = Closure::<dyn FnMut()>::new(move || finishing.finish());
    let fail = Closure::<dyn FnMut(String)>::new(move |message| feed.fail(message));
    let feed = object(&[
        ("asked", asked.into_js_value()),
        ("write", write.into_js_value()),
        ("finish", finish.into_js_value()),
        ("fail", fail.into_js_value()),
    ]);
    start.call1(value, &feed)?;
    Ok(stream)
}

/// Make a plain object with these properties.
fn object(properties: &[(&str, JsValue)]) -> JsValue {
    let object = Object::new();
    for (key, value) in properties {
        // Setting a property of a new plain object cannot fail.
        let _ = Reflect::set(&object, &JsValue::from(*key), value);
    }
    object.into()
}
