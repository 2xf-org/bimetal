# Hosts

A *host* is a language your Rust runs inside. Bimetal has two host languages and three ways to run Rust in them.

| | Python extension | Wasm module | Node.js addon |
| --- | --- | --- | --- |
| Turned on by | `[python]` | `[typescript]`, by default | `[typescript]` with `backend = "node"` |
| The compiled file | `native.abi3.so`, or `.pyd` on Windows | `native/native_bg.wasm` | `native/native.node` |
| Runs in | CPython 3.11 and newer | Node.js, browsers, bundlers | Node.js and Electron |
| Built on | PyO3 | wasm-bindgen | Node-API, through `napi-sys` |
| One file per | Platform | Nothing: it serves every platform | Platform |
| Async calls run on | The caller's thread to the first wait, then a Tokio runtime that the extension shares | The JavaScript event loop | The JavaScript thread to the first wait, then a Tokio runtime that the addon shares |
| Threads | Yes, without the interpreter lock | No | Yes |
| A panic | Raises `PanicException`, and the extension stays usable | Ends the module | Throws an `Error`, and the addon stays usable |

## Choosing a TypeScript backend

Use the default, Wasm, unless you can't. One package then works everywhere, and a clone needs no compiler.

Use `backend = "node"` when your Rust doesn't compile to `wasm32-unknown-unknown`, as with code that runs Tokio's multi-threaded runtime, or when you need threads. The trade-off is that browsers can't load it, and you need one build per platform. [`apps/examples/bank`](../../apps/examples/bank) takes this path. The calls, values and errors are the same on both backends. Only panics and stack overflows behave differently.

You can't have both from one `bimetal.toml`. An application that wants both can keep a second `bimetal.toml`, as [`showcase_node.py`](../../apps/examples/showcase_node.py) does for a copy of the showcase.

## What differs between hosts

Bimetal keeps one behaviour across hosts wherever the host allows it. These differences come from the hosts themselves:

- **Blocking.** Python can wait for a lock with the interpreter lock released. JavaScript can't block its one thread, so a synchronous method on a busy resource throws instead of waiting. [Resources](resources.md) explains.
- **Keyword arguments.** Python has them, and TypeScript skips an argument with an explicit `undefined` or `null`.
- **Knowing a block failed.** Python's `with` tells `exit` whether the block raised. JavaScript's `using` doesn't, so TypeScript always reports that it failed, and code commits explicitly.
- **Cancellation.** Cancelling a Python task drops the Rust future. A JavaScript promise can't be cancelled, so the call runs to its end.
- **Memory shared with Rust.** Python can share Arrow buffers with Rust. JavaScript can't keep a view into Wasm memory, because growing the memory detaches the views, so values are copied. [How values cross](crossing.md) gives the counts.
- **Integers.** JavaScript numbers can't hold every 64-bit integer, so `u64`, `i64`, `usize` and `isize` are `bigint` there.

## What runs where

Code in the Wasm module has one thread and no operating system. Tokio's I/O, timers and `tokio::spawn` don't exist there, and neither does `std::time`, so a dependency that calls them fails or panics. [Browsers](../guides/browser.md) lists what Bimetal handles for you, and [Wrapping a large project](../guides/large-projects.md) what you handle yourself.
