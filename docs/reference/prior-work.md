# Prior work

Bimetal stands on a lot of other people's work. These are what it uses and borrows, and what it does with them.

| Work | The idea | In Bimetal |
| --- | --- | --- |
| [PyO3](https://github.com/PyO3/pyo3) | Rust types as native Python objects, with the interpreter lock in the type system | The Python extension. The macros emit PyO3 wrappers |
| [maturin](https://github.com/PyO3/maturin) | One command that builds a Rust crate into a wheel | The shape of `bimetal build`. Bimetal builds and tags its own wheels through `pdm_build.py` |
| [pythonize](https://github.com/davidhewitt/pythonize) | Serde to Python objects, without JSON | How models cross in Python |
| [wasm-bindgen](https://github.com/wasm-bindgen/wasm-bindgen) | High-level calls between Wasm and JavaScript | The Wasm wrappers, and `wasm-bindgen-cli-support` generates the module's glue |
| [serde-wasm-bindgen](https://github.com/RReverser/serde-wasm-bindgen) | Serde to JavaScript values, without JSON | How models cross in Wasm. The addon's deserializer follows its checks and messages |
| [Node-API](https://nodejs.org/api/n-api.html), through [napi-sys](https://github.com/napi-rs/napi-rs) | A stable ABI for Node.js addons | The addon, written on the raw API, behind safe handles |
| [napi-rs](https://github.com/napi-rs/napi-rs) | Generated `.d.ts` files, and a loader for per-platform addons | The idea of committing generated JavaScript beside a native addon |
| [Serde](https://serde.rs) | One data model, with attributes for wire names | The wire format, and why Bimetal needs no attributes of its own for most models |
| [inventory](https://github.com/dtolnay/inventory) | Items that register themselves at startup through linker constructors | How macros in many crates add to one contract in native builds |
| [Pydantic](https://docs.pydantic.dev) | Validated models with types | What a Python model is |
| [Arrow C Data Interface](https://arrow.apache.org/docs/format/CDataInterface.html) and [PyCapsule interface](https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html) | Sharing columns between libraries without a copy | Arrow values in Python, and the bridge to Polars |
| [Arrow IPC](https://arrow.apache.org/docs/format/Columnar.html#serialization-and-interprocess-communication-ipc) | A stream format for columns | Arrow values in TypeScript, where nothing can share memory |
| [Tokio](https://tokio.rs) | A runtime for async Rust | What runs async calls on Python and the addon |
| [Reproducible Builds](https://reproducible-builds.org) | The same source gives the same bytes | `bimetal check --rebuild`, and the path remaps it depends on |
| [UniFFI](https://github.com/mozilla/uniffi-rs) and [Diplomat](https://github.com/rust-diplomat/diplomat) | One Rust definition, many languages | The closest tools. Bimetal covers fewer languages and more of each |
