# Testing

Bimetal's promises are about what happens on the far side of a boundary, so most of its tests build real packages and call them from real hosts.

## The commands

[Mise](https://mise.jdx.dev) installs the pinned Python, Node.js and uv, and sets `PYO3_PYTHON`, and `rust-toolchain.toml` pins Rust for rustup. Run these once:

```bash
mise trust
```

```bash
mise install
```

Without Mise, set `PYO3_PYTHON` to a Python interpreter that [the requirements](../reference/limits.md#requirements) allow, and run the commands of each task in `mise.toml`.

The examples commit their generated packages, so after you change the CLI, the macros or the runtime, regenerate them and run every host:

```bash
mise run example
```

For a change inside one crate, its own tests are quicker:

```bash
cargo test --locked -p bimetal-cli
```

Before you hand off, run all four:

| Command | Covers |
| --- | --- |
| `mise run lint` | rustfmt, and Clippy with warnings as errors, for combinations of the `python`, `node` and `arrow` features and for `wasm32`, then for each example |
| `mise run test` | Unit tests of every crate, then discovery, the Python boundary, which embeds CPython, and `fork`, with each version of Arrow, then the runtime's unit tests with the `node` feature |
| `mise run example` | Every example from end to end, as [the examples](#the-examples) describes |
| `mise run check-examples` | The same, but it verifies the committed outputs instead of rewriting them |

`check-examples` compares against what git holds, so stage or commit first. `mise run bench` measures build times, per-call time, peak memory and artifact sizes on this machine, and writes them to `target/bench/report.md`. CI doesn't run it.

## The layers

| Layer | Runs | Proves |
| --- | --- | --- |
| Unit tests | `cargo test` in each crate | The rules in isolation: naming, fingerprints, publication, validation |
| Macro diagnostics | `packages/macros/tests/diagnostics` | A bad signature gives a clear compile error, not a confusing one |
| The runtime's tests | `packages/runtime/tests/contract.rs`, `python.rs` and `fork.rs` | Discovery, and the Python boundary, embedding CPython and driving async calls with asyncio |
| Examples | `mise run example` | Every feature, called from Python, Wasm and the addon |
| The check | `mise run check-examples` | Each machine rebuilds the committed output, byte for byte |
| Benchmarks | `mise run bench` | Rough numbers, and copies against what each boundary should make |

`fork.rs` is alone in its binary because `fork` copies only the calling thread.

## The examples

`mise run example` builds each example, and takes its hosts and their projects from `bimetal check --json`, so a project may lie anywhere, as trading's do. For each example, it runs:

- the Python tests, then `mypy --strict` and strict pyright on the generated package;
- the TypeScript tests, which also type-check against the generated declarations;
- `apps/examples/check-packages.py`, which checks that the wheel and the npm tarball hold every file of their package, this platform's native artifacts among them, and installs each into a new project outside the repository, which imports it.

Then it runs the showcase's TypeScript suite again on the Node.js backend, from the copy `apps/examples/showcase_node.py` builds in `target/examples/showcase-node`, at the showcase's depth in the checkout. Every test file runs there unless `apps/examples/run` lists it as Wasm-only, so a new file can't go untested on the addon. Last, `bimetal new` scaffolds an application outside the repository, so that the repository's `rust-toolchain.toml` and `.gitignore` don't apply, builds it, and calls it from Python and from TypeScript.

The tutorial and the guides' samples are in those tests, in `test_tutorial.py`, `tutorial.test.ts`, `test_guides.py` and `guides.test.ts`.

A new feature goes in `apps/examples/showcase`, with tests on every host. A bug gets a test in the showcase, so that it stays found.

## The docs

`apps/cli/tests/docs.rs` holds the docs to what they describe: every Rust, Python and TypeScript sample in the README and in `docs/` has its lines, in order, in a program in `apps/examples/` or `apps/trials/`, every relative link goes somewhere, and the prose has none of the dashes the style rules out.

## CI

`.github/workflows/ci.yml` runs this matrix:

| Job | Lint | `mise run test` | Examples | Byte check |
| --- | --- | --- | --- | --- |
| Linux x86_64 (`ubuntu-latest`) | Yes | Yes | `check-examples` | Yes |
| Linux arm64 (`ubuntu-24.04-arm`) | | Yes | `check-examples` | Yes |
| macOS arm64 (`macos-latest`) | | Yes | `check-examples` | Yes |
| Windows x86_64 (`windows-latest`) | | Yes | `example`, on `main` only | Reported, on `main` only |

Pull requests run Linux x86_64 and macOS, and on Windows only the tests, while `main` runs every row in full. A final step loads greeter's Python package on Python 3.13, and on the oldest Node.js allowed, greeter's npm package, which uses the addon, and the showcase's, which uses the Wasm module. Every job starts with the setup steps in `.github/actions/setup`. No job of this workflow reads a secret, and its token can only read the repository, so a pull request from a fork has nothing to take. Each action is named by a commit, which Dependabot updates.

On Windows, mise runs a task through `cmd.exe`, so the `test` task has plain commands for it, and Git Bash runs `apps/examples/run`. On `main`, before it builds anything, it runs `bimetal check` on each example in the fresh clone, which fails the job if the committed fingerprints don't hold there, and then `bimetal check --rebuild`. That comparison doesn't stop the job, because the Wasm module can differ there, as [Reproducible output](reproducible.md#windows) says. Then it runs `example` rather than `check-examples`.

The Python boundary tests need the `python` feature, and the test binary must find libpython when it starts. `mise run test` adds an rpath to the interpreter's library directory, because macOS removes `DYLD_*` variables from child processes. Windows finds the interpreter's DLL on `PATH` instead.

## Which test guards what

| Promise | Test |
| --- | --- |
| A native call doesn't hold the interpreter lock | `functions_run_without_the_interpreter_lock` |
| Rust checks field rules on arguments and on returns | `rust_checks_the_field_rules_of_arguments`, `rust_checks_the_field_rules_of_returns` |
| Bad arguments name the function and the parameter | `argument_errors_name_the_callable_and_the_parameter` |
| A buffer crosses only in native byte order | `buffers_cross_only_in_the_native_byte_order` |
| An interrupt while reading arguments isn't lost | `interrupts_while_reading_arguments_propagate` |
| The extension records its contract's hash | `the_module_records_the_hash_of_its_contract` |
| An async call keeps its value from the call until it finishes | `async_calls_hold_the_value_from_the_call_until_they_finish` |
| An async call takes its lock when it's made | `async_calls_take_the_lock_when_they_are_made` |
| Taking a value fails while a call holds it | `taking_a_value_closes_the_resource_unless_a_call_holds_it` |
| Arrow crosses through the PyCapsule interface | `arrow_values_cross_through_the_pycapsule_interface` |
| A stream crosses one batch at a time both ways, and no pull waits holding the interpreter lock | `streams_cross_one_batch_at_a_time` |
| pyarrow, DuckDB and Polars read a returned stream and pass one, which stops when they stop | `apps/examples/streams/python/tests/test_streams.py` |
| A stream crosses lazily to TypeScript on both backends, and a `break` closes its source | the `arrow streams` suite of `apps/examples/showcase/typescript/tests/showcase.test.ts` |
| A callback can't deadlock on the resource whose call it's inside | `test_a_function_cannot_change_the_resource_it_was_called_from`, and its neighbours in `test_shelf.py` and `shelf.test.ts` |
| A panic raises `PanicException` with its message | `panics_raise_the_panic_exception_with_their_message` |
| Async calls work in a forked child, and the parent's streams fail there | `async_calls_run_in_a_forked_child` |
| A sync constructor can spawn tasks | `sync_constructors_can_spawn_tasks` |
| A build keeps your files and removes stale generated ones | `reconciliation_preserves_user_files_and_removes_stale_generated_files` |
| A build refuses to overwrite your files, before it changes anything | `reconciliation_rejects_user_owned_collisions_before_mutating` |
| A moved project leaves nothing behind and keeps your new files | `a_moved_project_leaves_no_old_files_and_keeps_the_users_new_ones` |
| An interrupted publication is reported, then finished | `an_interrupted_publication_is_reported_and_then_finished` |
| A panic ends the Wasm module, and later calls say so | `apps/examples/showcase/typescript/tests/panic.test.ts`, `async-panic.test.ts` |
| A trap without a panic ends the module the same way | `apps/examples/showcase/typescript/tests/trap.test.ts` |
| A trap inside a host callback is still a trap | `apps/examples/showcase/typescript/tests/callback-trap.test.ts` |
| A package works in a browser, through a loader, and through Vite | `browser.test.ts`, `vite.test.ts` |
| Each host holds up under load | `apps/examples/trading` |

## What's not tested

This list is as of 10 November 2026, and should shrink.

- **Windows and Linux arm64, for the newest features.** Resources across calls, callbacks, protocols, error details and Arrow streams have run on macOS arm64 and Linux x86_64 only, and the five trials on macOS arm64 only, with Python 3.11 and Node.js 24. The matrix above covers the other two platforms from its first run on the public repository.
- **The addon's thread-safe function during shutdown.** A review found a use-after-free window, and the fix has no regression test. It was checked by reading.
- **The Polars bridge.** It uses `transmute` between two versions of `repr(C)` structs. Tests exercise it, and nobody has reviewed it independently.
- **Memory.** No test runs under a sanitizer or Miri, or checks for leaks across long runs.
- **Bundlers other than Vite, and browsers other than headless Chrome.**
- **Publishing.** Wheels and tarballs are built and installed locally. None has gone to PyPI or npm.
