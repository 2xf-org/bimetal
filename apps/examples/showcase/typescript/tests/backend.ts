import { existsSync } from "node:fs";

/** Whether the package runs its Rust code as a Node.js addon, not as Wasm. */
export const addon = existsSync(new URL("../showcase/native/native.node", import.meta.url));
