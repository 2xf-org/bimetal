# Browsers

The default TypeScript backend runs your Rust as a WebAssembly module, so one npm package works in Node.js, in browsers and in bundlers. Node.js loads the module while the package is imported. Browsers load it asynchronously, so wait for it once before the first call:

```ts
await nativeReady;
```

If the module fails to load, `nativeReady` rejects, and every call throws an `Error` that names the module's URL and the failure. In Node.js, a missing or corrupt `native/native_bg.wasm` makes the import itself throw that `Error`, with the file's path.

## Bundlers

The loader finds the module with `new URL("./native/native_bg.wasm", import.meta.url)`. Vite, webpack 5 and other bundlers that support this form copy the module into the build output. [`vite.test.ts`](../../apps/examples/showcase/typescript/tests/vite.test.ts) builds a page with Vite and runs it.

## What Bimetal handles for you

Crates that need random numbers or the time compile and run in the module without configuration. The build sets the cfg that `getrandom` needs on `wasm32-unknown-unknown`, and the runtime turns on its browser backend in versions 0.2, 0.3 and 0.4, and chrono's `wasmbind`.

A build also stops, and names the symbols, when the module imports something nothing defines, which is what a C library compiled for the wrong target leaves behind. The loader would otherwise fail in the browser with a message about a module named `env`.

## What you handle yourself

- **A dependency's own switch.** Crates with their own Wasm feature still need it, such as `uuid`'s `js`, under `[target.'cfg(target_arch = "wasm32")'.dependencies]`.
- **`std::time`.** It panics in the module, so a dependency that calls it needs a stand-in such as `web-time`.
- **Threads.** The module has one. Code that spawns threads needs another path for this target, as [Tantivy's trial](../../apps/trials/tantivy/src/lib.rs) has.
- **Tokio.** [Async](async.md#tokio) has the options.
- **C libraries.** They don't compile for this target without work. [Wrapping a large project](large-projects.md) shows what the trials did.

`#[cfg]` on an exported method is a compile error, which says to put it inside the method's body. An export has to exist on every target, or the contract and the module would disagree.

## After a panic

A panic ends the module until the page reloads, as [Failures](../concepts/failures.md#panics) describes. Design around this if your page should survive a bug: put work that can panic behind a worker you can restart.

## Size and debugging

The module is optimized for size, with `opt-level = "s"`, link-time optimization and one codegen unit, and has no debug name sections. To debug it, build it locally. The [trials](../../apps/trials/README.md) range from 3 MB to 33 MB, before any compression your server applies.

JavaScript can't keep a view into Wasm memory, so values copy across, and Arrow costs more than in Python. [How values cross](../concepts/crossing.md) has the counts.
