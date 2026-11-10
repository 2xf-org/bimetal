# Reproducible output

Two checkouts of the same commit, at any path, on Linux or macOS, with the same toolchain, produce the same Wasm and generated sources. That's what lets `bimetal check --rebuild` compare them, byte for byte. These rules keep it true, so read them before you change how a build runs.

## Paths

Bimetal remaps every local path that can reach the output (`cargo/remap.rs`), in every compilation for a build's target:

| Local path | Becomes |
| --- | --- |
| The workspace's files | Paths relative to it, such as `src/lib.rs`, which panic messages then show |
| The Cargo home | `/cargo` |
| The target directory | `/target` |
| The link crates | `/bimetal-link`, so a link crate's own `src/` names no file of the workspace |
| Local packages outside the workspace | `/source/<name>` |
| The standard library's sources | `/rustc/<commit>` |

It also strips the Wasm name and producers sections. Diagnostics map the remapped paths back.

## Crate identity

rustc derives each crate's identity from its `-C metadata`, which names symbols, orders the functions of the Wasm module, and enters every `TypeId`. Cargo hashes into it the absolute path of each local package outside the building workspace, which the link crate is, and, through the build scripts and procedural macros a crate depends on, the host's target triple.

So Bimetal's rustc wrapper (`cargo/rustc_wrapper.rs`) replaces it, in the same compilations, with a hash of the package's directory as the remapped paths write it, the crate's name and types, and its `--cfg` flags. Cargo doesn't see the change, so the Wasm builds take a new target directory whenever the wrapper compiles differently.

## Leave the user's flags to Cargo

Bimetal's own flags, the path remaps, reach rustc through `bimetal` itself, which every build sets as `RUSTC_WRAPPER`. It adds them to each compilation that names the build's `--target`, so no build script or proc macro gets them, and then runs the user's own wrapper, if any.

Cargo fingerprints neither, which is safe only because every remap names a directory by a location-free name. A new flag must not change what rustc writes for a crate Cargo already compiled. Flags only the link crate needs go after `--` in `cargo rustc`, which applies them to that crate alone. Profile settings for one target, like the Wasm size settings, go in `--config profile.release.*`, which changes only that build's cache.

## Which inputs go where

Cargo's dep-info file beside each build's shared library or Wasm module lists the files the build read (`cargo::dep_info`), and the dep-info files rustc writes for each local library, with the build scripts' output, name the environment variables.

| Input | Goes in |
| --- | --- |
| The files of the checkout that the Wasm build read | `[generated].inputs`, and so the source fingerprint |
| The manifests, `Cargo.lock`, `.cargo/config.toml` files that git tracks, and the toolchain without its host | The source fingerprint |
| The generator | The source fingerprint, through `BIMETAL_GENERATOR_FINGERPRINT` |
| What only a native build reads | `.bimetal/native.json` |
| Files git ignores, files outside the checkout, and Cargo's home | `.bimetal/native.json` |
| Environment variables | `.bimetal/native.json` |

`project::checkout` makes the split. Only what every checkout of a commit has on every host enters the source fingerprint, so a build on another host never changes `bimetal.toml`. The Wasm build reads the same files on every host, so `inputs` comes out the same on each. An application without a Wasm module records what discovery read instead, which holds this rule only while no code for one operating system alone reads a file.

## Windows

On Windows, rustc keeps the Windows separators in the rest of a remapped path, as in `/cargo\registry\src\...`, and those paths reach the Wasm module through panic locations. So a build on Windows changes the module and `output_fingerprint`, though not `inputs`, and committed modules should come from Linux, macOS or WSL.

## macOS

Every native library links with `-C strip=none -C link-arg=-Wl,-S`, and names itself `@rpath/<file>` after the file Bimetal installs. Apple's `strip`, which rustc otherwise runs, can misalign the string table, and then dyld refuses to load the library. Only the Python extension adds `-undefined dynamic_lookup`, for the libpython symbols the interpreter provides.

## How it's checked

`mise run check-examples` runs `bimetal check --rebuild` on each example, and then `git diff --exit-code`. The committed examples were built on another machine, so on CI this shows that each runner rebuilds them, the Wasm module among them, byte for byte. [Testing](testing.md#ci) has the matrix.
