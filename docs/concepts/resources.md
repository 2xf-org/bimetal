# Resources

A *resource* is a Rust value that stays alive between host calls, and the host sees it as a class. The host holds a handle, and the value stays in Rust memory.

## Who can use it at once

Your signature decides. `&self` methods can run at the same time, from several Python threads or async calls. A `&mut self` method runs alone, and the others wait for it. Bimetal holds each resource behind a read-write lock, so you get Rust's aliasing rule across the language boundary.

Methods run without Python's interpreter lock, so other Python threads keep running during a long call.

| The call | Python | JavaScript |
| --- | --- | --- |
| A sync method finds the resource taken | Waits, without the interpreter lock | Throws `the resource is busy` |
| A sync method inside a callback finds it taken | Raises `the resource is busy` | Throws `the resource is busy` |
| An async method finds the resource taken | Waits in its future | Waits in its future |

JavaScript has one thread, so a synchronous call can't wait: whatever holds the lock would never get to run. So in JavaScript, a sync method called during an async `&mut self` call fails at once, even right after that call starts and before the code yields. In Python, a sync method waits, which blocks the event loop if it runs there.

An async `&mut self` method holds the resource from when the call starts until its result is ready. An `&self` call that waits for something only a `&mut self` call of the same resource can provide never finishes, because the `&mut self` call waits for it.

## When it ends

`close()` frees the Rust value, and later calls fail with an error that names the method, such as `Counter.add(): the resource is closed`. `closed` says whether that happened. Python's `with` and TypeScript's `using` call `close()` for you. A resource that's never closed is freed when the garbage collector collects it.

`close()` doesn't wait for running calls, which keep the value alive and finish, and Rust drops it after the last one. If no call is running, Rust drops the value inside `close()`, on the calling thread.

A method that takes `self` moves the value out and closes the resource. So does passing a resource to a parameter of type `T`. Either one fails if another call holds the value at that moment. A call that fails after it took a `T` argument, on a later bad argument for example, has still closed that resource.

## Bound to `Send` and `Sync`

With `[python]` on, a resource's type must be `Send` and `Sync`, because Python can call it from any thread. Bimetal reports a missing bound when it compiles the extension. Keep mutable state behind `&mut self` methods or in a `Mutex`, not in a `Cell`.

A field that is `Send` but not `Sync`, as many streams and writers are, goes in a `Mutex`: a `&mut self` method reaches it through `Mutex::get_mut`, which takes no lock. A value that isn't `Send`, such as one built on `Rc`, can't be a resource's state, so keep what rebuilds it instead. [Oxc's trial](../../apps/trials/oxc/src/lib.rs) does this: its syntax trees live in an arena, so its resource keeps the source text and parses again on each call.

## Resources that meet

Methods can return resources and take them as arguments. Passing one takes its lock for the length of the call, according to the parameter's type. A call that receives one resource twice, in two parameters or as its receiver and a parameter, fails instead of waiting for itself. Two calls that pass two resources to each other in opposite orders can wait for each other, as any two locks can.

## A callback that uses its own resource

A method can call a host function through a [callback](../guides/callbacks.md), and that function can call the same resource. If the inner call waited for a lock the outer call holds, neither would ever finish. So inside a callback, a synchronous call never waits, on any host: if its lock is taken, it fails with `the resource is busy`, and the outer call receives that as the callback's error.

Reading the resource from inside a `&self` method's callback works, since readers share the lock. Changing it doesn't, so finish the outer call first, or collect what you need and change it afterwards.

The same rule applies to any other resource that a callback uses. If another thread is using that resource at that moment, the call fails instead of waiting, as a synchronous call in JavaScript does. A thread that your function starts for itself isn't inside the callback, so a call there waits as usual, and if the function waits for that thread in turn, the two can deadlock.

[Resources](../guides/resources.md) shows the syntax, and [Resources and callbacks](../internals/resources.md) how it works.
