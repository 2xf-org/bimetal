# Limits and requirements

## Requirements

| Tool | Version | Notes |
| --- | --- | --- |
| Rust | 1.94 or newer, with the `wasm32-unknown-unknown` target | On Windows, an MSVC toolchain such as `stable-x86_64-pc-windows-msvc`. Bimetal refuses the GNU ones |
| Python | 3.11 or newer | Set `PYO3_PYTHON` to the interpreter pyo3 builds against, if `python3` on your `PATH` isn't the one you want |
| Node.js | 20.16 or newer | With the Node.js backend, it also loads in Electron with Node-API 8 or later |
| TypeScript | 5.2 or newer | For `using` and `Symbol.dispose`. With Arrow, 5.7 or newer, unless `skipLibCheck` is on |
| pyarrow | 16 or newer | With Arrow |
| apache-arrow | 21 | With Arrow |

## Versions

The `bimetal` command and the `bimetal` runtime crate must have the same version, and Bimetal stops with an error if they differ. A version is the date of its release, such as `202611.10.0` for the first release on 10 November 2026. `bimetal new` writes the dependency as `bimetal = "=202611.10.0"`, the version of the command, so `cargo update` never moves it. To upgrade, install the new command and change that line.

The application's `bimetal` dependency can come from crates.io, from a path, or from a git repository (`branch`, `tag`, `rev`, or the default branch). Other registries aren't supported.

## Limits

| What | Value |
| --- | --- |
| Tuple size | 2 to 8 items |
| Nesting of an argument on the Node.js addon | 128 levels |
| Arrow-rs versions | 60, or 59 with `arrow-59` |
| Callback arguments | A tuple of contract types |
| Exported `impl` blocks per resource | 1 |
| Protocols per method | 1 |
| Python ABI | CPython's stable ABI from 3.11 |
| Node-API | 8 |

## What Bimetal doesn't do yet

Nothing has needed these, and each needs design:

- `bytes` and buffers inside a model. A `Vec<u8>` field is a list of integers.
- Untagged and adjacently tagged enums, tuple variants that hold other than one value, `#[serde(flatten)]`, generic types, `u128`, and maps with keys other than strings.
- A JSON Schema of an externally tagged enum's wire form. Pydantic's schema of its models describes their Python fields.
- Async host functions, `Option<Callback<A, R>>`, keyword-only arguments and `*args`.
- Operators other than `==`, such as `<` and `+`, and pickling a resource.
- More than one `#[bimetal::export]` block for a resource, which fails to compile.
- One method implementing two protocols.
- Inferring `returns(resource)`, which a resource-heavy API repeats on most methods.

## What it can't do

These come from the hosts:

- A panic or trap ends a Wasm module until it's loaded again.
- JavaScript can't cancel a promise, so an async call runs to its end.
- JavaScript doesn't tell `using` whether its block threw.
- A browser can't load a native addon.
- On the Node.js addon, a blocking wait inside an async export on a stream that JavaScript writes must go through Bimetal's stream reads, such as iterating the `RecordBatchStream`. Another executor's `block_on` there, before the call's first wait, stops the JavaScript thread.
- A `#[cfg]` that tests the target can't gate an exported method, because the hosts share one contract but differ by target. A feature can.
- Python and JavaScript type checkers can't read types from a binary, so the generated files stay.
