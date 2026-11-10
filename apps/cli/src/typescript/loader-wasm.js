import initializeNative, * as native from "./native/native.js";

// The loader. Node.js instantiates the module synchronously while the package
// is imported; other hosts fetch it. Until the module is ready, and after it
// fails to load or a trap aborts it, `nativeUnusable` says why, and
// `checkNative` throws that before any call enters the module.
//
// Every call into the module goes through `guardExports`, which the bindings'
// loader applies to the module's exports, and which aborts the module at the
// first trap.

const wasmUrl = new URL("./native/native_bg.wasm", import.meta.url);
const process = globalThis.process;
let nativeReady;
// Makes the `Error` that says why no call may enter the module: it is still
// loading, it failed to load, or a trap aborted it. Undefined while calls may
// enter it.
let nativeUnusable;
// The `Error` of the trap that aborted the module, which the call that
// trapped throws and every pending async call rejects with.
let nativeAborted;
// The message of the Rust panic that is about to trap.
let panicMessage;
// The reject functions of the async calls that have not settled.
const pendingCalls = new Set();
// The calls into the module that are running, with the calls back into it
// that it makes through JavaScript.
let nativeDepth = 0;
// The exports that the host calls itself, rather than a wrapper: the closures
// that the module hands to JavaScript, such as its async executor's
// microtask, and the finalizers that free collected values.
const hostCallback = /^(wasm_bindgen__convert__closures_|__wbindgen_destroy_closure$|__wbg_\w+_free$)/;

// Instantiating the module already calls it, so the guard comes first.
native.__bimetal_guard_exports(guardExports);
if (process?.versions?.node && process.getBuiltinModule) {
  let wasmPath;
  if (wasmUrl.protocol === "file:") {
    wasmPath = process.getBuiltinModule("node:url").fileURLToPath(wasmUrl);
  } else if (wasmUrl.pathname.startsWith("/@fs/")) {
    // Vite, as vitest runs it, serves a file outside the project root at
    // `/@fs/<absolute path>`.
    const vitePath = decodeURIComponent(wasmUrl.pathname.slice("/@fs".length));
    wasmPath = process.platform === "win32" && /^\/[A-Za-z]:\//.test(vitePath)
      ? vitePath.slice(1)
      : vitePath;
  } else {
    throw new Error(`Unsupported Node Wasm URL: ${wasmUrl.href}`);
  }
  try {
    native.initSync({ module: process.getBuiltinModule("node:fs").readFileSync(wasmPath) });
  } catch (error) {
    throw loadError(wasmPath, error);
  }
  nativeLoaded();
  nativeReady = Promise.resolve();
} else {
  nativeUnusable = () => new Error(
    "The WebAssembly module is not ready. Browsers load it asynchronously: `await nativeReady`, which the package exports, once before the first call.",
  );
  nativeReady = initializeNative({ module_or_path: wasmUrl }).then(nativeLoaded, (error) => {
    nativeUnusable = () => loadError(wasmUrl.href, error);
    throw error;
  });
}

// Whether calls may enter the module.
export function nativeUsable() {
  return nativeUnusable === undefined;
}

function loadError(location, error) {
  return new Error(`The WebAssembly module failed to load from ${location}: ${error?.message ?? error}`, { cause: error });
}

function nativeLoaded() {
  // The panic hook calls this just before the panic traps the module.
  native.bimetal_on_panic((message) => {
    panicMessage = message;
  });
  nativeUnusable = undefined;
}

// Rust returns its errors, so an exception leaves the module only when a
// trap, or a JavaScript error such as a `RangeError` for a stack overflow,
// cuts a call short. That leaves the module's memory, locks, and async
// executor as they were partway through, so the guard aborts the module.
function guardExports(exports) {
  return Object.fromEntries(Object.entries(exports).map(([name, value]) => [
    name,
    typeof value === "function" ? guardExport(value, hostCallback.test(name)) : value,
  ]));
}

function guardExport(enter, isHostCallback) {
  return (...args) => {
    // Wrappers check the module before they call it, so only the host's
    // callbacks get here after an abort. They find nothing to do, and the
    // bindings read 0 as no result and no error.
    if (nativeAborted !== undefined) return 0;
    nativeDepth += 1;
    try {
      const result = enter(...args);
      // A trap inside a callback reaches Rust as the callback's error, so the
      // outer call can return normally from a module that is no longer usable.
      if (nativeAborted !== undefined && nativeDepth === 1) throw nativeAborted;
      return result;
    } catch (error) {
      abortNative(error);
      // No caller of a host callback can catch the trap: the host reports it
      // as uncaught, which ends a Node.js process. The pending calls have
      // their rejections already.
      if (isHostCallback && nativeDepth === 1) return 0;
      throw nativeAborted;
    } finally {
      nativeDepth -= 1;
    }
  };
}

// Abort the module at its first trap: keep every later call out, and reject
// every pending async call, whose future can no longer finish.
function abortNative(cause) {
  if (nativeAborted !== undefined) return;
  const panicked = panicMessage !== undefined;
  const detail = panicked ? panicMessage : String(cause);
  nativeAborted = new Error(panicked ? `Rust panicked: ${detail}` : `The WebAssembly module trapped: ${detail}`, { cause });
  nativeUnusable = () => new Error(
    `An earlier ${panicked ? "Rust panic" : "WebAssembly trap"} aborted the WebAssembly module (${detail}); reload the page or restart the process to call it again.`,
  );
  for (const reject of pendingCalls) reject(nativeAborted);
  pendingCalls.clear();
}

// Every wrapper calls this before it enters the module.
export function checkNative() {
  if (nativeUnusable !== undefined) throw nativeUnusable();
}

// Every async wrapper awaits this, which settles as the module's `promise`
// does, or rejects when a trap aborts the module first.
export function nativePromise(promise) {
  return new Promise((resolve, reject) => {
    pendingCalls.add(reject);
    promise.then(resolve, reject).finally(() => pendingCalls.delete(reject));
  });
}

export { native, nativeReady };
