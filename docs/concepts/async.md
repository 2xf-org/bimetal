# Async calls

An `async fn` becomes a Python coroutine and a TypeScript function that returns a `Promise`. The hosts differ in what runs it.

| | Runs on | Needs |
| --- | --- | --- |
| Python | The caller's thread to the first wait, then a Tokio runtime that the extension shares | A running asyncio event loop |
| Node.js addon | The JavaScript thread to the first wait, then a Tokio runtime that the addon shares | Nothing |
| Wasm | The JavaScript event loop | Nothing |

On Python and the addon, synchronous constructors and factories run inside the runtime too, as do synchronous functions and methods that return a resource, so a resource can `tokio::spawn` the tasks that it keeps. Other synchronous calls don't, so a package with no resources or async calls never starts the runtime's threads.

## When a call starts

A Python coroutine starts when it's awaited or scheduled, for example with `asyncio.create_task`. A JavaScript call checks and converts its arguments at once. Its Rust body starts at once on the addon, and on Wasm in a microtask, once the calling code yields. Both try to take a method's resource when the call starts. If another call holds it, the call waits in its future, and on Python and the addon calls waiting this way have no guaranteed order among themselves.

## The first wait

On Python and the addon, a call runs on the caller's thread until it first waits. A call that finishes there, as a read from memory does, settles its future or promise at once, and no worker runs it. The first async call still starts the runtime's threads. A call that waits moves to the runtime, where a worker finishes it and hands the result back.

A call that takes a `Callback` or a `RecordBatchStream` starts on a worker instead. Before its first wait, it could wait for the caller's event loop: a function that waits for a coroutine on that loop, or a stream that JavaScript writes. On the caller's thread, that wait would never end, and on Python a blocking read of a stream would stop the event loop.

On Python the first part runs without the interpreter lock, as a synchronous call that isn't `brief` does. An `await` of a call that finished doesn't suspend the caller, so other tasks of the event loop get no turn from it.

Long work before the first wait would stop the caller's event loop, so each async export learns how long its calls work, and a call of an export that works long starts on a worker instead. That happens when the export works for more than about 60 µs, which is 2.5 times what handing a call to a worker costs, or for more than about 25 µs while other calls of it run on workers, so concurrent calls spread over cores. An export learns from its calls as they run, and one long call moves nothing, since the system may have stalled it. An export whose calls turn slow moves after two long calls in a row when they take more than about 250 µs, and after a few more when they take less, so those first calls still run on the caller's thread. Work that is long on every call can still go to `tokio::task::spawn_blocking`, or follow `tokio::task::yield_now().await`, which moves the whole call to a worker.

The caller's thread polls with a waker that wakes no one, and a worker's poll replaces it. So a hand-written future must register its waker on every poll, as Tokio's futures do. Before the first wait the body isn't a Tokio task yet, so `tokio::task::id()` panics there.

## Cancellation

Cancelling a Python task drops the Rust future as soon as a runtime thread gets to it, or right after a poll in progress. A call that finished before its first wait has nothing left to cancel. A JavaScript promise can't be cancelled, so the call runs to its end.

## Fork

A Python child process that `fork` creates starts its own runtime at its first async call or synchronous constructor, so `multiprocessing` with the `fork` start method works.

Calls running in the parent don't continue in the child, and cancelling one there does nothing. A resource one of them held stays held in the child, and a call to it that needs the resource waits forever. So fork before such calls start, or create the resources in the child.

## The rules on a future

An exported future must be `Send` and `'static`, because Tokio can move a future between threads. It can't hold an `Rc` or a `RefCell` borrow across an `.await`, and Bimetal checks this when the crate compiles. The call owns its arguments, so `&str` and `&[T]` parameters work, but an async export can't return a reference.

## Tokio on Wasm

Tokio's I/O, timers and `tokio::spawn` are available on every host but Wasm. Code that the Wasm module runs must not depend on them, or must choose an implementation per target with `#[cfg(target_arch = "wasm32")]`. Runtime-independent code, such as `tokio::sync`, works everywhere.

[Async](../guides/async.md) shows the syntax, and what to do with a library that spawns tasks internally.
