# Async calls

An `async fn` becomes a Python coroutine and a TypeScript function that returns a `Promise`. The hosts differ in what runs it.

| | Runs on | Needs |
| --- | --- | --- |
| Python | A Tokio runtime that the extension shares | A running asyncio event loop |
| Node.js addon | A Tokio runtime that the addon shares | Nothing |
| Wasm | The JavaScript event loop | Nothing |

On Python and the addon, synchronous constructors and factories run inside the runtime too, as do synchronous functions and methods that return a resource, so a resource can `tokio::spawn` the tasks that it keeps. Other synchronous calls don't, so a package with no resources or async calls never starts the runtime's threads.

## When a call starts

A Python coroutine starts when it's awaited or scheduled, for example with `asyncio.create_task`. A JavaScript call checks and converts its arguments at once. Its Rust body starts at once on the addon, and on Wasm in a microtask, once the calling code yields. Both try to take a method's resource when the call starts. If another call holds it, the call waits in its future, and on Python and the addon calls waiting this way have no guaranteed order among themselves.

## Cancellation

Cancelling a Python task drops the Rust future as soon as a runtime thread gets to it, or right after a poll in progress. A JavaScript promise can't be cancelled, so the call runs to its end.

## Fork

A Python child process that `fork` creates starts its own runtime at its first async call or synchronous constructor, so `multiprocessing` with the `fork` start method works.

Calls running in the parent don't continue in the child, and cancelling one there does nothing. A resource one of them held stays held in the child, and a call to it that needs the resource waits forever. So fork before such calls start, or create the resources in the child.

## The rules on a future

An exported future must be `Send` and `'static`, because Tokio can move a future between threads. It can't hold an `Rc` or a `RefCell` borrow across an `.await`, and Bimetal checks this when the crate compiles. The call owns its arguments, so `&str` and `&[T]` parameters work, but an async export can't return a reference.

## Tokio on Wasm

Tokio's I/O, timers and `tokio::spawn` are available on every host but Wasm. Code that the Wasm module runs must not depend on them, or must choose an implementation per target with `#[cfg(target_arch = "wasm32")]`. Runtime-independent code, such as `tokio::sync`, works everywhere.

[Async](../guides/async.md) shows the syntax, and what to do with a library that spawns tasks internally.
