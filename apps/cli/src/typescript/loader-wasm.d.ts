/**
 * Resolves when the Rust code is ready to call. Node.js loads it while the
 * package is imported, so this has resolved by the first call. Browsers load
 * the WebAssembly module asynchronously, so wait for this once before the
 * first call; an earlier call throws an `Error` that says so. When the module
 * fails to load, this rejects with the failure, and every call throws an
 * `Error` that names the module's URL and the failure.
 */
export declare const nativeReady: Promise<void>;
