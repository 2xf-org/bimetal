# Node.js

The Wasm module runs in Node.js with no extra work. Choose the native addon when your Rust doesn't compile to Wasm, as with code on Tokio's multi-threaded runtime, or when you need threads:

```toml
[typescript]
backend = "node"
```

The package then loads `native/native.node`, which `bimetal build` installs for this platform. It runs only in Node.js and Electron with Node-API 8 or later. The calls, values and errors are the same as with the Wasm module. Only panics and stack overflows behave differently, as [Failures](../concepts/failures.md) describes. `nativeReady` has resolved by the first call.

[`apps/examples/trading`](../../apps/examples/trading) uses this backend, under load: a thousand orders at once, worker threads, and closing the engine while calls run.

## When it won't load

A missing addon, or one built for another platform or from other Rust sources, makes the import throw an `Error` that says to run `bimetal build-native` or `bimetal build`.

## Electron

Electron can't load an addon from inside an asar archive, so unpack `.node` files when you package the app. electron-builder's `asarUnpack: ["**/*.node"]` does it, and so does Electron Forge's auto-unpack-natives plugin.

## Depending on a local package

To depend on the package from another local project, install a tarball, or set npm's `install-links=true`:

```bash
npm pack ./my-app/typescript
```

```bash
npm install ./my-app-0.1.0.tgz
```

A plain `file:` dependency is a symlink, through which Node.js resolves the package's own imports, such as `apache-arrow`, from `typescript/` instead of your project. With Arrow, that loads a second copy of apache-arrow, whose values the package rejects. This applies to both backends.

## TypeScript

TypeScript users need TypeScript 5.2 or newer, for `using` and `Symbol.dispose`.
