# `bimetal.toml`

`bimetal.toml` sits beside the Cargo package. Every table above `[generated]` is yours, and Bimetal keeps their comments and formatting. It writes `[generated]` after them.

```toml
[application]
name = "bank"
additional_packages = []
namespaces = "packages"

[python]
package = "bank"
project = "python"

[typescript]
project = "typescript"
backend = "wasm"

[generated]
source_fingerprint = "…"
output_fingerprint = "…"
inputs = ["src/ledger.rs", "src/lib.rs"]

[generated.python]
project = "python"
files = ["bank/api.py", "…"]
dependencies = ["pydantic"]

[generated.typescript]
project = "typescript"
files = ["bank/api.js", "bank/api.d.ts", "…"]
dependencies = []
```

## `[application]`

Every setting is optional.

| Setting | Default | Meaning |
| --- | --- | --- |
| `name` | The Cargo package name | The application name. It also names the npm package's directory, `<name>` in the npm project |
| `additional_packages` | `[]` | Other packages of the same Cargo workspace to link into the application. Each must depend directly on `bimetal` |
| `namespaces` | `"packages"` | What names the namespaces of the linked packages: `"packages"` their Cargo package names, and `"folders"` their folders, as [Workspaces](../guides/workspaces.md#linking-more-packages) shows |

With `namespaces = "packages"`, a package's namespaces start with its name, without the `-`-separated prefix that all the packages share. The application package's declarations are at the root only when its name is that prefix, as `app` is beside `app-hardware`. Beside `xf-core`, those of `xf-bindings` are in `bindings`.

With `namespaces = "folders"`, a package's namespaces start with its folder below the base folder, the deepest folder that holds every linked package, the application package's included. The application package's declarations are at the root wherever its folder is. Every package that declares exports has to be linked, as the application package or in `additional_packages`, and inside the workspace's folder, so a git or registry dependency that declares exports needs `"packages"`. Linking a package outside the base folder, or moving the application package out of it, moves the base folder and renames every namespace.

## Host tables

Each host table turns its host on, even when it's empty, and Bimetal needs at least one. Every setting is optional.

| Table | Setting | Default | Meaning |
| --- | --- | --- | --- |
| `[python]` | `package` | `name`, with `-` replaced by `_` | The Python import package, such as `bank` or `acme.tools` |
| | `project` | `"python"` | The Python project, which holds `pyproject.toml` |
| `[typescript]` | `project` | `"typescript"` | The npm project, which holds `package.json`, whose `name` is the npm package name |
| | `backend` | `"wasm"` | `"wasm"` runs the Rust code as a WebAssembly module, and `"node"` as a native Node.js addon, which only Node.js and Electron load |

A `project` is a folder relative to `bimetal.toml`, and can climb out of it with `..`, as in `project = "../python"`. Bimetal stops when a project is the directory of `bimetal.toml` or one of its parents, lies inside Cargo's target directory, or is the same folder as another host's project, or inside it.

## `[generated]`

Bimetal writes these tables. Don't edit them, and commit them with the generated files.

| Key | What it holds |
| --- | --- |
| `source_fingerprint` | Covers what every checkout of the commit has that can change the build: the manifests and `Cargo.lock`, the Rust toolchain, the `.cargo/config.toml` files that git tracks, the files in `inputs`, the generator inside the `bimetal` command, the tables above `[generated]`, and with `namespaces = "folders"` the folders that name the namespaces, which a symlink's name can change |
| `output_fingerprint` | Covers the generated files |
| `inputs` | The files of the checkout that the WebAssembly build reads, relative to the workspace: the library's Rust sources, the files that `include_str!` and the like embed, and the paths that build scripts watch |
| `[generated.python]`, `[generated.typescript]` | Each enabled host's `project`, relative to `bimetal.toml`; the `files` that Bimetal owns there, relative to the project; and the `dependencies`, the packages that the generated code imports, which `pyproject.toml` must depend on and `package.json` must list in `peerDependencies` |

An application without a WebAssembly module lists in `inputs` what the contract discovery build reads, which compiles for this platform. A file that only code for one operating system reads, such as an `include_str!` under `#[cfg(target_os = "linux")]`, then makes `check` fail on the others.

No record lists a native artifact: the Python extension is the one beside the `native.pyi` stub, with the stub's name, and the Node.js addon the one beside `runtime.js`.

What else a build reads belongs to this machine. Bimetal records it in `.bimetal/`, and `build` compiles everything again when it changes. `check` leaves it out, so it passes on every host. [Reproducible output](../internals/reproducible.md) lists which inputs belong to the checkout and which to this machine.
