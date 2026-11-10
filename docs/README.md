# Bimetal documentation

Read these in order if you're new. Each page assumes the ones before it and defines its own terms.

1. [Tutorial](tutorial.md) writes a small Rust library, then calls it from Python and TypeScript. It takes about ten minutes once the tools are installed.
2. Concepts explain how Bimetal thinks, without code you need to memorize:
   - [The contract](concepts/contract.md): what Bimetal reads from your Rust
   - [Packages and projects](concepts/packages.md): which files are yours, and which are Bimetal's
   - [Hosts](concepts/hosts.md): the Python extension, the Wasm module and the Node.js addon
   - [How values cross](concepts/crossing.md): what each value costs at the boundary
   - [Resources](concepts/resources.md): Rust values that outlive one call
   - [Async calls](concepts/async.md): which runtime runs them, and when they start and stop
   - [Failures](concepts/failures.md): errors, bad arguments, panics and traps

## Guides

Look these up when you need them.

| To work with | Read |
| --- | --- |
| Functions, constants, namespaces and default arguments | [Functions](guides/functions.md) |
| Structs, enums and field rules | [Models](guides/models.md) |
| Errors that callers branch on | [Errors](guides/errors.md) |
| Classes that keep a Rust value | [Resources](guides/resources.md) |
| `with`, `for`, properties and `len()` | [Protocols](guides/protocols.md) |
| `async fn` and Tokio | [Async](guides/async.md) |
| Host functions that Rust calls | [Callbacks](guides/callbacks.md) |
| Byte strings and numeric arrays | [Bytes and buffers](guides/bytes.md) |
| Arrow arrays, batches, tables and streams | [Arrow](guides/arrow.md) |
| Web pages and bundlers | [Browsers](guides/browser.md) |
| The native addon and Electron | [Node.js](guides/node.md) |
| Several crates, or projects outside the crate | [Workspaces](guides/workspaces.md) |
| CI, merges and fresh clones | [Keeping packages current](guides/current.md) |
| PyPI and npm | [Publishing](guides/publishing.md) |
| A large crate you didn't write | [Wrapping a large project](guides/large-projects.md) |

## The white paper

[The white paper](paper/bimetal.pdf) describes what Bimetal is and why we wrote it: how one contract reaches three hosts, what each boundary promises, how the output stays reproducible, what that costs, and where its limits are. Read it if you want to judge the design rather than use it.

## Reference

- [Commands](reference/cli.md): every command, flag and JSON report
- [`bimetal.toml`](reference/config.md): every setting
- [Attributes](reference/attributes.md): every `#[bimetal(...)]`
- [Types](reference/types.md): every Rust type, and what each host sees
- [Names](reference/names.md): how names change per host, and which are refused
- [Generated files](reference/generated.md): what Bimetal writes, owns and deletes
- [Limits and requirements](reference/limits.md): versions, limits and what Bimetal doesn't do yet
- [Prior work](reference/prior-work.md): the work Bimetal builds on

## Internals

For anyone changing the code. They walk through one operation at a time.

- [The crates](internals/crates.md): what each crate and module does
- [The life of a build](internals/build.md)
- [The life of a call](internals/call.md)
- [Resources and callbacks](internals/resources.md)
- [Reproducible output](internals/reproducible.md)
- [Testing](internals/testing.md): the commands, the test layers, and which test guards which promise
- [Releases](internals/releases.md)
