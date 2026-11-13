# Commands

| Command | Use |
| --- | --- |
| `bimetal new <directory>` | Create an application |
| `bimetal build` | Compile the application and write the package of each host that `bimetal.toml` turns on |
| `bimetal check` | Make sure the committed generated files match the Rust sources |

## What every command does

`build` and `check` read the `bimetal.toml` of the current directory, or of the nearest parent directory that has one, the way Cargo finds `Cargo.toml`. Pass `--config <path>` to use another one: the file, or its directory.

Each command prints its result as text. Add `--json` to print it as one JSON object for scripts. While it compiles, a command prints progress to stderr, and says when it waits for another `bimetal` command building the same application, or for another Cargo command, such as rust-analyzer or `cargo test`, that holds the lock of a build directory.

On failure, a command prints `error: ...` to stderr and exits with status 1. With `--json`, it also prints `{"status": "error", "error": "..."}` to stdout. Invalid arguments exit with status 2 and print usage.

## How it runs Cargo

Cargo runs in the directory of `bimetal.toml`, so the `.cargo/config.toml` files there and in its parents apply wherever you start `bimetal`. Your `rustflags`, `RUSTFLAGS` and `RUSTC_WRAPPER`, such as sccache, apply as they do for `cargo build`. `--locked`, `--offline` and `--frozen` pass on to Cargo as they are, and `CARGO_NET_OFFLINE` works as it does for Cargo.

A configured `build.target` or `CARGO_BUILD_TARGET` changes none of the builds, which name their targets. `build` may update `Cargo.lock`, as `cargo build` does. `check` never does, as if you passed `--locked`, and says so when `Cargo.lock` is missing or out of date.

A command shows the compiler's warnings about your code once each, though each build compiles it, in colour when stderr is a terminal. Warnings and errors show your files' real paths.

## `bimetal new`

```text
bimetal new <directory> [--bimetal-path <checkout> | --bimetal-git <url> [--tag <tag> | --branch <branch> | --rev <commit>]]
```

Creates the directory with the files of a new application. The directory must not exist, or must be empty, like `.` in a new directory, and Bimetal creates it and its missing parents. All of these files are yours:

| File | Contents |
| --- | --- |
| `Cargo.toml` | The Rust package, with `bimetal` and `serde` as dependencies. It is its own Cargo workspace |
| `src/lib.rs` | A sample contract: one model and one function |
| `bimetal.toml` | A `[python]` and a `[typescript]` table, which turn on both packages |
| `python/pyproject.toml` | The Python project |
| `python/pdm_build.py` | The build hook that tags wheels for the stable ABI and the extension's platform |
| `typescript/package.json` | The npm package |
| `.gitignore` | Build output and the platform-specific native artifacts |
| `.gitattributes` | Keeps Git from converting the line endings of generated files |

The directory's name becomes the application name: the Cargo package and npm package `my-app`, and the Python package `my_app`. It uses lower-case letters, digits and single hyphens. Its Python package can't be a Python keyword, the name of a standard-library module such as `test` or `email`, or the name of a package the generated code imports, such as `pydantic` or `numpy`, which it would hide.

The application depends on the `bimetal` crate of the command's own version on crates.io. `--bimetal-path` writes a path dependency on a checkout of Bimetal instead, relative, with `/` between its parts on every platform, when the application and the checkout share a parent directory. `--bimetal-git` writes a git dependency.

With `--json`:

```json
{
  "status": "ok",
  "application": "my-app",
  "directory": "/path/to/my-app",
  "files": ["Cargo.toml", "src/lib.rs", "bimetal.toml", "…"]
}
```

## `bimetal build`

```text
bimetal build [--force]
```

```text
Compiling greeter: contract discovery, Python extension, Node.js addon
  Compiled the contract discovery in …s
  Compiled the Python extension in …s
  Compiled the Node.js addon in …s
Built greeter 0.1.0.
  Python package greeter in python
  npm package greeter in typescript
```

