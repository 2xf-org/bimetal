# The crates

| Crate | Folder | Role | Links |
| --- | --- | --- | --- |
| `bimetal` | `packages/runtime` | The runtime that applications depend on. It re-exports the macros, collects exports with `inventory` in native builds, converts values at the Python, Node.js and Wasm boundaries (`bimetal::convert`), holds resource state and runs async calls (`bimetal::runtime`), and builds the contract for discovery. With the `arrow` feature, `bimetal::arrow` adds Arrow values | `pyo3` and `pythonize` only with the `python` feature, `napi-sys` only with the `node` feature, `wasm-bindgen` and `wasm-bindgen-futures` only on `wasm32`, and the arrow-rs crates only with `arrow` or `arrow-59` |
| `bimetal-macros` | `packages/macros` | `Model`, `Error`, `export` and `application`. They emit registry entries, which only native builds compile, the Python wrappers, the Node-API wrappers and the wasm-bindgen wrappers | `bimetal-ir`, for the naming rules and the hash |
| `bimetal-ir` | `packages/ir` | The serialized contract (`Manifest`), the discovery ABI, the naming rules the hosts share, and the FNV-1a hash | `serde` and `serde_json` |
| `bimetal-cli` | `apps/cli` | The `bimetal` command | `bimetal-ir`, never `bimetal` |

`packages/` holds the crates that others depend on, and `apps/` holds what uses them: the CLI, the examples, the trials and the benchmark.

## Two boundaries

Two boundaries keep libpython out of the wrong binaries:

- The `python` feature of `bimetal` is off by default, so applications and their test binaries never link libpython. The CLI turns the feature on only for the Python extension build.
- The CLI depends on `bimetal-ir`, not on `bimetal`. It reads the contract through a C ABI at run time, so it never links pyo3.

Macro output that only the Python extension needs goes through `bimetal::__python_items!`. That macro expands its input only when `bimetal` has the `python` feature, so applications declare no features for it and see no `cfg`. `bimetal::__node_items!` does the same for the addon with the `node` feature. The `arrow` feature is the one applications enable themselves, and Cargo's feature unification carries it into the link crate.

## Inside `bimetal-cli`

| Module | Role |
| --- | --- |
| `cli`, `commands` | Arguments, and printing results as text or JSON |
| `scaffold` | `bimetal new` |
| `config` | Reading `bimetal.toml`, and rewriting only `[generated]`, after every user table |
| `cargo` | Cargo and rustc. `metadata` types `cargo metadata`, `config_files` reads the `.cargo/config.toml` files a build sees, `toolchain` asks rustc for its host, sysroot and commit, `remap` gives local directories portable names, `rustc_wrapper` adds them to each compilation, `messages` reads what a build reports, and `dep_info` what it read |
| `literal`, `paths` | Quoting strings for TOML, Python and JavaScript, the identifier grammar they share, and writing paths the way `bimetal.toml` records them |
| `problems` | Reporting every problem of the Rust API, the application or a user-owned file in one error |
| `project` | `snapshot` resolves a `Project` from `bimetal.toml` and Cargo. `pipeline` runs the build and `check` steps, and `reports` holds what they return. `packages` selects packages and the `bimetal` source. `checkout` computes the source fingerprint, and splits what the builds read into the checkout's files and this machine's. `link` writes the link crate, `compile` compiles its targets, `discovery` reads the contract, `exports` checks that the Wasm module exports what the contract describes, and `validation` checks names for each enabled host, and `pyproject.toml` and `package.json` |
| `contract`, `dependencies`, `documentation` | What the generators share: an index over the contract, one walk over its type references, the schemas both runtimes read, the host packages the generated code imports, and the sections of the Rustdoc and the wrapping of its comments |
| `python`, `typescript` | The generators. In each, `package` emits the files, `api` and `models` render the modules, and `render` holds the shared type and documentation rendering. A module renders its body first, spelling every name it imports through its `context`, which then writes exactly those imports above it. The runtimes are plain files beside the code: `python/runtime.py`, and `typescript/runtime.js`, the loaders with their declarations, and `arrow.js`. The generator appends only the contract's schemas and, for Python and the Node.js backend, the contract hash the runtime checks the native library against |
| `output` | `scratch` holds the `.bimetal/` directory and its build lock, and installs a file beside its destination. `publish` installs the staged files and deletes stale ones. `artifacts` names every platform's native artifacts. `fingerprint` decides when nothing needs to be built, and holds the hashing that `project::checkout` shares. `native` records the installed native artifacts and the local inputs |

`build.rs` hashes the generator into `BIMETAL_GENERATOR_FINGERPRINT`: the CLI's sources, the runtimes among them, and its version, which decides the crates it renders with, such as `wasm-bindgen-cli-support`, because the manifests pin them exactly. Applications include it in their source fingerprint, so a generator change rebuilds them even when the version stays the same. A build from a checkout and a build of the crate from crates.io hash the same files, so every copy of one release agrees on the fingerprint.

## Pins that move together

`wasm-bindgen`, `wasm-bindgen-cli-support` and `wasm-bindgen-futures` are pinned to exact, matching versions in the workspace `Cargo.toml`. The runtime links the first and third, and the CLI post-processes the Wasm module with the second. Each `wasm-bindgen-futures` release pins one `wasm-bindgen` release.

`pyo3` and `pythonize` change together, since each `pythonize` release follows one `pyo3` minor version.

The arrow-rs crates share one version per feature, and applications reach them through `bimetal::arrow`.

## The examples

The examples are separate Cargo workspaces that depend on Bimetal by path, as an application would. `apps/examples/showcase` uses every feature, so add to it when Bimetal gains one. `apps/examples/bank` is the README's account, and a bank on Tokio's multi-threaded runtime that can't build for Wasm. Its tests load each host under concurrency: a thousand transfers at once, threads, worker threads, cancellation, `fork`, and closing the bank while calls run, and they run the programs that the README and the tutorial quote. Commit the regenerated examples with each change to Bimetal.
