# The life of a call

Read [How values cross](../concepts/crossing.md) first. This page follows one call, from the macro that plans it to the value that comes back.

## One plan

The macros check each exported signature once (`types::Signature` in `bimetal-macros/src/types/callable.rs`), for the contract and every host, and plan every wrapper with one planner, `wrapper::plan` in `bimetal-macros/src/export/wrapper.rs`. It decodes the arguments, reaches the resource value, calls Rust, and converts the result, the same way for sync and async calls, free functions and resource members.

Where hosts differ, it asks the host (`HostBackend` in `export/host_backend.rs`) instead of branching on it, and each host's item shells live in their own file: `export/python.rs`, `export/node.rs` and `export/wasm.rs`. The wrappers carry the final host names of their parameters, which their argument errors use.

## Into Rust

**Python.** A scalar, a container of scalars or a buffer crosses as it is. Every other argument goes through `dump_host`: Pydantic validates it, so that a dictionary passed for a model meets the model's rules, and then writes it in its JSON mode, in one pass, where a model is a dictionary and a date is ISO 8601 text. A `serde_json::Value` has rules of its own, which its type `JsonValue` applies only in a dump that crosses. Rust reads the result with `pythonize`, then checks its field rules.

**TypeScript.** `checkArgument` checks the outermost type of each argument, `prepareHost` walks the value with its schema, then `serde-wasm-bindgen` reads it, and Rust checks its field rules. The addon reads it with `bimetal::convert::node`, whose deserializer follows serde-wasm-bindgen's checks, order and messages, stops at 128 levels of nesting, where native recursion could overflow the stack, and copies bytes and buffers once out of their typed arrays.

## Out of Rust

**Python.** Rust checks the field rules, `pythonize` writes the value, and Pydantic validates it. `restore_host` runs first only for bytes and buffers. Buffers arrive as a `bytearray`, which NumPy wraps without a copy.

**TypeScript.** Rust checks the field rules, and `serde-wasm-bindgen` writes the value, with maps as `Map`s, as the addon's serializer does. The addon copies bytes and buffers once into plain typed arrays, never a Node.js `Buffer`. `restoreHost` turns each map into a plain object, where a `__proto__` key stays data, freezes the value, and restores bytes and buffers.

## Arrow

**Python.** The Arrow PyCapsule interface, both ways, without a copy (`bimetal/src/convert/python_arrow.rs`), around the C Data Interface code in `convert/arrow_ffi.rs`. Imports are validated. Rust exports small objects that `pa.array`, `pa.record_batch`, `pa.table`, `pa.schema` and `pa.RecordBatchReader.from_stream` import.

A `RecordBatchStream` (`bimetal/src/arrow/record_batch_stream.rs`) holds an async stream or a blocking reader. Exported, it is an `ArrowArrayStream` whose `get_next` waits on the calling thread, with the interpreter lock released if that thread holds it: a blocking reader runs there, and an async stream is polled there inside Bimetal's runtime, with a Tokio worker's other work handed away first. The first failure or panic is kept, so every later `get_next` fails with its message, and the source drops when the stream ends. In a forked child the stream fails and leaks its source, as `Abort` leaves the parent's tasks alone. Imported, a stream reads its schema at once and its batches as Rust asks: synchronous code calls `get_next` on its own thread, and a `poll_next` hands the call to a blocking thread of the runtime that polls it.

**TypeScript streams.** Both backends share `arrow_ipc`'s `Outgoing` and `Feed` and the adapters in `arrow.js`; only the object that carries them differs, closures in `convert/wasm_arrow.rs` and two classes that the addon registers in `convert/node/arrow.rs`. A returned stream is an object whose `schema` is an IPC stream and whose `next()` resolves with each batch as an IPC stream of its own, which the generated `RecordBatchStream` decodes. A stream argument is a source that the async wrapper opens: Rust gives its `start` a feed, whose `asked()` resolves when Rust polls for a batch and resolves `false` once the stream drops, and the source answers with `write`, `finish` or `fail`. The addon's `asked()` doesn't hold the event loop, since Rust's own calls do while it reads. A synchronous call can't wait for JavaScript, so blocking code that reads such a stream fails rather than hangs: in Wasm when the stream waits for anything, and on the addon when it waits on the JavaScript thread. A panic ends a returned stream, as a failure does. Wasm reads go through `checkNative` and `nativePromise`, so a trap rejects them as it rejects calls.

**TypeScript.** An Arrow IPC stream (`bimetal/src/convert/arrow_ipc.rs`): `tableToIPC` and one copy into Rust memory, where the decoder keeps aligned buffers, and on return one copy out and `tableFromIPC`, which keeps views. JavaScript can't keep a view into Wasm memory, because growing it detaches the views, and apache-arrow has no C Data Interface for the addon.

## Async on Python

Bimetal's shared Tokio runtime runs the future (`bimetal/src/runtime/executor.rs`), and `bimetal::runtime::python::awaitable` settles an asyncio future with its result. The event loop and the future wait in a slot that only code holding the interpreter lock touches, and a done callback aborts the task when the caller cancels the future. A `Callback` argument is the exception to the task owning no Python object: if its last clone drops on a worker, PyO3 defers the release to its reference pool.

Synchronous constructors and factories enter the same runtime, so they can spawn tasks. Synchronous functions and methods don't, so a package without resources or async calls never starts it. A forked child starts its own runtime, because the parent's worker threads don't exist in it. The result converts on a blocking thread that holds the interpreter lock, so no Tokio worker waits for the lock to deliver a result.

## Async on the addon

The addon runs the future on the same kind of Tokio runtime, and gives each call a thread-safe function, through which the worker that finishes the call hands its result to the JavaScript thread, which converts it and settles the promise (`bimetal/src/runtime/node/promise.rs`).

## Async on Wasm

A synchronous export returns the promise of `bimetal::convert::wasm::promise`. wasm-bindgen 0.2.126's own async exports read their arguments only when the future first runs, so Bimetal doesn't use them.

## Traps

The CLI routes every call that `native.js` makes into the module, including the async executor's microtasks and the finalizers, through the runtime's guard. An exception that leaves the module means a trap cut a call short, so the guard aborts the module: it rejects every pending promise, which the trap would leave pending, and `checkNative` keeps every later call out. The panic hook first gives the guard the panic's message.

## Panics on native hosts

A panic must not unwind into a host. So every native host catches at its boundary, through `executor::catch`, which drops the caught payload in turn, and leaks it only if its own drop panics. Python raises `PanicException`, in `bimetal/src/runtime/python.rs` and for a panic while a Tokio worker polls, in `executor.rs`. The addon catches every call from Node.js into Rust in `bimetal/src/runtime/node/`: the callbacks, the module initializer, the conversion of an async result and the finalizers.
