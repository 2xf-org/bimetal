# Trials

Five large Rust projects behind Bimetal, each with one API for Python, Node.js, and a
browser. They test whether Bimetal copes with real code rather than code written for
it, and they are too large for CI.

| Trial | Project | Rust in the project | Crates | Package |
| --- | --- | --- | --- | --- |
| [`datafusion`](datafusion) | Apache DataFusion 55, a SQL query engine | 575,000 lines | 260 | `query` |
| [`oxc`](oxc) | Oxc 0.120, a JavaScript toolchain | 269,000 lines | 163 | `jstools` |
| [`polars`](polars) | Polars 0.55, a dataframe library | 424,000 lines | 375 | `frames` |
| [`tantivy`](tantivy) | Tantivy 0.27, a search engine | 155,000 lines | 161 | `search` |
| [`typst`](typst) | Typst 0.15, a typesetting system | 134,000 lines | 332 | `typeset` |

Each API is what a user of that project would want from Python, written before
checking what Bimetal allowed. Where Bimetal could not express it, Bimetal changed. The
wrappers are 370 to 622 lines each.

## Running them

```bash
apps/trials/run
```

```bash
apps/trials/run --node
```

The first builds each trial and runs its Python tests, its Node.js tests on the Wasm
module, and its page in headless Chrome, through [`browser.py`](browser.py). The second builds a copy on the Node.js addon
and runs its tests there. Name trials to run only those: `apps/trials/run oxc`.

## Results

Each number is tests passed, with none failing. The browser column is one page per
trial in headless Chrome, which runs the same calls as the tests and reports back.

| Trial | Python | Node.js, Wasm | Node.js, addon | Browser | Python extension | Wasm module |
| --- | --- | --- | --- | --- | --- | --- |
| `datafusion` | 10 | 7 | 7 | Passes | 122 MB | 29 MB |
| `oxc` | 16 | 12 | 12 | Passes | 6 MB | 3 MB |
| `polars` | 33 | 15 | 15 | Passes | 126 MB | 24 MB |
| `tantivy` | 14 | 5 | 8 | Passes | 8 MB | 4 MB |
| `typst` | 10 | 8 | 8 | Passes | 53 MB | 33 MB |

A first build takes 2 to 9 minutes on 6 cores. Typst's size is mostly the fonts that it
embeds. These ran on macOS arm64 only, with Python 3.11 and Node.js 24. One Polars test
passes a Python `polars.DataFrame` both ways, and skips unless that package is installed:
run it with `uv run --with polars python -m unittest discover -s tests`.

Grouping a million rows with Polars took about 22 ms in the browser, on one thread, and
about 4 ms in Python, on every core. Both are single runs on one machine.

## What the projects needed

These gaps showed up as compile errors or failing tests, and Bimetal now covers them.

| Need | Who needed it | What Bimetal has now |
| --- | --- | --- |
| A method that returns another resource | All four | A return of the resource's type, also in `Option` and `Vec` |
| A resource with no public constructor | All four | Constructors are optional |
| Structured error data | All four | `#[bimetal(details)]`, a typed model on the exception |
| `for` and `async for` | All four | `#[bimetal(next)]`, sync or async |
| Properties, `len()`, `repr()`, `in` | All four | `getter`, `setter`, `len`, `repr`, and the other protocols |
| Optional arguments | All four | `#[bimetal(default = ...)]`, and trailing `Option`s |
| Commit on success, roll back on exception | Tantivy | `#[bimetal(exit)]`, told whether the block failed |
| A method that consumes its value | Tantivy | Methods can take `self` |
| Host functions that Rust calls | DataFusion, Oxc | `bimetal::Callback` |
| A resource as an argument | DataFusion | A parameter of the resource's type: `&T`, `&mut T`, `Option<&T>`, or `T` |
| `async for` over Arrow batches | DataFusion | `Option` of a direct value in a return |
| `getrandom` on Wasm, in all three versions in use | DataFusion, Tantivy, Polars | The Wasm build configures it |
| The time on Wasm | DataFusion | The runtime turns on chrono's `wasmbind` |
| A clear failure for a C library on Wasm | DataFusion | `bimetal build` names the symbols that nothing defines |
| A clear failure for `#[cfg]` on an export | Tantivy | The macro says to put it inside the body |
| Arrow values from an older arrow-rs | DataFusion | The `arrow-59` feature |

Bimetal also gained `usize` and `isize`, as 64-bit integers. The trials convert their
sizes to `u32` and `u64`, so only the showcase exercises them.

## What a project must still do itself

These are limits of the platforms or of Bimetal's design, with what each trial did.

- **A dependency's own Wasm switch.** Tantivy needs `uuid`'s `js` feature.
- **C libraries on Wasm.** DataFusion links zstd through arrow-ipc, whose default
  features it turns on. [`nozstd.rs`](datafusion/src/nozstd.rs) defines the symbols,
  since queries in memory never call them.
- **Threads on Wasm.** Tantivy's `IndexWriter` needs them, so
  [`BulkWriter`](tantivy/src/lib.rs) writes on the calling thread, and works everywhere.
- **Tokio tasks on Wasm.** DataFusion spawns them, so its calls
  [run on a current-thread runtime](datafusion/src/lib.rs) there.
- **Choosing a version of Arrow.** DataFusion 55 uses arrow-rs 59, so this trial
  enables Bimetal's `arrow-59` feature, which makes their types the same.
- **`Send` but not `Sync`.** DataFusion's streams and Tantivy's `BulkWriter` are. A
  `Mutex` makes them fit, and `get_mut` takes no lock.
- **Neither `Send` nor `Sync`.** Oxc's syntax trees live in an arena, so a resource
  keeps the source, and each call parses it again.
- **An Arrow implementation other than arrow-rs.** Polars has its own, so no feature can
  make the types the same. [`bridge.rs`](polars/src/bridge.rs) converts through the C
  Data Interface without a copy, using `transmute` between the two versions of its structs.
- **Polars in a browser.** Polars does not compile for this target as released.
  [`prepare`](polars/prepare) downloads five of its crates and patches them for
  `wasm32-unknown-unknown`, leaving native builds as released: Tokio features that Tokio
  refuses there, a function that lacks its Wasm check, and features that force on the
  streaming engine, which needs threads. So the browser build runs on one thread, and has
  no streaming engine, Parquet, IPC, or cloud storage. CSV, JSON, Arrow, SQL, lazy
  queries, and joins work. This trial also needs Rust 1.95 or later, so it has its own
  `rust-toolchain.toml`. A version manager that sets `RUSTUP_TOOLCHAIN` overrides that
  file, and `apps/trials/run` clears it.
- **Foreign types.** A `Model` must be a type of your crate, so each trial mirrors the
  project's types it exposes, such as `Diagnostic` and `Hit`.

## What Bimetal does not do yet

Nothing in the trials needed these, and each needs design:

- `bytes` and buffers inside a model; a `Vec<u8>` field is a list of integers.
- Enums other than unit-only and internally tagged, `#[serde(flatten)]`, generic types,
  `u128`, and maps with keys other than strings.
- Async host functions, `Option<Callback<A, R>>`, keyword-only arguments, and `*args`.
- Operators other than `==`, such as `<` and `+`, and pickling a resource.
- More than one `#[bimetal::export]` block for a resource, which fails to compile.
- One method implementing two protocols, so Polars' `repr` and `display` are two methods.
