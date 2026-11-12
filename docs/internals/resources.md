# Resources and callbacks

Read [Resources](../concepts/resources.md) first. Serde can't carry a resource or a function, so both cross as host objects, on paths of their own.

## What holds the value

Every resource keeps its value in `bimetal::runtime::Resource` (`bimetal/src/runtime/resource.rs`): an outer `Arc<Mutex<Option<...>>>`, which `close` and `take` empty, around an `Arc<RwLock<T>>`, a Tokio lock. Each call clones the inner `Arc`, so an async call keeps the value after `close()`.

`&self` methods take the read lock, and `&mut self` methods the write lock. A Python synchronous method takes a free lock with the interpreter lock held, waits for a taken one without the interpreter lock (`sync_lock` in `bimetal-macros/src/export/boundary.rs`), and keeps the lock while it converts its result. Wasm and Node.js can't block the JavaScript thread, so a synchronous method that finds the lock taken reports that the resource is busy.

An async method tries its lock on the calling thread when the host makes the call, which in Python is when the coroutine first runs. If another call holds it, the method joins the lock's queue when its future is first polled, which on the native hosts gives calls waiting this way no guaranteed order. It keeps the lock until its result has converted: inside the future in Wasm, and in the deferred conversion in Python and Node.js.

`take`, which backs `self` methods and `T` parameters, moves the value out only when nothing else holds it, and reports the resource busy otherwise.

## Resources in signatures

A resource's `#[bimetal::export] impl` implements `ResourceType`, which gives the contract its identity, and each host's `HostResource`, which makes the object of the native class and finds the `Resource` behind one. `returns(resource)` converts through `ResourceOutput`, which also covers `Option` and `Vec`.

Python and Node.js find the class by checking the object. wasm-bindgen gives Rust no typed access to the value behind a `JsValue`, so `convert::wasm` calls the class's `__bimetal_share`, which leaves a clone of the `Resource` in a thread-local, and downcasts it.

The generated class of each host wraps a returned native object through `__bimetal_wrap__` or `$wrap`, and unwraps an argument through the runtime's `native_resource` or `nativeResource`, which checks its class first.

`ReachedResources` tracks which resources a call has reached. A second reach of the same resource fails with `ResourceError::Aliased`, because a call that waited for itself would never end. It's local to one wrapper, so it doesn't see a call nested through a callback. The next rule covers that case.

## Protocols

A protocol adds no native code. The macro records it on the method's `MethodDef`, `project::validation::resources` checks the method's signature, and the generators write the special members that call the method.

Python passes the block's real exception state to `exit`. TypeScript's disposal always passes `failed = true`, because JavaScript can't see the exception. Async hooks make plain `with` and `using` raise a `TypeError`.

## Callbacks

`bimetal::Callback` holds a closure that the host's `callback_from_host` builds, so the conversions are the concrete types of each export, and a value crosses as an export's values do.

`Callback::call` marks its thread for as long as the function runs (`runtime::inside_callback`). `boundary::sync_lock` checks this mark on the host that can wait, so a synchronous call inside a callback uses `try_read` or `try_write`, and reports `ResourceError::Busy` instead of blocking. The outer call may hold that lock while it waits for the function, so blocking there could never end.

In Python, a call takes the interpreter lock, and `executor::allow_blocking` lets a Python callback made on a Tokio worker call back into a synchronous method. On the addon, the closure calls the function directly on the JavaScript thread, and from any other thread goes through a thread-safe function and waits for the answer (`runtime/node/callback.rs`), holding its slot across the call so that a shutdown can't free the function under it.

## Error details

An error's `#[bimetal(details)]` registers a generated model through the same `TypeRegistration` as `Model`, and serializes the fields as JSON in `ContractError::details`. Each host carries that text beside the code, and the generated code reads it as the model.

## Lifetimes across a trap

On Wasm, a trap leaves the module unusable. The generated `close()` and the finalizers check for that first, so a `using` block reports the panic itself, and disposing after a trap does nothing.
