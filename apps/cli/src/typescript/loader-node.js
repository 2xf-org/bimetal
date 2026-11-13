import { createRequire } from "node:module";

// The loader. Node.js loads the addon, the native library that `bimetal build`
// installs for this platform, while the package is imported.

const napiVersion = Number(globalThis.process?.versions?.napi);
if (!(napiVersion >= 8)) {
  throw new Error(
    `This package runs its Rust code as a Node.js addon, which needs Node-API 8 or later; this runtime has ${napiVersion ? `Node-API ${napiVersion}` : "no Node-API"}.`,
  );
}

let native;
try {
  native = createRequire(import.meta.url)("./native/native.node");
} catch (error) {
  throw new Error(
    `The Node.js addon native/native.node is missing, was built for another platform, or failed to load; run \`bimetal build\` to build it: ${error?.message ?? error}`,
    { cause: error },
  );
}
const nativeReady = Promise.resolve();

// An addon built from other Rust sources, such as one built before a
// `git pull`, would fail at the first call that it lacks.
function requireContract(contract) {
  if (native.__bimetal_contract__ !== contract) {
    throw new Error(
      "The Node.js addon native/native.node was built from other Rust sources than this package; run `bimetal build` to rebuild it.",
    );
  }
}

// Every wrapper calls this before it enters the addon. Rust throws its own
// failures and panics, so every call may enter it.
export function checkNative() {}

// A panic leaves the addon usable.
export function nativeUsable() {
  return true;
}

// Every async wrapper awaits this, which settles as the addon's promise does.
export function nativePromise(promise) {
  return promise;
}

export { native, nativeReady };
