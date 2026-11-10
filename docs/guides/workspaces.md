# Workspaces

`bimetal new` makes a Cargo package that is its own workspace. To add it to an existing one, remove its `[workspace]` table and list the directory in that workspace's `members`. `bimetal.toml` stays beside the package.

## Keeping projects somewhere else

Set `project` to put a host's project anywhere outside the Cargo package, and move its manifest there:

```toml
[python]
project = "../python"
```

[`apps/examples/trading`](../../apps/examples/trading) is laid out this way, as a larger application often is. Its workspace has the models in `crates/core` and the engine in `crates/engine`, and the projects sit at the top, beside `crates/`:

```text
trading/
├── Cargo.toml
├── crates/
│   ├── core/
│   └── engine/
│       ├── bimetal.toml
│       └── src/
├── python/
└── node/
```

```toml
[application]
additional_packages = ["trading-core"]

[python]
project = "../../python"

[typescript]
project = "../../node"
backend = "node"
```

`bimetal build` finds that `bimetal.toml` from `crates/engine` and the folders below it, the way Cargo finds `Cargo.toml`. From the top of the workspace, pass `--config`:

```bash
bimetal build --config crates/engine/bimetal.toml
```

A project can't be the folder of `bimetal.toml` or one of its parents, can't be inside Cargo's target directory, and can't be another host's project or inside it.

## Linking more packages

`additional_packages` links other packages of the same workspace into one application. Each must be a library, and must depend directly on `bimetal`.

Each package's name, without the `-`-separated prefix that all the packages share, comes first in its namespaces. With `app` and `app-hardware`, the `annotation` module of `app-hardware` becomes `app.hardware.annotation` in Python and `app/hardware/annotation` in TypeScript. Nothing is left of `app`, so the application package's own declarations are at the root.

[`apps/examples/showcase/geometry`](../../apps/examples/showcase/geometry/src/lib.rs) is linked this way, as the `geometry` namespace. Another package's models and errors work in your signatures like your own:

```rust
/// The length of a route. Uses an error declared in the `geometry` namespace.
#[bimetal::export]
pub fn route_length(route: Vec<Point>) -> Result<f64, GeometryError> {
```

A workspace that groups its packages in folders can name the namespaces after the folders instead:

```toml
[application]
name = "xf"
additional_packages = ["xf-core", "xf-trading", "xf-polymarket", "xf-sports"]
namespaces = "folders"
```

Each package is then named after its folder below the base folder, the deepest folder that holds every linked package, the application package's included, one namespace per folder. The application package's declarations are at the root. Here the application is `xf-bindings`, in `packages/bindings`:

```text
packages/
├── bindings/
├── core/
├── trading/
└── sources/
    ├── data/
    │   └── sports/
    └── venues/
        └── polymarket/
```

`packages/` holds them all, so the namespaces are `core`, `trading`, `sources.venues.polymarket` and `sources.data.sports`. The `markets::fees` module of `xf-polymarket` becomes `xf.sources.venues.polymarket.markets.fees` in Python and `xf/sources/venues/polymarket/markets/fees` in TypeScript. `sources`, `sources.venues` and `sources.data` are namespaces too, which hold only the namespaces below them.

Switching the setting can move the application package's own exports. With the default, `"packages"`, its function `greet` is `xf.bindings.greet`, because nothing but `xf` is shared by every name. With `"folders"`, it is `xf.greet`.

The base folder depends on every linked package. Linking a package outside `packages/`, or moving `bindings` out of it, moves the base folder up and renames every namespace.

Every package that declares exports has to be linked, as the application package or in `additional_packages`, which takes only members of the workspace, and has to be inside the workspace's folder. A git or registry dependency that declares exports needs `"packages"`.

A `-` in a folder's name becomes `_` in Python, as in a package's. Bimetal stops when a folder's name can't be a Python module and the Python package is on, when a linked package's folder holds all the others, which would give it the root, and when a module and a folder become the same module of either host.

## Moving things later

To move a package, change its `project` and move your own files, such as `pyproject.toml`, there. The next build deletes the generated files from the old project and writes them in the new one.

To rename a package, change `package` in its table, or `name`, and run `bimetal build`. The old package directory goes once nothing but Bimetal's files is left in it.

To stop building a host's package, remove its table. The next build deletes the files Bimetal generated for it.
