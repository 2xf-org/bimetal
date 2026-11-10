# Failures

A call can fail in four ways, and each one reaches the host differently. The first is part of your API. The others are for finding bugs.

| Failure | Python raises | TypeScript throws |
| --- | --- | --- |
| A typed error you return | Your error class, a `RuntimeError` | Your error class, an `Error` |
| An argument of the wrong type | `TypeError` | `TypeError` |
| An argument that breaks a field rule or won't fit | `ValueError` | `RangeError` for a field rule, `TypeError` otherwise |
| A panic | `PanicException` | `Error`, and on Wasm the module ends |

## Typed errors

An error you return has a stable `code`, which is what callers branch on. The message is for logs, and it can change. [Errors](../guides/errors.md) shows how to declare one.

## Bad arguments

Bad arguments name the function, the parameter and the problem, in each language's own style:

```text
greet(): argument `name` must be a string, got number
repeat() argument 'str_': 'int' object is not an instance of 'str'
countTags(): argument `records`: `[1].stars` must be at most 5, got 9
```

TypeScript checks the outermost type of each argument before the call enters Rust, and Rust reports what that can't see, such as a field of the wrong type inside a model, or an integer out of range. A `Map` or a `Set` where a plain object is expected is a `TypeError`, rather than crossing as an empty object. On the addon, an argument nested more than 128 levels deep is a `TypeError` too, so that reading it can't overflow the native stack.

Python's `TypeError` covers a value of the wrong type. A value of the right type that Rust can't take, such as `-1` for a `u32` or 3 bytes for a `[u8; 4]`, is a `ValueError`. Pydantic also checks the rules where a model is built, so `Record(label="")` raises its `ValidationError` before any call.

A returned value that breaks a field rule is a bug in your Rust, so it's a `RuntimeError` in Python and an `Error` in TypeScript. A constant that breaks one fails `bimetal build`.

## Panics

A panic is a bug, and no host lets it pass as a typed error.

**Python** raises `PanicException` with the panic's message, from synchronous and async calls alike. The package root exports it. It derives from `BaseException`, so `except Exception` doesn't catch it. The extension stays usable, although a later call can reach a resource whose `&mut self` method panicked partway, because Tokio's lock doesn't record a panic.

**The Node.js addon** throws, or rejects with, an `Error` whose message is `Rust panicked:` and the panic's message. The addon stays usable, and other calls keep running.

**WebAssembly** can't recover: a panic aborts the module partway through the call, and its memory can no longer be trusted. So:

- The call throws, or its promise rejects, with an `Error` whose message starts with `Rust panicked:` and whose `cause` is the `WebAssembly.RuntimeError` of the trap.
- That `Error` also rejects every other pending async call, whose Rust future can't finish.
- From then on, every call fails at once with an `Error` that starts with `An earlier Rust panic aborted the WebAssembly module` and repeats the panic's message, without entering the module.
- `close()` does nothing, so a `using` block reports the panic itself, and garbage collection no longer frees Rust values.

The process keeps running. Reload the page or restart the process to call the module again.

Bimetal builds the extension and the addon to unwind even if your Cargo profiles set `panic = "abort"`, so a panic reaches the host as above. Panic messages name your files relative to the workspace, such as `src/lib.rs:12:5`, because compiled packages contain no path from the machine that built them.

## Traps and stack overflows

A trap without a panic aborts the Wasm module the same way: running out of memory, or a stack overflow, which JavaScript reports as a `RangeError`. Its `Error` starts with `The WebAssembly module trapped:` and names the trap, and later calls start with `An earlier WebAssembly trap aborted the WebAssembly module`.

On Python and the addon, a Rust stack overflow ends the process, as in any native code.

## Callbacks

A function you pass to Rust can raise. Rust sees that as `Err(CallbackError)` with the exception's text, and maps it into its own error type. The original exception doesn't reach the caller. [Callbacks](../guides/callbacks.md) shows how.
