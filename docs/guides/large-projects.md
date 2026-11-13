# Wrapping a large project

Bimetal has been run on five large projects, with [DataFusion, Oxc, Polars, Tantivy and Typst](../../apps/trials/README.md) each behind one API for Python, Node.js and a browser. This page is what that taught us. Each wrapper is about 370 to 620 lines.

## Write the API first

Write the API a user of that project would want from Python, before you check what Bimetal allows. Then write the wrapper as a crate of its own, with `bimetal` and the project as dependencies. You won't export the project's types directly, because a `Model` must be a type of your crate, so mirror the ones you expose, such as `Diagnostic` and `Hit`.

Resources usually come in chains: a session makes a query, a query makes a stream. A method that returns a resource's type returns a new object of its class, so give a resource no constructor if only other exports should make it.

## Fitting `Send` and `Sync`

A resource must be both, and large projects often aren't.

**`Send` but not `Sync`** is common for streams and writers. A `Mutex` makes them fit, and `&mut self` reaches the value through `get_mut`, which takes no lock:

```rust
stream: Mutex<SendableRecordBatchStream>,
```

**Neither** usually means an arena or an `Rc`. Keep what rebuilds the value instead. Oxc's syntax trees live in an arena, so its resource keeps the source and parses again on each call.

## Making it compile for Wasm

Try `bimetal build`, and go through what it reports. The trials needed these:

| Problem | What it looks like | What to do |
| --- | --- | --- |
| A dependency's own switch | `uuid` fails to compile | Turn on its feature, such as `js`, under `[target.'cfg(target_arch = "wasm32")'.dependencies]` |
| A C library | `bimetal build` names symbols nothing defines | Turn off the feature that needs it, or define the symbols if they're never called, as [`nozstd.rs`](../../apps/trials/datafusion/src/nozstd.rs) does |
| Threads | A failure at run time | Take another path on this target, as [Tantivy's `BulkWriter`](../../apps/trials/tantivy/src/lib.rs) writes on the calling thread |
| Tokio tasks | A failure at run time | [Run them on a current-thread runtime](async.md#tokio) |
| `std::time` | A panic at run time | Use `web-time` where you control the code |
| Tokio features Tokio refuses on Wasm | `tokio` fails to compile | Find out who asks for them with `cargo tree -i tokio --target wasm32-unknown-unknown` |

A project that doesn't compile for this target as released may need patches. Polars does, and [`apps/trials/polars/prepare`](../../apps/trials/polars/prepare) downloads five of its crates and patches them, through a `[patch.crates-io]` table Bimetal respects. That leaves native builds as released, with a browser build that has one thread and no streaming engine, Parquet, IPC or cloud storage. A trial can have a `rust-toolchain.toml` of its own, as Polars needs Rust 1.95 or later.

If Wasm is more than it's worth, the [Node.js backend](node.md) needs none of this.

## Sharing Arrow

Choose the arrow-rs version the project uses, as [Arrow](arrow.md#choosing-a-version-of-arrow-rs) says. A project with its own Arrow implementation, such as Polars, needs a bridge through the C Data Interface, which [`bridge.rs`](../../apps/trials/polars/src/bridge.rs) shows. That bridge uses `transmute` between two versions of the same `repr(C)` structs, so read it with care.

## Very large results

JSON through `serde_json::Value` walks every field. For a large tree, also export the JSON as a `String`, and let the caller parse it. Oxc's `ast_text` does this.

## Build size and time

Each host compiles the project separately. Expect the first build of a large project to take several minutes, and the target directories to take gigabytes. Later builds reuse what they can. [`apps/trials/run`](../../apps/trials/run) shows how to test a wrapper on each host and in headless Chrome.
