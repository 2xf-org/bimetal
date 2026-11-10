# Keeping packages current

Run `bimetal build` after each change to the contract, and commit the generated files, `bimetal.toml` and `Cargo.lock` with the Rust change. `Cargo.lock` pins every crate that the packages contain, those of the Python extension included.

## In CI

```bash
bimetal check
```

`check` fails if a generated file is missing, changed or stale, and lists each one. It never writes anything. Run it with the Rust toolchain that built the committed files, which a `rust-toolchain.toml` can pin, because the fingerprint covers the toolchain and another one builds another Wasm module.

```bash
bimetal check --rebuild
```

`--rebuild` also compiles the Wasm module and the generated files from source, and compares them byte for byte, which matching fingerprints can't show. It proves this machine can reproduce what's committed. Build a committed Wasm module on Linux, macOS or WSL: on Windows the paths in its panic messages keep Windows separators, so the module differs there.

## In a fresh clone

`bimetal build` compiles only this platform's native artifacts when the committed files are current. Where you need only those, such as in a deployment image after `bimetal check`, run:

```bash
bimetal build-native
```

It stops if the source fingerprint changed since `bimetal build` generated the packages, because the generated code would call functions the artifacts lack.

## Line endings

`check` compares files byte for byte, and Bimetal writes them with LF line endings. So `.gitattributes` has to keep Git from converting them, which Git for Windows does by default. `bimetal new` writes these lines, and an application it didn't create needs them too before anyone clones it on Windows:

```text
* text=auto eol=lf
*.wasm binary
```

Without them, `check` reports every generated text file as changed there, and `build-native` refuses the packages as stale.

## Merge conflicts

Two branches that change a Rust source, an `include_str!` file or a manifest both change `source_fingerprint`, so merging them conflicts in `[generated]`. Changes to tests, benches, examples and binaries don't.

Keep either side of each conflict in `[generated]`, or delete those tables, and run `bimetal build`. It adopts the generated files it finds, and writes `[generated]` again. Resolve conflicts in generated files the same way, since the build replaces them.

## When a build should run and doesn't

Bimetal sees what a build reads through Cargo, so it can't see what a build script or a procedural macro reads without telling Cargo, with `rerun-if-changed` or `rerun-if-env-changed`. Run `bimetal build --force` after such a change.

A build that compiles only native artifacts, as in a fresh clone, keeps what the last full build recorded for the builds it skips. So a change to what only discovery or the Wasm build reads, such as an environment variable under `cfg(target_arch = "wasm32")`, goes unnoticed until a build compiles everything, as `bimetal build --force` does.
