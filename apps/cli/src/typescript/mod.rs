//! JavaScript, type declaration, and wasm-bindgen artifact emission.
//!
//! Each generated module is a JavaScript file with a `.d.ts` file beside it,
//! so the package runs without a TypeScript build. The Wasm module, or the
//! Node.js addon that `bimetal build` installs, sits next to the JavaScript
//! that loads it.
//!
//! - `package` writes the package's files, each pair through `module`, among
//!   them `runtime.js` and `runtime.d.ts`, the private runtime, which starts
//!   with the loader of its backend (`loader-wasm.js` or `loader-node.js`)
//!   and declares what that loader exports (`loader-wasm.d.ts` or
//!   `loader-node.d.ts`), and `arrow.js`, its Arrow adapters;
//! - `models` and `api` render each namespace's `models` and `api` modules;
//! - `render` holds the spelling decisions that they share, and `context`
//!   the state they render with, which records what each module imports.

mod api;
mod context;
mod models;
mod module;
mod package;
mod render;

pub(crate) use api::RESOURCE_MEMBERS;
pub(crate) use context::{API_ALIAS_PREFIX, ARROW_NAMESPACE, MODELS_ALIAS_PREFIX};
pub(crate) use package::{
    NATIVE_DIRECTORY, NATIVE_READY, NODE_ADDON, Native, RUNTIME_MODULE, WASM_MODULE, emit,
};
pub(crate) use render::{JSON_VALUE, RECORD_BATCH_SOURCE, RECORD_BATCH_STREAM, parameter_name};