A build compiles what the enabled hosts need: contract discovery always, the WebAssembly module for a `[typescript]` table with the Wasm backend, the Python extension for `[python]`, and the addon for the Node.js backend. It installs the native artifacts after it writes the generated files.

If the sources and the generated files both match the fingerprints in `bimetal.toml`, and nothing that only this machine's last build read changed, the command compiles nothing and prints `greeter 0.1.0 is up to date.` In a fresh clone, where only this platform's native artifacts are missing, it compiles only those. `--force` compiles everything and writes the packages again.

With `--json`:

```json
{
  "status": "ok",
  "application": "greeter",
  "version": "0.1.0",
  "hosts": [
    {
      "host": "python",
      "package": "greeter",
      "directory": "/path/to/greeter/python",
      "files": ["greeter/__init__.py", "greeter/_native.pyi", "…", "greeter/_native.abi3.so"]
    },
    {
      "host": "typescript",
      "package": "greeter",
      "directory": "/path/to/greeter/typescript",
      "files": ["greeter/api.d.ts", "greeter/api.js", "…", "greeter/native/native.node"]
    }
  ]
}
```

`status` is `"unchanged"` when nothing was compiled. `hosts` lists the enabled hosts, in the order `python`, `typescript`, each with its package, the project that holds it, and the package's files relative to the project: the generated files, and then this platform's native artifacts.

### What a build checks

Before it compiles, a build stops if a package of the application isn't a Rust library: its `[lib] crate-type` may list only `lib` or `rlib`. Remove `cdylib`, because Bimetal builds the native libraries and the WebAssembly module itself.

After the WebAssembly build, it stops if the module lacks a function, resource or method of the contract, or has one the contract lacks, as when `#[cfg(target_arch = "wasm32")]`, or its negation, is on an export. It also stops if the module imports a symbol nothing defines, and names it.

Every build checks the files you own, even one that compiles nothing, and reports every problem in one error, together with any problem in the contract. It does so before it writes any file, and only for the enabled hosts:

- `Cargo.toml`, `pyproject.toml` and `package.json` declare the same version.
- `package.json` declares a `name`, the npm package name, and `"type": "module"`. Its root export points into `<name>/`, and `files`, if present, includes `<name>`, so `npm pack` contains the generated package.
- `pyproject.toml` depends on `pydantic` 2.7 or later, also on `numpy` 2 or later if the contract uses buffers, and on `pyarrow` 16 or later if it uses Arrow values, and allows no older version. With `pdm-backend`, `[tool.pdm.build]` sets `is-purelib = false`, and its `includes` cover the Python package, so wheels are platform-specific and contain it.
- `package.json` lists `apache-arrow` 21.2 or later in `peerDependencies` if the contract uses Arrow values, and allows no older version.

## `bimetal check`

```text
bimetal check [--rebuild]
```

`check` checks the files you own as `build` does, and then succeeds without compiling if the fingerprints match. Otherwise, or with `--rebuild`, it compiles contract discovery and, with the Wasm backend, the WebAssembly module, generates the packages, compares the result with the files on disk, and lists each missing, changed or stale file on its own line, relative to `bimetal.toml`:

```text
error: the generated files are not in sync with the Rust sources; run `bimetal build`
  changed: python/greeter/api.py
  stale: typescript/greeter/old/index.js
```

It never writes generated files or `Cargo.lock`. Native artifacts are no part of the check, because each platform builds its own, and `check` compiles none. [Keeping packages current](../guides/current.md) says when to use each form.

With `--json`, a passing check prints `"status": "ok"` and the same `application`, `version` and `hosts` as `build`. A check that finds files out of sync lists them beside `error`:

```json
{
  "status": "error",
  "error": "the generated files are not in sync with the Rust sources; run `bimetal build`\n  changed: python/greeter/api.py",
  "missing": [],
  "changed": ["python/greeter/api.py"],
  "stale": []
}
```
