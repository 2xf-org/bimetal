# Packages and projects

A Bimetal application is one Cargo package with a `bimetal.toml` beside it, and a *project* for each host that `bimetal.toml` turns on. A project is an ordinary Python or npm project: it holds your manifest, your tests and your lockfile. A *package* is what Bimetal generates inside the project.

```text
my-app/
├── bimetal.toml                  Yours, and Bimetal adds a [generated] table
├── python/
│   ├── pyproject.toml          Yours
│   ├── pdm_build.py            Yours: tags the wheel for the stable ABI and this platform
│   ├── tests/                  Yours
│   └── my_app/                 Generated: __init__.py, api.py, models.py, py.typed, and private _runtime.py and _native
└── typescript/
    ├── package.json            Yours
    └── my-app/                 Generated: index, api, models, and private runtime as .js and .d.ts, and native/
```

By default the projects are `python/` and `typescript/`, beside `src/`. Set `project` in `bimetal.toml` to keep them anywhere else, as [Workspaces](../guides/workspaces.md) shows.

## Whose files are whose

Bimetal writes only inside the package folder, and records each file it writes. Everything else is yours, and it never changes those files. If a generated file would overwrite one of yours, the build stops before it changes anything. [Generated files](../reference/generated.md) has the exact rules.

The public API is the package root and its namespaces. In Python, the underscore marks `_runtime` and `_native` as private. In TypeScript, `package.json` exports only the root and the namespaces, so `runtime` and `native/` stay internal.

## What you commit

| File | Commit it? | Why |
| --- | --- | --- |
| `bimetal.toml` and `Cargo.lock` | Yes | They pin what every machine builds |
| Generated `.py`, `.js` and `.d.ts` files | Yes | API changes show up in review, and CI can compare them |
| The Wasm module | Yes | One file serves every platform, so a clone needs no Rust to use it |
| The Python extension and the Node.js addon | No | Each platform builds its own |

Committing generated code is a choice with a cost: more files, and larger diffs. It pays for itself in `bimetal check`, which compares what the Rust would generate with what's committed, byte for byte. A fresh clone then only compiles the native artifacts.

You can leave generated packages out of git, as [`apps/trials/`](../../apps/trials/.gitignore) does for its large Wasm modules. `bimetal build` then has to run before anything imports, and `bimetal check` fails on a fresh checkout, since the files it compares are missing.

## How Bimetal knows nothing changed

`bimetal.toml` records two fingerprints. The source fingerprint covers what every checkout of a commit has that can change the build: the manifests, `Cargo.lock`, the Rust toolchain, the Rust sources, the generator itself and the settings above `[generated]`. The output fingerprint covers the generated files. When both match, a build compiles nothing.

What only this machine read, such as environment variables and files git ignores, stays out of `bimetal.toml` and in `.bimetal/`, so a build on another machine doesn't change a committed file. [Reproducible output](../internals/reproducible.md) explains that split.

## What a package checks at load

The generated code and the compiled code have to agree, and they can drift apart: a local extension older than a `git pull`, for example. So the extension and the addon carry a hash of their contract, and the package compares it at import. A mismatch raises an error that says to run `bimetal build-native` or `bimetal build`.
