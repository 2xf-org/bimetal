# The life of a build

This page goes through `bimetal build` in order, in `apps/cli/src/project/`. [`pipeline.rs`](../../apps/cli/src/project/pipeline.rs) runs the steps.

## 1. Read the project

Bimetal reads `bimetal.toml` and `cargo metadata`, and selects the package beside `bimetal.toml` and the `additional_packages`. Each must be a library that the link crate can link, and must depend directly on the same `bimetal` package, at the version of the CLI, from a path or a git source. It resolves the package and project of each host a table turns on, asks the toolchain for its version, and refuses a `[generated]` record whose project no host could have, or whose files leave it.

## 2. Compare fingerprints

If the source and output fingerprints match `[generated]`, Bimetal checks the enabled hosts' project manifests against the dependencies `[generated]` records. `build` then also compares the local inputs and the installed native artifacts with the record in `.bimetal/native.json`, and stops when they match too. [Reproducible output](reproducible.md#which-inputs-go-where) says which inputs each fingerprint covers.

## 3. Write the link crate

Bimetal writes a `cdylib` crate in `target/bimetal/link-<workspace>/<key>`, where `<workspace>` hashes the workspace's location, so workspaces sharing a target directory keep apart, and `<key>` hashes the location of `bimetal.toml` in its workspace. The crate depends on `bimetal`, with the same source as the application, and on each selected package, and calls `bimetal::application!(...)`.

The link crate is its own workspace, with the application workspace's `[patch]`, `[replace]`, `[profile]` and `resolver`. Bimetal copies the application's `Cargo.lock` into it, so it builds with exactly the dependency versions the application tests. That lockfile also pins crates only the extension builds, such as pyo3: the runtime lists them a second time under `[target.'cfg(any())'.dependencies]`, a target that never holds, and Cargo locks the dependencies of every target but builds them for none.

## 4. Compile the targets

Bimetal compiles the targets at once, each with `cargo rustc --locked` and an explicit `--target`, so a configured `build.target` changes none of them. Each build runs only when an enabled host needs it:

| Build | Runs for | Profile | Output |
| --- | --- | --- | --- |
| Discovery | Every application | `bimetal-discovery`: release, but with no optimization, incremental, and no debug info | A native library the CLI loads to read the contract |
| Wasm | `[typescript]` with the Wasm backend | Release with `opt-level = "s"`, LTO and one codegen unit, for `wasm32-unknown-unknown`, in its own target directory | The Wasm module |
| Python | `[python]` | Release, `bimetal/python`, with `PYO3_BUILD_EXTENSION_MODULE=1` | The extension module |
| Native | `[typescript]` with the Node.js backend | `bimetal-native`: release, with `bimetal/node` | The Node.js addon |

`check` runs only discovery and Wasm, and `build` starts the Python and native builds beside them. Discovery inherits the release profile so it sees the extension's `cfg(debug_assertions)`.

Every native build unwinds on a panic whatever the application's profiles say, so each host can report it, and a panic during discovery can't abort the CLI. Each build caps its Cargo jobs at its share of the machine's cores, unless `CARGO_BUILD_JOBS` or `build.jobs` sets them, so the builds together start no more jobs than the machine can run. These builds use different profiles, and Wasm its own target directory, so none waits for another's Cargo lock.

A build that waits for the lock of a build directory another Cargo command holds says so as it starts waiting. Each build shows the warnings no other build showed.

## 5. Discover the contract

The CLI loads the discovery library with `libloading` and calls `bimetal_discovery_v1_contract__<crate>`. The function returns the contract as JSON, or a validation error. The discovery ABI never unwinds, and the CLI frees each payload with the paired `..._free` function.

## 6. Validate

The CLI checks the contract for host-name collisions, reserved names and unsupported shapes, and it checks the Python and npm project manifests. It reports every problem in one error, a validation error from discovery included.

The extension is built for the same platform as discovery, with the same configuration, so its exports are the contract's. The Wasm module is built for `wasm32`, so after its build `project::exports` compares the module's export section with the contract, and lists unresolved `env` imports.

## 7. Render

The generator of each enabled host writes into `<stage>/<host>/`, a staging directory in `.bimetal/`, as its files go in the host's project.

Python gets modules with Pydantic models, a private `_runtime.py`, and a `_native.pyi` stub for the extension. The extension carries a hash of its contract in `__bimetal_contract__`, and `_runtime.py` compares it with the hash it was generated from at import. Both sides hash the discovery JSON with `bimetal_ir::discovery::contract_hash`, and both take Python parameter names from `bimetal_ir::python_names`. NumPy is imported only by a package whose contract passes buffers.

TypeScript gets a `.js` file and a `.d.ts` file for each module, a private `runtime` whose `nativeReady` the root `index` re-exports, and, when the contract passes Arrow values, `arrow.js`, the one module that imports apache-arrow. The runtime starts with its backend's loader. For Wasm, `loader-wasm.js` loads what `wasm-bindgen-cli-support` makes of the module: `native/native.js`, `native.d.ts` and `native_bg.wasm`. For Node.js, `loader-node.js` loads `native/native.node` and checks its contract hash.

## 8. Publish

Bimetal maps each staged `<host>/<file>` to `<project>/<file>`, checks every destination, copies each staged file whose bytes changed into place through a temporary file beside it, deletes the stale files that are its own, and records the result in `[generated]` last. Previous files are where their records put them, so a disabled host's files, and those of a project that moved, are stale. A build that stops midway leaves only Bimetal's files, which the next build adopts. [Generated files](../reference/generated.md) has the rules.

## 9. Install the native artifacts

`build` and `build-native` copy the libraries of the Python and native builds into the hosts' projects, and record each one's size, modification time and content fingerprint in `.bimetal/native.json`, beside the source fingerprint and the local inputs. A later build hashes an artifact again only when its size or time changed. An artifact is copied beside its destination and renamed into place only when its bytes changed.

No artifact goes through the staging tree or `[generated]`. `output::native_file` names each platform's artifact from the file that implies it, the Python stub or the TypeScript runtime, so publication deletes every platform's artifacts with a stale one.

## Locks

Bimetal holds `.bimetal/build.lock` during a command, so two commands can't build the same application at the same time, and a command that has to wait says so. Before it publishes, Bimetal reads `bimetal.toml` and the source fingerprint again, and stops if either changed during the build. Once it holds the lock, a command deletes the staging directories an interrupted one left in `.bimetal/`, where native artifacts are also copied before they replace the installed ones.

## Why the contract comes from a native build

Exports register through `inventory` only in native builds, which is why the contract comes from a native build. The Wasm module registers nothing, and `bimetal_initialize` runs the constructors that dependencies declare once, at instantiation.
